//! Grid → canonical rows: header resolution, terminal-status skip, and
//! duplicate-ID refusal.
//!
//! [`map_grid`] is the boundary between the raw [`Grid`] fetched from the
//! Sheets API and the [`CanonicalRow`]s the rest of the sync engine works
//! with. It fails loud (returns `Err`) if the mapping's `columns` reference
//! a header the sheet doesn't actually have — a stale or misconfigured
//! `.forge/sheets/<alias>.toml` must never silently drop data. See
//! `specs/todo/feat-sheet-sync.md`, "Duplicate ID → fail loud" and
//! "Sync semantics".

use std::collections::{BTreeMap, HashMap};

use crate::config::SheetMapping;
use crate::model::{CanonicalField, CanonicalRow, Grid};
use crate::normalize::{find_duplicate_ids, normalize_id};

/// The result of mapping a raw [`Grid`] through a [`SheetMapping`]: the
/// surviving canonical rows, plus the ids that were excluded and why.
///
/// `rows` never contains a row whose normalized id appears in
/// `duplicate_ids` or whose normalized id was recorded in
/// `skipped_terminal` — both are exclusions, not just annotations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MappedGrid {
    pub rows: Vec<CanonicalRow>,
    pub skipped_terminal: Vec<String>,
    pub duplicate_ids: Vec<String>,
}

/// Maps `grid` to canonical rows using `mapping`.
///
/// Fails loud with `Err` naming the offending header if any key in
/// `mapping.columns` is absent from `grid.headers` — a hard error, no
/// partial mapping is ever returned.
///
/// Per data row (order of exclusion checks matters — each is mutually
/// exclusive since a row is dropped on the first match):
/// 1. A blank/empty normalized id is not a real bug row: excluded from
///    `rows` and from duplicate detection entirely.
/// 2. A normalized id in `duplicate_ids` (computed once, up front, over all
///    non-blank normalized ids) is an ambiguous join: excluded from `rows`.
/// 3. A row whose mapped Status cell is in `mapping.terminal_status` is
///    excluded from `rows` and its normalized id recorded in
///    `skipped_terminal`.
///
/// Rows may be ragged (the Sheets API omits trailing empty cells): any
/// cell index beyond a row's length is treated as `""`, never a panic.
pub fn map_grid(grid: &Grid, mapping: &SheetMapping) -> anyhow::Result<MappedGrid> {
    let header_index: HashMap<&str, usize> = grid
        .headers
        .iter()
        .enumerate()
        .map(|(idx, header)| (header.as_str(), idx))
        .collect();

    for header in mapping.columns.keys() {
        if !header_index.contains_key(header.as_str()) {
            anyhow::bail!(
                "sheet mapping: mapped header {header:?} not found in grid headers {:?}",
                grid.headers
            );
        }
    }

    // Validated above: every mapping.columns key (including id_column, per
    // SheetMapping's own validation) is present in header_index.
    let id_index = header_index[mapping.id_column.as_str()];
    let status_header = mapping
        .columns
        .iter()
        .find(|(_, field)| **field == CanonicalField::Status)
        .map(|(header, _)| header.as_str());

    let normalized_ids: Vec<String> = grid
        .rows
        .iter()
        .map(|row| normalize_id(cell(row, id_index)))
        .filter(|id| !id.is_empty())
        .collect();
    let duplicate_ids = find_duplicate_ids(&normalized_ids);

    let mut rows = Vec::new();
    let mut skipped_terminal = Vec::new();

    for (sheet_row_index, row) in grid.rows.iter().enumerate() {
        let normalized = normalize_id(cell(row, id_index));
        if normalized.is_empty() {
            continue;
        }
        if duplicate_ids.contains(&normalized) {
            continue;
        }
        if let Some(status_header) = status_header {
            let status_value = cell(row, header_index[status_header]);
            if mapping.terminal_status.contains(status_value) {
                skipped_terminal.push(normalized);
                continue;
            }
        }

        let mut fields = BTreeMap::new();
        for (header, field) in &mapping.columns {
            let value = cell(row, header_index[header.as_str()]);
            if !value.is_empty() {
                fields.insert(*field, value.to_string());
            }
        }

        rows.push(CanonicalRow {
            id: normalized,
            fields,
            sheet_row_index,
        });
    }

    Ok(MappedGrid {
        rows,
        skipped_terminal,
        duplicate_ids,
    })
}

/// Reads `row[idx]`, treating any index past the (possibly ragged) row's
/// end as an empty string rather than panicking.
fn cell(row: &[String], idx: usize) -> &str {
    row.get(idx).map(String::as_str).unwrap_or("")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Same shape as `config::tests::VALID_TOML` (real QA-sheet headers per
    /// `specs/todo/feat-sheet-sync.md`), reproduced here so this module's
    /// tests don't depend on `config`'s private test fixtures.
    const QA_MAPPING_TOML: &str = r#"
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

    fn qa_mapping() -> SheetMapping {
        SheetMapping::from_toml_str(QA_MAPPING_TOML).expect("fixture TOML should parse")
    }

    fn qa_headers() -> Vec<String> {
        [
            "QA Log ID",
            "Title",
            "Type",
            "Category",
            "Description",
            "Steps to Reproduce",
            "Expected Result",
            "Actual Result",
            "Environment",
            "Severity",
            "Priority",
            "MVP Blocker?",
            "Status",
            "Build/Version Fixed (git commit for demo, version tag for prod)",
            "Resolution Notes",
        ]
        .into_iter()
        .map(String::from)
        .collect()
    }

    fn row(cells: &[&str]) -> Vec<String> {
        cells.iter().map(|c| c.to_string()).collect()
    }

    #[test]
    fn bug_row_maps_every_populated_field() {
        let grid = Grid {
            headers: qa_headers(),
            rows: vec![row(&[
                "QA-001",
                "Login button unresponsive",
                "Bug",
                "UI",
                "Button does not respond to click",
                "1. Open app\n2. Click login",
                "Login modal opens",
                "Nothing happens",
                "Chrome 120 / macOS",
                "High",
                "P1",
                "Yes",
                "In Progress",
                "",
                "",
            ])],
        };

        let mapped = map_grid(&grid, &qa_mapping()).expect("mapping should succeed");
        assert!(mapped.duplicate_ids.is_empty());
        assert!(mapped.skipped_terminal.is_empty());
        assert_eq!(mapped.rows.len(), 1);

        let mapped_row = &mapped.rows[0];
        assert_eq!(mapped_row.id, "QA-001");
        assert_eq!(mapped_row.sheet_row_index, 0);
        assert_eq!(
            mapped_row.fields.get(&CanonicalField::Title),
            Some(&"Login button unresponsive".to_string())
        );
        assert_eq!(
            mapped_row.fields.get(&CanonicalField::Status),
            Some(&"In Progress".to_string())
        );
        assert_eq!(
            mapped_row.fields.get(&CanonicalField::Priority),
            Some(&"P1".to_string())
        );
        // Empty cells (fix_ver, resolution_notes) are not inserted.
        assert!(!mapped_row.fields.contains_key(&CanonicalField::FixVer));
    }

    #[test]
    fn terminal_status_row_is_skipped_not_mapped() {
        let grid = Grid {
            headers: qa_headers(),
            rows: vec![row(&[
                "QA-002",
                "Old bug",
                "Bug",
                "UI",
                "desc",
                "steps",
                "expected",
                "actual",
                "env",
                "Low",
                "P3",
                "No",
                "Verified/Closed",
                "abc123",
                "fixed",
            ])],
        };

        let mapped = map_grid(&grid, &qa_mapping()).expect("mapping should succeed");
        assert!(mapped.rows.is_empty());
        assert_eq!(mapped.skipped_terminal, vec!["QA-002".to_string()]);
        assert!(mapped.duplicate_ids.is_empty());
    }

    #[test]
    fn duplicate_ids_are_excluded_from_rows() {
        let grid = Grid {
            headers: qa_headers(),
            rows: vec![
                row(&[
                    "QA-005",
                    "First copy",
                    "Bug",
                    "UI",
                    "desc",
                    "steps",
                    "expected",
                    "actual",
                    "env",
                    "Medium",
                    "P2",
                    "No",
                    "New",
                    "",
                    "",
                ]),
                row(&[
                    "QA-005 (This may not be a bug)",
                    "Second copy",
                    "Bug",
                    "UI",
                    "desc",
                    "steps",
                    "expected",
                    "actual",
                    "env",
                    "Medium",
                    "P2",
                    "No",
                    "New",
                    "",
                    "",
                ]),
            ],
        };

        let mapped = map_grid(&grid, &qa_mapping()).expect("mapping should succeed");
        assert_eq!(mapped.duplicate_ids, vec!["QA-005".to_string()]);
        assert!(mapped.rows.is_empty());
        assert!(mapped.skipped_terminal.is_empty());
    }

    #[test]
    fn missing_mapped_header_is_err_naming_it() {
        let mut headers = qa_headers();
        headers.retain(|h| h != "Status");
        let grid = Grid {
            headers,
            rows: vec![],
        };

        let err = map_grid(&grid, &qa_mapping()).expect_err("missing header should fail loud");
        assert!(
            err.to_string().contains("Status"),
            "error should mention the missing header, got: {err}"
        );
    }

    #[test]
    fn ragged_row_does_not_panic_and_maps_present_cells() {
        let grid = Grid {
            headers: qa_headers(),
            rows: vec![row(&["QA-009", "Short row", "Bug"])],
        };

        let mapped = map_grid(&grid, &qa_mapping()).expect("mapping should succeed");
        assert_eq!(mapped.rows.len(), 1);
        let mapped_row = &mapped.rows[0];
        assert_eq!(mapped_row.id, "QA-009");
        assert_eq!(
            mapped_row.fields.get(&CanonicalField::Title),
            Some(&"Short row".to_string())
        );
        assert_eq!(
            mapped_row.fields.get(&CanonicalField::Type),
            Some(&"Bug".to_string())
        );
        // Missing trailing cells are treated as empty and thus not inserted.
        assert!(!mapped_row.fields.contains_key(&CanonicalField::Status));
        assert!(!mapped_row.fields.contains_key(&CanonicalField::Category));
    }
}
