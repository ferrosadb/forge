//! Per-sheet mapping config: `.forge/sheets/<alias>.toml`.
//!
//! Translates a spreadsheet's own header text and status vocabulary into the
//! sync engine's fixed [`crate::model::CanonicalField`] set and
//! [`forge_tasks::TaskStatus`], and captures the handoff sets that decide
//! which sheet-side statuses the dev side may advance on push (see
//! `specs/todo/feat-sheet-sync.md`, "Config" and "Sync semantics").

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use serde::Deserialize;

use crate::model::CanonicalField;

/// A fully validated per-sheet mapping, ready for use by the mapping,
/// planning, and push-edit stages.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SheetMapping {
    pub spreadsheet_id: String,
    pub tab: String,
    pub id_column: String,
    pub columns: BTreeMap<String, CanonicalField>,
    pub writable: BTreeSet<CanonicalField>,
    pub status_map: BTreeMap<String, forge_tasks::TaskStatus>,
    pub dev_writable_status: BTreeSet<String>,
    pub terminal_status: BTreeSet<String>,
}

/// Mirrors the on-disk TOML shape verbatim; `TryFrom<RawMapping>` performs
/// all field-target parsing and cross-field validation.
#[derive(Debug, Deserialize)]
struct RawMapping {
    spreadsheet_id: String,
    tab: String,
    id_column: String,
    #[serde(default)]
    columns: BTreeMap<String, String>,
    #[serde(default)]
    writable: Vec<String>,
    #[serde(default)]
    status_map: BTreeMap<String, String>,
    #[serde(default)]
    dev_writable_status: BTreeSet<String>,
    #[serde(default)]
    terminal_status: BTreeSet<String>,
}

impl TryFrom<RawMapping> for SheetMapping {
    type Error = anyhow::Error;

    fn try_from(raw: RawMapping) -> Result<Self, Self::Error> {
        if raw.id_column.trim().is_empty() {
            anyhow::bail!("sheet mapping: `id_column` must be non-empty");
        }
        if raw.columns.is_empty() {
            anyhow::bail!("sheet mapping: `columns` must be non-empty");
        }

        let mut columns = BTreeMap::new();
        for (header, target) in &raw.columns {
            let field = CanonicalField::parse(target).ok_or_else(|| {
                anyhow::anyhow!(
                    "sheet mapping: column {header:?} maps to unknown canonical field {target:?}"
                )
            })?;
            columns.insert(header.clone(), field);
        }

        match columns.get(&raw.id_column) {
            Some(CanonicalField::Id) => {}
            Some(_) => anyhow::bail!(
                "sheet mapping: `id_column` {:?} is mapped, but not to `id`",
                raw.id_column
            ),
            None => anyhow::bail!(
                "sheet mapping: `id_column` {:?} is not present as a key in `columns`",
                raw.id_column
            ),
        }

        let mut writable = BTreeSet::new();
        for target in &raw.writable {
            let field = CanonicalField::parse(target).ok_or_else(|| {
                anyhow::anyhow!(
                    "sheet mapping: `writable` names unknown canonical field {target:?}"
                )
            })?;
            if !columns.values().any(|mapped| *mapped == field) {
                anyhow::bail!(
                    "sheet mapping: `writable` names {target:?}, which is not mapped in `columns`"
                );
            }
            writable.insert(field);
        }

        let mut status_map = BTreeMap::new();
        for (sheet_status, target) in &raw.status_map {
            let status = forge_tasks::TaskStatus::parse(target).ok_or_else(|| {
                anyhow::anyhow!(
                    "sheet mapping: status_map[{sheet_status:?}] targets unknown TaskStatus {target:?}"
                )
            })?;
            status_map.insert(sheet_status.clone(), status);
        }

        Ok(SheetMapping {
            spreadsheet_id: raw.spreadsheet_id,
            tab: raw.tab,
            id_column: raw.id_column,
            columns,
            writable,
            status_map,
            dev_writable_status: raw.dev_writable_status,
            terminal_status: raw.terminal_status,
        })
    }
}

/// Walk up from `start` looking for `.forge/sheets/<alias>.toml`.
/// Returns `Some(path)` if found, `None` otherwise. Pure; testable.
fn locate_alias_path(start: &std::path::Path, alias: &str) -> Option<PathBuf> {
    let relative = PathBuf::from(".forge")
        .join("sheets")
        .join(format!("{alias}.toml"));
    for dir in start.ancestors() {
        let candidate = dir.join(&relative);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

impl SheetMapping {
    /// Parses and validates a `.forge/sheets/<alias>.toml` body.
    ///
    /// Validation (each fails loud, naming the offending value): `id_column`
    /// is non-empty and present as a key in `columns` mapped to
    /// [`CanonicalField::Id`]; every `writable` entry names a field that
    /// appears as a value in `columns`; `columns` is non-empty; every
    /// `status_map` value parses via [`forge_tasks::TaskStatus::parse`].
    pub fn from_toml_str(body: &str) -> anyhow::Result<Self> {
        let raw: RawMapping = toml::from_str(body)
            .map_err(|e| anyhow::anyhow!("sheet mapping: invalid TOML: {e}"))?;
        SheetMapping::try_from(raw)
    }

    /// Walks up from the current working directory looking for
    /// `.forge/sheets/<alias>.toml`, mirroring the ancestor-walk pattern in
    /// `crates/tasks/src/config.rs` (`read_config_cql_host`). Only builds/
    /// locates the path — does not read or parse it. If not found, returns
    /// the fallback path `.forge/sheets/<alias>.toml`.
    pub fn alias_path(alias: &str) -> PathBuf {
        let fallback = PathBuf::from(".forge")
            .join("sheets")
            .join(format!("{alias}.toml"));
        std::env::current_dir()
            .ok()
            .and_then(|cwd| locate_alias_path(&cwd, alias))
            .unwrap_or(fallback)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The spec's canonical `.forge/sheets/<alias>.toml` example (see
    /// `specs/todo/feat-sheet-sync.md`, "Config"), with `writable`,
    /// `dev_writable_status`, and `terminal_status` hoisted above the
    /// `[columns]`/`[status_map]` tables.
    ///
    /// TOML semantics: a bare `key = value` line binds to whichever
    /// `[table]` most recently opened, not to the document root, until the
    /// next `[table]` header. The spec doc's example places those three
    /// array keys *after* `[columns]` / `[status_map]`, so a byte-for-byte
    /// copy silently nests them inside those tables instead of the mapping
    /// root (confirmed via `tomllib`/`toml` — `columns.writable` becomes a
    /// `Vec`, breaking the `BTreeMap<String, String>` shape). That's a
    /// latent bug in the spec's example, not an intentional layout, so this
    /// fixture reorders the *keys* while keeping every value verbatim. See
    /// the Task 2 report's "concerns" section — Task 10 must not copy the
    /// doc's ordering byte-for-byte into `.forge/sheets/spoton-qa.toml`.
    const VALID_TOML: &str = r#"
spreadsheet_id = "EXAMPLE_SPREADSHEET_ID"
tab            = "QA Log"
id_column      = "QA Log ID"

writable = ["status", "fix_ver", "resolution_notes"]
dev_writable_status = ["In Progress", "In Review", "Fixed - Needs Verification"]
terminal_status = ["Verified/Closed", "Won't Fix", "Duplicate"]

[columns]
"QA Log ID"          = "id"
"Title"              = "title"
"Type"               = "type"
"Category"           = "category"
"Description"        = "description"
"Steps to Reproduce" = "steps"
"Expected Result"    = "expected"
"Actual Result"      = "actual"
"Environment"        = "environment"
"Severity"           = "severity"
"Priority"           = "priority"
"MVP Blocker?"       = "mvp_blocker"
"Status"             = "status"
"Build/Version Fixed (git commit for demo, version tag for prod)" = "fix_ver"
"Resolution Notes"   = "resolution_notes"

[status_map]
"New"                       = "triage"
"Triaged"                   = "ready"
"In Progress"               = "in_progress"
"In Review"                 = "in_progress"
"Fixed - Needs Verification" = "complete"
"Verified/Closed"           = "archived"
"Won't Fix"                 = "archived"
"Duplicate"                 = "archived"
"Deferred"                  = "blocked"
"#;

    #[test]
    fn parses_the_spec_example() {
        let mapping = SheetMapping::from_toml_str(VALID_TOML).expect("valid mapping should parse");
        assert_eq!(mapping.spreadsheet_id, "EXAMPLE_SPREADSHEET_ID");
        assert_eq!(mapping.tab, "QA Log");
        assert_eq!(mapping.id_column, "QA Log ID");
        assert_eq!(mapping.columns.get("QA Log ID"), Some(&CanonicalField::Id));
        assert_eq!(mapping.columns.len(), 15);
        assert!(mapping.writable.contains(&CanonicalField::Status));
        assert!(mapping.writable.contains(&CanonicalField::FixVer));
        assert!(mapping.writable.contains(&CanonicalField::ResolutionNotes));
        assert_eq!(
            mapping.status_map.get("New"),
            Some(&forge_tasks::TaskStatus::Triage)
        );
        assert_eq!(
            mapping.status_map.get("In Review"),
            Some(&forge_tasks::TaskStatus::InProgress)
        );
        assert!(mapping
            .dev_writable_status
            .contains("Fixed - Needs Verification"));
        assert!(mapping.terminal_status.contains("Verified/Closed"));
    }

    #[test]
    fn missing_id_column_key_is_err_mentioning_id_column() {
        let toml = r#"
spreadsheet_id = "sheet-1"
tab            = "Log"
id_column      = "Row ID"

[columns]
"Title" = "title"
"Status" = "status"
"#;
        let err = SheetMapping::from_toml_str(toml).expect_err("missing id_column should fail");
        assert!(
            err.to_string().contains("id_column"),
            "error should mention id_column, got: {err}"
        );
    }

    #[test]
    fn writable_naming_unmapped_field_is_err() {
        let toml = r#"
spreadsheet_id = "sheet-1"
tab            = "Log"
id_column      = "Row ID"

[columns]
"Row ID" = "id"
"Title"  = "title"

writable = ["fix_ver"]
"#;
        let err =
            SheetMapping::from_toml_str(toml).expect_err("unmapped writable field should fail");
        assert!(
            err.to_string().contains("fix_ver"),
            "error should mention the unmapped field, got: {err}"
        );
    }

    #[test]
    fn unknown_status_map_target_is_err() {
        let toml = r#"
spreadsheet_id = "sheet-1"
tab            = "Log"
id_column      = "Row ID"

[columns]
"Row ID" = "id"
"Status" = "status"

[status_map]
"New" = "not_a_real_status"
"#;
        let err = SheetMapping::from_toml_str(toml).expect_err("unknown status target should fail");
        assert!(
            err.to_string().contains("not_a_real_status"),
            "error should mention the offending value, got: {err}"
        );
    }

    #[test]
    fn empty_columns_is_err() {
        let toml = r#"
spreadsheet_id = "sheet-1"
tab            = "Log"
id_column      = "Row ID"
"#;
        let err = SheetMapping::from_toml_str(toml).expect_err("empty columns should fail");
        assert!(
            err.to_string().contains("columns"),
            "error should mention columns, got: {err}"
        );
    }

    #[test]
    fn locate_alias_path_finds_in_ancestor() {
        let tmpdir = tempfile::TempDir::new().expect("tempdir creation");
        let root = tmpdir.path();

        // Create .forge/sheets/spoton-qa.toml in the root
        let sheets_dir = root.join(".forge").join("sheets");
        std::fs::create_dir_all(&sheets_dir).expect("create .forge/sheets");
        let config_path = sheets_dir.join("spoton-qa.toml");
        std::fs::write(&config_path, "test content").expect("write config file");

        // Create nested subdirectory a/b
        let nested = root.join("a").join("b");
        std::fs::create_dir_all(&nested).expect("create nested dir");

        // Call locate_alias_path from the nested dir
        let result =
            locate_alias_path(&nested, "spoton-qa").expect("should find config in ancestor");

        assert_eq!(result, config_path);
    }

    #[test]
    fn locate_alias_path_returns_none_when_not_found() {
        let tmpdir = tempfile::TempDir::new().expect("tempdir creation");
        let root = tmpdir.path();

        // Create nested subdirectory a/b but no .forge/sheets/does-not-exist.toml
        let nested = root.join("a").join("b");
        std::fs::create_dir_all(&nested).expect("create nested dir");

        // Call locate_alias_path for a non-existent alias
        let result = locate_alias_path(&nested, "does-not-exist");

        assert_eq!(result, None);
    }
}
