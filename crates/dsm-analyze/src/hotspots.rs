//! Module: Cognitive-complexity hot spots mapped onto DSM elements.
//! Correctness: Correct when hot spots are attributed only to elements that
//! exist in the DSM, and every unattributable hot spot is counted and disclosed.
//! Last revised: 2026-10-01
//! Last changed: Initial implementation, ranked refactoring targets.

//! Bridge between `forge-cognitive-complexity` (function-level, AST) and the
//! DSM (element-level, dependency graph).
//!
//! A DSM tells you which elements are coupled; cognitive complexity tells you
//! which elements are hard to understand. Together they rank *where* to
//! refactor: a high-coupling element full of cognitive hot spots is a far
//! better first target than one of either alone.
//!
//! Attribution mirrors the Rust extractor's own module naming
//! (`path_to_module` in `extract/rust_lang.rs`) so labels match exactly in
//! module-level (`level = "full"`) analysis.

use forge_cognitive_complexity::{CognitiveConfig, CognitiveReport};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// One cognitive hot spot attributed to a DSM element.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HotspotEvidence {
    /// DSM element label this hot spot belongs to.
    pub element: String,
    /// Source path as scanned.
    pub file: String,
    /// Function or method name.
    pub name: String,
    /// SonarSource cognitive complexity.
    pub cognitive: u32,
    /// Maximum control-flow nesting depth.
    pub nesting: usize,
    /// 1-based line of the signature.
    pub line: usize,
}

/// Cognitive complexity findings carried alongside a DSM report.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CognitiveSummary {
    /// True when a cognitive scan was attempted.
    pub scanned: bool,
    /// Functions scored across every file scanned.
    pub functions_analyzed: usize,
    /// Files that could not be analyzed (parse or read failures).
    pub failures: usize,
    /// Hot spots that mapped onto a DSM element, worst first.
    pub hot_spots: Vec<HotspotEvidence>,
    /// Hot spots that could not be attributed to any DSM element.
    pub unattributed: usize,
    /// Disclosures about the scan and its attribution.
    pub warnings: Vec<String>,
}

/// Scan `root` for cognitive complexity and attribute findings to `labels`.
///
/// Never fails the DSM run: a scan error is reported as a warning and an empty
/// summary, because a cognitive scan is advisory evidence, not a gate.
pub fn collect(root: &Path, labels: &[String]) -> CognitiveSummary {
    let config = CognitiveConfig::default();
    let report = match forge_cognitive_complexity::analyze_path(root, &config) {
        Ok(report) => report,
        Err(err) => {
            return CognitiveSummary {
                warnings: vec![format!("cognitive complexity scan skipped: {err}")],
                ..Default::default()
            };
        }
    };
    map_report(&report, root, labels)
}

/// Attribute the hot spots in an already-computed report to DSM elements.
pub fn map_report(report: &CognitiveReport, root: &Path, labels: &[String]) -> CognitiveSummary {
    let mut summary = CognitiveSummary {
        scanned: true,
        functions_analyzed: report.functions_analyzed,
        failures: report.failures.len(),
        ..Default::default()
    };

    let mut hot_spots: Vec<HotspotEvidence> = Vec::new();
    for function in &report.functions {
        if function.cognitive < forge_cognitive_complexity::MODERATE_THRESHOLD {
            continue;
        }
        match attribute(&function.file, root, labels) {
            Some(element) => hot_spots.push(HotspotEvidence {
                element,
                file: function.file.clone(),
                name: function.name.clone(),
                cognitive: function.cognitive,
                nesting: function.nesting,
                line: function.line,
            }),
            None => summary.unattributed += 1,
        }
    }

    // Worst first, deterministic ties — same contract as the cognitive report.
    hot_spots.sort_by(|a, b| {
        b.cognitive
            .cmp(&a.cognitive)
            .then_with(|| a.element.cmp(&b.element))
            .then_with(|| a.name.cmp(&b.name))
            .then_with(|| a.line.cmp(&b.line))
    });

    if summary.failures > 0 {
        summary.warnings.push(format!(
            "{} file(s) could not be analyzed for cognitive complexity",
            summary.failures
        ));
    }
    if summary.unattributed > 0 {
        summary.warnings.push(format!(
            "{} cognitive hot spot(s) could not be attributed to a DSM element; \
             run with level=full for module-level attribution",
            summary.unattributed
        ));
    }

    summary.hot_spots = hot_spots;
    summary
}

/// Find the DSM element a source file belongs to.
///
/// Mirrors the Rust extractor's `path_to_module` so that module-level labels
/// match exactly, then falls back to the longest label that is a module-path
/// prefix of the file. Returns `None` rather than guessing.
fn attribute(file: &str, root: &Path, labels: &[String]) -> Option<String> {
    let module_path = module_path_of(file, root);
    let crate_prefix = "crate::";

    // Exact match first: this is what the extractor emits for the same file.
    let exact = format!("{crate_prefix}{module_path}");
    if labels.iter().any(|l| l == &exact) {
        return Some(exact);
    }
    if module_path.is_empty() && labels.iter().any(|l| l == "crate") {
        return Some("crate".to_string());
    }

    // Longest module-path prefix wins (a coarser element still owns its files).
    let mut best: Option<&String> = None;
    for label in labels {
        let Some(stripped) = label.strip_prefix(crate_prefix) else {
            continue;
        };
        if stripped.is_empty() {
            continue;
        }
        let is_prefix = module_path == stripped
            || module_path
                .strip_prefix(stripped)
                .is_some_and(|rest| rest.starts_with("::"));
        if is_prefix && best.is_none_or(|current| stripped.len() > current.len()) {
            best = Some(label);
        }
    }
    best.cloned()
}

/// Reproduce the extractor's module naming for a Rust source file.
///
/// Rules match `extract/rust_lang.rs::path_to_module`: drop the scan root, drop
/// a leading `src`, drop the `.rs` extension, and drop a trailing `mod`/`lib`
/// (those files represent their parent module).
fn module_path_of(file: &str, root: &Path) -> String {
    let path = Path::new(file);
    let relative = path.strip_prefix(root).unwrap_or(path);
    let mut parts: Vec<&str> = relative
        .components()
        .filter_map(|c| c.as_os_str().to_str())
        .collect();
    if parts.first() == Some(&"src") {
        parts.remove(0);
    }
    if let Some(last) = parts.last_mut() {
        if let Some(name) = last.strip_suffix(".rs") {
            *last = name;
        }
    }
    if parts.last() == Some(&"mod") || parts.last() == Some(&"lib") {
        parts.pop();
    }
    parts.join("::")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn module_path_matches_the_extractor_naming() {
        let root = Path::new(".");
        assert_eq!(module_path_of("src/lib.rs", root), "");
        assert_eq!(module_path_of("src/extract/mod.rs", root), "extract");
        assert_eq!(module_path_of("src/matrix.rs", root), "matrix");
        assert_eq!(
            module_path_of("src/extract/rust_lang.rs", root),
            "extract::rust_lang"
        );
    }

    #[test]
    fn attributes_a_file_to_its_exact_module_element() {
        let found = attribute(
            "src/extract/rust_lang.rs",
            Path::new("."),
            &labels(&["crate::extract", "crate::extract::rust_lang"]),
        );
        assert_eq!(found.as_deref(), Some("crate::extract::rust_lang"));
    }

    #[test]
    fn falls_back_to_the_longest_prefix_element() {
        // No exact element, but the parent module exists.
        let found = attribute(
            "src/extract/rust_lang.rs",
            Path::new("."),
            &labels(&["crate::extract"]),
        );
        assert_eq!(found.as_deref(), Some("crate::extract"));
    }

    #[test]
    fn attributes_the_crate_root_file_to_the_crate_element() {
        let found = attribute("src/lib.rs", Path::new("."), &labels(&["crate"]));
        assert_eq!(found.as_deref(), Some("crate"));
    }

    /// A package-level (summary) label must not swallow every file — and an
    /// unmappable file must be reported as unattributed, never guessed.
    #[test]
    fn unmappable_files_are_not_guessed() {
        assert_eq!(
            attribute(
                "src/extract/rust_lang.rs",
                Path::new("."),
                &labels(&["forge-dsm-analyze"])
            ),
            None
        );
    }

    #[test]
    fn a_sibling_module_is_not_matched_by_prefix() {
        // `crate::extra` must not capture `extra_utils.rs`.
        assert_eq!(
            attribute(
                "src/extra_utils.rs",
                Path::new("."),
                &labels(&["crate::extra"])
            ),
            None
        );
    }

    #[test]
    fn collect_on_a_missing_path_warns_instead_of_failing() {
        let summary = collect(
            Path::new("/nonexistent/definitely/not/here"),
            &labels(&["crate"]),
        );
        assert!(!summary.scanned);
        assert!(!summary.warnings.is_empty());
        assert!(summary.hot_spots.is_empty());
    }

    #[test]
    fn collect_ranks_hot_spots_worst_first() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("lib.rs"),
            r#"
fn mild(xs: &[u32]) -> u32 {
    let mut t = 0;
    for x in xs { if *x > 0 { t += x; } }
    t
}

fn harsh(xs: &[u32]) -> u32 {
    let mut t = 0;
    for x in xs {
        if *x > 0 {
            for y in 0..*x {
                if y > 0 {
                    for z in 0..y {
                        if z % 2 == 0 { t += z; }
                    }
                }
            }
        }
    }
    t
}
"#,
        )
        .unwrap();

        let summary = collect(dir.path(), &labels(&["crate"]));
        assert!(summary.scanned);
        assert_eq!(summary.functions_analyzed, 2, "both functions are scored");
        assert_eq!(summary.unattributed, 0, "lib.rs belongs to the crate root");
        // Only functions at or above the moderate threshold are hot spots:
        // `mild` (for + nested if) scores 3, `harsh` (six nested branches) 21.
        assert_eq!(summary.hot_spots.len(), 1);
        assert_eq!(summary.hot_spots[0].name, "harsh");
        assert_eq!(summary.hot_spots[0].element, "crate");
        assert!(
            summary.hot_spots[0].cognitive >= forge_cognitive_complexity::MODERATE_THRESHOLD,
            "only genuine hot spots are listed: {:?}",
            summary.hot_spots
        );
    }
}
