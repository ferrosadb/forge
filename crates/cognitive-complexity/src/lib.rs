//! Module: Cognitive complexity analysis for Rust source.
//! Correctness: Correct when every parsed function/method is scored with the
//! SonarSource cognitive complexity metric and nothing is silently dropped.
//! Last revised: 2026-10-01
//! Last changed: Initial implementation, ranked refactoring targets.

//! Cognitive complexity (SonarSource, G. Ann Campbell) for Rust, computed from
//! the `syn` AST via the `complexity` crate.
//!
//! Cyclomatic complexity counts branches; cognitive complexity scores what a
//! reader actually has to hold in their head. Nesting is penalised, flat
//! branching is rewarded, and a wide `match` stays cheap because it is easy to
//! read. That makes it a much better signal for *refactoring targets* than the
//! brace/regex heuristics in `forge-smell-detect`.
//!
//! ```no_run
//! use forge_cognitive_complexity::{analyze_path, CognitiveConfig};
//!
//! let report = analyze_path("src".as_ref(), &CognitiveConfig::default())?;
//! for hotspot in &report.functions {
//!     println!("{} {} cognitive={}", hotspot.file, hotspot.name, hotspot.cognitive);
//! }
//! # Ok::<(), anyhow::Error>(())
//! ```
//!
//! [`churn`] adds the other half of the picture: complexity weighted by how
//! often a file actually changes.

pub mod churn;
mod engine;
mod report;

pub use churn::{churn_factor, rank_files, ChurnConfig, ChurnMap, FileHotspot};

pub use engine::{
    analyze_dir, analyze_path, analyze_source, apply_limits, merge_reports, rust_files, unbounded,
    CognitiveConfig, DEFAULT_TOP,
};
pub use report::summarize;
pub use report::{
    band, Band, BandCounts, CognitiveReport, FailureKind, FileFailure, FunctionComplexity,
    FunctionKind, Percentiles, HIGH_THRESHOLD, MODERATE_THRESHOLD, SEVERE_THRESHOLD,
};

#[cfg(test)]
mod tests {
    use super::*;

    /// Small fixtures must not be affected by the default output cap, so tests
    /// build on an unbounded config unless they are testing the cap itself.
    fn analyze(src: &str, config: &CognitiveConfig) -> CognitiveReport {
        analyze_source("fixture.rs", src, config)
    }

    fn analyze_all(src: &str) -> CognitiveReport {
        analyze_source("fixture.rs", src, &unbounded(&CognitiveConfig::default()))
    }

    /// The metric must match the definition published with the upstream crate:
    /// nested loops + if = 7, a wide `match` = 1.
    #[test]
    fn scores_match_the_published_definition() {
        let src = r#"
fn sum_of_primes(max: u64) -> u64 {
    let mut total = 0;
    'outer: for i in 1..=max {
        for j in 2..i {
            if i % j == 0 {
                continue 'outer;
            }
        }
        total += i;
    }
    total
}

fn get_words(number: u64) -> &'static str {
    match number {
        1 => "one",
        2 => "a couple",
        3 => "a few",
        _ => "lots",
    }
}
"#;
        let report = analyze_all(src);
        assert_eq!(report.functions_analyzed, 2, "both functions reported");
        let by_name = |n: &str| {
            report
                .functions
                .iter()
                .find(|f| f.name == n)
                .unwrap_or_else(|| panic!("{n} missing from report"))
                .cognitive
        };
        assert_eq!(by_name("sum_of_primes"), 7, "nested loops + if = 7");
        assert_eq!(by_name("get_words"), 1, "flat match = 1 regardless of arms");
    }

    #[test]
    fn ranks_hotspots_first_and_reports_every_function_in_the_totals() {
        let src = r#"
fn trivial() -> u32 { 1 }

fn gnarly(xs: &[u32]) -> u32 {
    let mut total = 0;
    for x in xs {
        if *x > 1 {
            for y in 0..*x {
                if y % 2 == 0 {
                    total += y;
                }
            }
        }
    }
    total
}
"#;
        let report = analyze_all(src);
        assert_eq!(report.functions[0].name, "gnarly", "hotspot ranked first");
        assert_eq!(report.functions_analyzed, 2);
        assert_eq!(report.functions_total, 2);
        assert_eq!(report.returned, 2);
        assert!(!report.truncated);
        assert_eq!(report.max_cognitive, report.functions[0].cognitive);
        assert!(report.total_cognitive >= report.max_cognitive);
    }

    /// Truncation must never be silent: the caller can always tell how many
    /// matches were withheld.
    #[test]
    fn top_truncation_is_explicit() {
        let mut src = String::new();
        for i in 0..6 {
            src.push_str(&format!(
                "fn f{i}() -> u32 {{ if true {{ 1 }} else {{ 2 }} }}\n"
            ));
        }
        let config = CognitiveConfig {
            top: Some(2),
            ..CognitiveConfig::default()
        };
        let report = analyze(&src, &config);
        assert_eq!(report.returned, 2);
        assert_eq!(report.functions_total, 6, "total counts every match");
        assert_eq!(report.functions_analyzed, 6);
        assert!(report.truncated, "withheld matches are disclosed");
        assert_eq!(report.functions.len(), 2);
    }

    #[test]
    fn threshold_filter_is_disclosed_in_totals() {
        let src = r#"
fn flat() -> u32 { 1 }

fn nested(xs: &[u32]) -> u32 {
    let mut t = 0;
    for x in xs {
        for y in 0..*x {
            t += y;
        }
    }
    t
}
"#;
        let config = CognitiveConfig {
            max_cognitive: Some(3),
            ..CognitiveConfig::default()
        };
        let report = analyze(src, &config);
        assert_eq!(report.threshold, Some(3));
        assert_eq!(report.functions_total, 1, "only the hotspot clears the bar");
        assert_eq!(report.functions[0].name, "nested");
        assert_eq!(report.functions_analyzed, 2, "everything was still scored");
    }

    #[test]
    fn deterministic_across_runs() {
        let src = r#"
impl Alpha {
    fn one(&self) -> u32 { if true { 1 } else { 2 } }
    fn two(&self) -> u32 { if true { 1 } else { 2 } }
}
fn alpha() -> u32 { if true { 1 } else { 2 } }
"#;
        let first = analyze_all(src);
        let second = analyze_all(src);
        assert_eq!(
            serde_json::to_string(&first).unwrap(),
            serde_json::to_string(&second).unwrap(),
            "same input must serialize identically"
        );
    }

    #[test]
    fn methods_are_named_with_their_self_type() {
        let src = r#"
impl Engine {
    fn run(&self, xs: &[u32]) -> u32 {
        let mut t = 0;
        for x in xs {
            if *x > 0 {
                t += x;
            }
        }
        t
    }
}
"#;
        let report = analyze_all(src);
        assert_eq!(report.functions.len(), 1);
        assert_eq!(report.functions[0].name, "Engine::run");
        assert_eq!(report.functions[0].kind, FunctionKind::Method);
        assert_eq!(report.functions[0].line, 3, "1-based line of the signature");
        // Signature line `fn run(...)` starts at 3, closing brace at 11.
        assert_eq!(
            report.functions[0].lines, 9,
            "signature through closing brace, inclusive"
        );
    }

    /// `lines` must span the signature through the closing brace. Doc comments
    /// and attributes above the signature are not part of the body.
    #[test]
    fn line_count_spans_signature_to_closing_brace() {
        let src = r#"/// Doc comment, not part of the body.
#[inline]
fn documented(x: u32) -> u32 {
    if x > 1 {
        x
    } else {
        0
    }
}
"#;
        let report = analyze_all(src);
        assert_eq!(report.functions.len(), 1);
        assert_eq!(report.functions[0].line, 3, "signature line, not the docs");
        assert_eq!(
            report.functions[0].lines, 7,
            "lines 3..=9 is 7 lines, signature through closing brace"
        );
    }

    #[test]
    fn nested_modules_are_prefixed_and_walked() {
        let src = r#"
mod inner {
    pub mod deeper {
        fn buried() -> u32 { if true { 1 } else { 2 } }
    }
}
"#;
        let report = analyze_all(src);
        assert_eq!(report.functions.len(), 1);
        assert_eq!(report.functions[0].name, "inner::deeper::buried");
    }

    #[test]
    fn nesting_depth_is_measured_from_the_ast() {
        let src = r#"
fn shallow(xs: &[u32]) -> u32 {
    let mut t = 0;
    for x in xs {
        if *x > 0 { t += x; }
    }
    t
}

fn deep(xs: &[u32]) -> u32 {
    let mut t = 0;
    for x in xs {
        for y in 0..*x {
            for z in 0..y {
                t += z;
            }
        }
    }
    t
}
"#;
        let report = analyze_all(src);
        let by_name = |n: &str| {
            report
                .functions
                .iter()
                .find(|f| f.name == n)
                .expect("function present")
        };
        assert_eq!(by_name("shallow").nesting, 2, "for + if");
        assert_eq!(by_name("deep").nesting, 3, "three nested loops");
    }

    #[test]
    fn bands_follow_the_calibrated_thresholds() {
        assert_eq!(band(0), Band::Low);
        assert_eq!(band(MODERATE_THRESHOLD - 1), Band::Low);
        assert_eq!(band(MODERATE_THRESHOLD), Band::Moderate);
        assert_eq!(band(HIGH_THRESHOLD - 1), Band::Moderate);
        assert_eq!(band(HIGH_THRESHOLD), Band::High);
        assert_eq!(band(SEVERE_THRESHOLD - 1), Band::High);
        assert_eq!(band(SEVERE_THRESHOLD), Band::Severe);
    }

    /// The default output cap must bound the list while leaving the full
    /// population visible in the counts — bounded output, no silent loss.
    #[test]
    fn default_config_bounds_the_ranked_list() {
        let mut src = String::new();
        for i in 0..(DEFAULT_TOP + 25) {
            src.push_str(&format!(
                "fn f{i}() -> u32 {{ if true {{ 1 }} else {{ 2 }} }}\n"
            ));
        }
        let report = analyze(src.as_str(), &CognitiveConfig::default());
        assert_eq!(report.returned, DEFAULT_TOP, "list is capped by default");
        assert_eq!(
            report.functions_analyzed,
            DEFAULT_TOP + 25,
            "every function is still counted"
        );
        assert!(report.truncated, "the cap is disclosed");
        assert_eq!(report.top, Some(DEFAULT_TOP));
    }

    /// The calibration inputs must be the published ones: the bands sit on the
    /// measured percentile curve, not on round numbers.
    #[test]
    fn thresholds_match_the_calibration() {
        assert_eq!(
            (MODERATE_THRESHOLD, HIGH_THRESHOLD, SEVERE_THRESHOLD),
            (10, 20, 40)
        );
    }

    #[test]
    fn band_counts_sum_to_the_scored_population() {
        let src = r#"
fn flat() -> u32 { 1 }
fn nested(xs: &[u32]) -> u32 {
    let mut t = 0;
    for x in xs { for y in 0..*x { if y > 0 { t += y; } } }
    t
}
"#;
        let report = analyze_all(src);
        let counts = report.bands;
        assert_eq!(
            counts.low + counts.moderate + counts.high + counts.severe,
            report.functions_analyzed
        );
    }

    /// A file that does not parse must be reported, never silently skipped.
    #[test]
    fn unparsable_source_is_reported_not_dropped() {
        let report = analyze("fn broken( { ", &CognitiveConfig::default());
        assert_eq!(report.files_scanned, 1);
        assert_eq!(report.files_analyzed, 0);
        assert_eq!(report.failures.len(), 1);
        assert_eq!(report.failures[0].kind, FailureKind::Parse);
        assert!(
            !report.is_trustworthy(),
            "a scan that produced no analysis is not trustworthy"
        );
    }

    #[test]
    fn parse_failures_do_not_abort_the_rest_of_the_scan() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("good.rs"), "fn ok() -> u32 { 1 }\n").unwrap();
        std::fs::write(dir.path().join("bad.rs"), "fn broken( {\n").unwrap();
        std::fs::write(dir.path().join("notes.txt"), "not rust, ignored\n").unwrap();

        let report = analyze_path(dir.path(), &CognitiveConfig::default()).unwrap();
        assert_eq!(report.files_analyzed, 1, "the good file still analyzed");
        assert_eq!(report.functions_analyzed, 1);
        assert_eq!(report.failures.len(), 1, "the bad file is disclosed");
        assert_eq!(report.files_scanned, 2, "only .rs files are scanned");
        assert!(report.is_trustworthy(), "partial analysis is still useful");
    }

    #[test]
    fn test_modules_can_be_excluded_without_hiding_the_decision() {
        let src = r#"
fn prod(xs: &[u32]) -> u32 {
    let mut t = 0;
    for x in xs { if *x > 0 { t += x; } }
    t
}

#[cfg(test)]
mod tests {
    fn test_only(xs: &[u32]) -> u32 {
        let mut t = 0;
        for x in xs { for y in 0..*x { if y > 0 { t += y; } } }
        t
    }
}
"#;
        let with_tests = analyze_all(src);
        assert_eq!(with_tests.functions_analyzed, 2);

        let config = CognitiveConfig {
            exclude_tests: true,
            ..CognitiveConfig::default()
        };
        let without_tests = analyze(src, &config);
        assert_eq!(without_tests.functions_analyzed, 1);
        assert_eq!(without_tests.functions[0].name, "prod");
    }

    #[test]
    fn empty_input_is_a_valid_empty_report() {
        let report = analyze("", &CognitiveConfig::default());
        assert_eq!(report.functions_analyzed, 0);
        assert!(report.functions.is_empty());
        assert!(report.failures.is_empty());
        assert!(report.is_trustworthy());
        assert_eq!(report.max_cognitive, 0);
    }

    /// syn 1 panics (rather than erroring) on C-string literals, which are
    /// valid since Rust 1.77 and appear in real code. An uncaught panic would
    /// abort the whole scan and produce no report at all — this is the
    /// regression that killed a full-repository scan.
    #[test]
    fn c_string_literal_is_a_disclosed_failure_not_a_dead_process() {
        let source = "fn with_cstr() -> &'static std::ffi::CStr {\n    c\"hello\"\n}\n";
        let report = analyze(source, &CognitiveConfig::default());
        assert_eq!(report.files_scanned, 1);
        assert_eq!(report.files_analyzed, 0, "the file cannot be scored");
        assert_eq!(report.failures.len(), 1, "the failure is disclosed");
        assert_eq!(report.failures[0].kind, FailureKind::Parse);
        assert!(
            report.failures[0].detail.contains("panicked"),
            "the panic is explained: {}",
            report.failures[0].detail
        );
    }

    /// The panic must be contained per file: every other file still scores.
    #[test]
    fn a_c_string_file_does_not_take_down_the_scan() {
        let dir = tempfile::tempdir().unwrap();
        // Sorted order puts the bad file first, so containment is proven even
        // when the panic happens early in the walk.
        std::fs::write(
            dir.path().join("a_bad.rs"),
            "fn with_cstr() -> &'static std::ffi::CStr {\n    c\"boom\"\n}\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("b_good.rs"),
            "fn gnarly(xs: &[u32]) -> u32 {\n    let mut t = 0;\n    for x in xs {\n        if *x > 0 {\n            for y in 0..*x {\n                if y > 0 { t += y; }\n            }\n        }\n    }\n    t\n}\n",
        )
        .unwrap();

        let report = analyze_path(dir.path(), &CognitiveConfig::default()).unwrap();
        assert_eq!(report.files_scanned, 2);
        assert_eq!(report.files_analyzed, 1, "the good file still scores");
        assert_eq!(report.functions_analyzed, 1);
        assert_eq!(report.failures.len(), 1, "the bad file is disclosed");
        assert!(
            report.functions[0].cognitive >= MODERATE_THRESHOLD,
            "the good file produced a real score: {:?}",
            report.functions
        );
        assert!(report.is_trustworthy(), "partial analysis is still useful");
    }
}
