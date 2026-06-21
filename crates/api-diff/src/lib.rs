//! Detect breaking API changes between two source trees (or two source files)
//! by comparing the public symbols extracted by `forge-outline`.
//!
//! v1 is a string-equality comparison of the outlined argument list — it does
//! not understand type equivalence (`&str` vs `&'a str`). Git-ref resolution
//! is left to the CLI layer: this crate operates on already-checked-out
//! directories and in-memory sources.

use anyhow::Result;
use forge_outline::outline;
use ignore::WalkBuilder;
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// Output types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Breaking,
    Minor,
    Patch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeKind {
    Added,
    Removed,
    SignatureChanged,
    FieldAdded,
    FieldRemoved,
}

#[derive(Debug, Serialize)]
pub struct Change {
    pub kind: ChangeKind,
    pub symbol: String,
    pub file: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub before: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub after: Option<String>,
    pub severity: Severity,
}

#[derive(Debug, Serialize)]
pub struct Summary {
    pub breaking: usize,
    pub minor: usize,
    pub patch: usize,
}

#[derive(Debug, Serialize)]
pub struct DiffReport {
    pub changes: Vec<Change>,
    pub summary: Summary,
    pub suggested_bump: String,
}

// ---------------------------------------------------------------------------
// Internal symbol representation
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct Symbol {
    name: String,
    /// Canonical signature used for equality checks. For functions this is
    /// `"fn <name>(<args>)"`, for types it is `"<kind> <name>"`.
    signature: String,
    file: String,
}

fn canonical_fn(kind: &str, name: &str, args: &str) -> String {
    format!("{kind} {name}({args})")
}

fn canonical_type(kind: &str, name: &str) -> String {
    format!("{kind} {name}")
}

/// Extract public symbols (functions + types) from a single source file.
/// Keys are `"<file>::<kind>:<name>"` so that structs and functions with the
/// same name do not collide.
fn extract_symbols(file_path: &str, source: &str) -> BTreeMap<String, Symbol> {
    let o = outline(file_path, source);
    let mut map = BTreeMap::new();
    for f in o.public_functions {
        let sig = canonical_fn("fn", &f.name, &f.args);
        let key = format!("{file_path}::fn:{}", f.name);
        map.insert(
            key,
            Symbol {
                name: f.name,
                signature: sig,
                file: file_path.to_string(),
            },
        );
    }
    for t in o.types {
        let sig = canonical_type(&t.kind, &t.name);
        let key = format!("{file_path}::{}:{}", t.kind, t.name);
        map.insert(
            key,
            Symbol {
                name: t.name,
                signature: sig,
                file: file_path.to_string(),
            },
        );
    }
    map
}

// ---------------------------------------------------------------------------
// Diff logic
// ---------------------------------------------------------------------------

fn diff_symbol_maps(
    before: &BTreeMap<String, Symbol>,
    after: &BTreeMap<String, Symbol>,
) -> Vec<Change> {
    let mut changes = Vec::new();

    for (key, before_sym) in before {
        match after.get(key) {
            None => {
                changes.push(Change {
                    kind: ChangeKind::Removed,
                    symbol: before_sym.name.clone(),
                    file: before_sym.file.clone(),
                    before: Some(before_sym.signature.clone()),
                    after: None,
                    severity: Severity::Breaking,
                });
            }
            Some(after_sym) => {
                if before_sym.signature != after_sym.signature {
                    changes.push(Change {
                        kind: ChangeKind::SignatureChanged,
                        symbol: before_sym.name.clone(),
                        file: after_sym.file.clone(),
                        before: Some(before_sym.signature.clone()),
                        after: Some(after_sym.signature.clone()),
                        severity: Severity::Breaking,
                    });
                }
            }
        }
    }

    for (key, after_sym) in after {
        if !before.contains_key(key) {
            changes.push(Change {
                kind: ChangeKind::Added,
                symbol: after_sym.name.clone(),
                file: after_sym.file.clone(),
                before: None,
                after: Some(after_sym.signature.clone()),
                severity: Severity::Minor,
            });
        }
    }

    changes
}

fn summarise(changes: &[Change]) -> (Summary, String) {
    let mut breaking = 0;
    let mut minor = 0;
    let mut patch = 0;
    for c in changes {
        match c.severity {
            Severity::Breaking => breaking += 1,
            Severity::Minor => minor += 1,
            Severity::Patch => patch += 1,
        }
    }
    let bump = if breaking > 0 {
        "major"
    } else if minor > 0 {
        "minor"
    } else {
        "patch"
    }
    .to_string();
    (
        Summary {
            breaking,
            minor,
            patch,
        },
        bump,
    )
}

// ---------------------------------------------------------------------------
// Public API — single file pair
// ---------------------------------------------------------------------------

/// Diff the public API of two versions of a single source file.
pub fn diff_sources(before: &str, after: &str, filename: &str) -> Result<DiffReport> {
    let before_map = extract_symbols(filename, before);
    let after_map = extract_symbols(filename, after);
    let changes = diff_symbol_maps(&before_map, &after_map);
    let (summary, suggested_bump) = summarise(&changes);
    Ok(DiffReport {
        changes,
        summary,
        suggested_bump,
    })
}

// ---------------------------------------------------------------------------
// Public API — directory pair
// ---------------------------------------------------------------------------

/// Languages whose files we know how to outline.
fn supported_ext(ext: &str) -> bool {
    matches!(
        ext,
        "rs" | "py"
            | "go"
            | "ts"
            | "tsx"
            | "js"
            | "jsx"
            | "ex"
            | "exs"
            | "java"
            | "cpp"
            | "cc"
            | "h"
            | "hpp"
    )
}

/// Walk a directory and build a symbol map keyed by
/// `<relative_path>::<kind>:<symbol_name>`.
fn walk_tree(root: &Path, lang_hint: Option<&str>) -> Result<BTreeMap<String, Symbol>> {
    let mut map: BTreeMap<String, Symbol> = BTreeMap::new();

    let walker = WalkBuilder::new(root)
        .hidden(false)
        .git_ignore(true)
        .build();

    for dent in walker.flatten() {
        let path: PathBuf = dent.path().to_path_buf();
        if !path.is_file() {
            continue;
        }
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase();
        if !supported_ext(&ext) {
            continue;
        }
        if let Some(hint) = lang_hint {
            // Only keep files matching the hinted language extension group.
            let matches = match hint {
                "rust" => ext == "rs",
                "python" => ext == "py",
                "go" => ext == "go",
                "typescript" => matches!(ext.as_str(), "ts" | "tsx" | "js" | "jsx"),
                "elixir" => matches!(ext.as_str(), "ex" | "exs"),
                "java" => ext == "java",
                "cpp" => matches!(ext.as_str(), "cpp" | "cc" | "h" | "hpp"),
                _ => true,
            };
            if !matches {
                continue;
            }
        }

        let source = match std::fs::read_to_string(&path) {
            Ok(s) => s,
            Err(_) => continue,
        };
        let rel = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .to_string_lossy()
            .to_string();

        let file_symbols = extract_symbols(&rel, &source);
        map.extend(file_symbols);
    }

    Ok(map)
}

/// Diff the public API between two source trees.
pub fn diff_trees(
    before_dir: &Path,
    after_dir: &Path,
    lang_hint: Option<&str>,
) -> Result<DiffReport> {
    let before_map = walk_tree(before_dir, lang_hint)?;
    let after_map = walk_tree(after_dir, lang_hint)?;
    let changes = diff_symbol_maps(&before_map, &after_map);
    let (summary, suggested_bump) = summarise(&changes);
    Ok(DiffReport {
        changes,
        summary,
        suggested_bump,
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn remove_pub_fn_is_breaking() {
        let before = r#"
pub fn parse(s: &str) -> usize { s.len() }
pub fn stay() {}
"#;
        let after = r#"
pub fn stay() {}
"#;
        let report = diff_sources(before, after, "lib.rs").unwrap();
        assert!(report
            .changes
            .iter()
            .any(|c| matches!(c.kind, ChangeKind::Removed) && c.symbol == "parse"));
        assert_eq!(report.suggested_bump, "major");
        assert!(report.summary.breaking >= 1);
    }

    #[test]
    fn change_arg_list_is_signature_change() {
        let before = r#"
pub fn connect(url: &str) -> Result<()> { Ok(()) }
"#;
        let after = r#"
pub fn connect(url: &str, timeout: u64) -> Result<()> { Ok(()) }
"#;
        let report = diff_sources(before, after, "lib.rs").unwrap();
        let sig: Vec<_> = report
            .changes
            .iter()
            .filter(|c| matches!(c.kind, ChangeKind::SignatureChanged))
            .collect();
        assert_eq!(sig.len(), 1);
        assert_eq!(sig[0].symbol, "connect");
        assert!(matches!(sig[0].severity, Severity::Breaking));
        assert_eq!(report.suggested_bump, "major");
    }

    #[test]
    fn add_pub_fn_is_minor() {
        let before = r#"
pub fn a() {}
"#;
        let after = r#"
pub fn a() {}
pub fn b() {}
"#;
        let report = diff_sources(before, after, "lib.rs").unwrap();
        let added: Vec<_> = report
            .changes
            .iter()
            .filter(|c| matches!(c.kind, ChangeKind::Added))
            .collect();
        assert_eq!(added.len(), 1);
        assert_eq!(added[0].symbol, "b");
        assert!(matches!(added[0].severity, Severity::Minor));
        assert_eq!(report.suggested_bump, "minor");
    }

    #[test]
    fn no_changes_is_patch() {
        let src = r#"
pub fn a() {}
"#;
        let report = diff_sources(src, src, "lib.rs").unwrap();
        assert_eq!(report.suggested_bump, "patch");
        assert_eq!(report.summary.breaking, 0);
        assert_eq!(report.summary.minor, 0);
    }

    #[test]
    fn tree_diff_aggregates_across_files() {
        let before_dir = TempDir::new().unwrap();
        let after_dir = TempDir::new().unwrap();

        // Unchanged file — stays in both trees.
        fs::write(before_dir.path().join("keep.rs"), "pub fn keep() {}\n").unwrap();
        fs::write(after_dir.path().join("keep.rs"), "pub fn keep() {}\n").unwrap();

        // Removed file — only in before.
        fs::write(before_dir.path().join("gone.rs"), "pub fn gone() {}\n").unwrap();

        // New file — only in after.
        fs::write(after_dir.path().join("fresh.rs"), "pub fn fresh() {}\n").unwrap();

        let report = diff_trees(before_dir.path(), after_dir.path(), Some("rust")).unwrap();

        let removed: Vec<_> = report
            .changes
            .iter()
            .filter(|c| matches!(c.kind, ChangeKind::Removed))
            .collect();
        let added: Vec<_> = report
            .changes
            .iter()
            .filter(|c| matches!(c.kind, ChangeKind::Added))
            .collect();

        assert_eq!(removed.len(), 1);
        assert_eq!(removed[0].symbol, "gone");
        assert_eq!(added.len(), 1);
        assert_eq!(added[0].symbol, "fresh");
        assert_eq!(report.suggested_bump, "major");
    }

    #[test]
    fn report_serializes_to_json() {
        let before = "pub fn a() {}";
        let after = "pub fn a(x: u32) {}";
        let report = diff_sources(before, after, "lib.rs").unwrap();
        let json = serde_json::to_string(&report).unwrap();
        assert!(json.contains("\"suggested_bump\":\"major\""));
        assert!(json.contains("\"signature_changed\""));
    }
}
