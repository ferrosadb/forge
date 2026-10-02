//! Module: Report types for cognitive complexity analysis.
//! Correctness: Correct when every scored function appears in the totals and
//! any withheld result is disclosed by an explicit count plus `truncated`.
//! Last revised: 2026-10-01
//! Last changed: Initial implementation, ranked refactoring targets.

use serde::{Deserialize, Serialize};

/// Cognitive complexity at which a function starts to be worth attention.
///
/// Thresholds are calibrated against measured Rust codebases, not picked for
/// roundness. Across ~186k functions in a large multi-crate Rust suite the
/// distribution was: p50 = 0, p90 = 7, p95 = 13, p99 = 40, p99.9 = 138 with a
/// long tail (max 714). The bands below land on that curve so each one names an
/// actionable proportion of a codebase rather than an arbitrary cut:
///
/// | band     | score | share of functions |
/// |----------|-------|--------------------|
/// | moderate |  10   | ~7%                |
/// | high     |  20   | ~3%                |
/// | severe   |  40   | ~1%                |
///
/// A lower high-threshold (15, SonarQube's own default for this rule) flagged
/// 4.2% of functions — thousands of findings, which is a report nobody acts on.
/// The goal here is a *target list*, so selectivity is the feature.
pub const MODERATE_THRESHOLD: u32 = 10;
/// Cognitive complexity at which a function is a likely refactoring target.
pub const HIGH_THRESHOLD: u32 = 20;
/// Cognitive complexity at which a function is hard to reason about at all.
pub const SEVERE_THRESHOLD: u32 = 40;

/// Severity band for a cognitive complexity score.
///
/// The cut points mirror Forge's existing cyclomatic thresholds so that a
/// reader moving between `smell-detect` and this tool sees familiar bands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Band {
    Low,
    Moderate,
    High,
    Severe,
}

/// Classify a raw cognitive complexity score into a severity band.
pub fn band(score: u32) -> Band {
    if score >= SEVERE_THRESHOLD {
        Band::Severe
    } else if score >= HIGH_THRESHOLD {
        Band::High
    } else if score >= MODERATE_THRESHOLD {
        Band::Moderate
    } else {
        Band::Low
    }
}

/// Whether a scored item is a free function or a method living in an
/// `impl`/`trait` block.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FunctionKind {
    Function,
    Method,
}

/// Why a source file produced no analysis.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FailureKind {
    /// The file is not valid Rust and could not be parsed.
    Parse,
    /// The file could not be read from disk.
    Read,
}

/// One scored function or method.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FunctionComplexity {
    /// Path as scanned.
    pub file: String,
    /// Fully qualified name, e.g. `Engine::run` or `inner::deeper::buried`.
    pub name: String,
    pub kind: FunctionKind,
    /// 1-based line of the signature.
    pub line: usize,
    /// Body length in lines: signature line through the closing brace,
    /// inclusive. Attributes and doc comments above the signature are excluded.
    pub lines: usize,
    /// SonarSource cognitive complexity score.
    pub cognitive: u32,
    /// Maximum control-flow nesting depth inside the body.
    pub nesting: usize,
    pub band: Band,
    /// True when the item is marked as a test (`#[test]`, `#[cfg(test)]`).
    pub is_test: bool,
}

/// A file that could not be analyzed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FileFailure {
    pub file: String,
    pub kind: FailureKind,
    pub detail: String,
}

/// Population of each severity band across everything scored.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BandCounts {
    pub low: usize,
    pub moderate: usize,
    pub high: usize,
    pub severe: usize,
}

/// Distribution of scores across everything scored.
///
/// Published so a caller can judge whether the fixed thresholds suit *their*
/// codebase rather than trusting them blindly: a repo denser than the ones the
/// thresholds were calibrated on shows up immediately in these numbers.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Percentiles {
    pub p50: u32,
    pub p75: u32,
    pub p90: u32,
    pub p95: u32,
    pub p99: u32,
}

impl Percentiles {
    /// Nearest-rank percentiles over the scored population.
    pub fn of(scores: &mut [u32]) -> Self {
        if scores.is_empty() {
            return Self::default();
        }
        scores.sort_unstable();
        let at = |p: f64| -> u32 {
            let rank = (p / 100.0 * scores.len() as f64).ceil() as usize;
            let idx = rank.saturating_sub(1).min(scores.len() - 1);
            scores[idx]
        };
        Self {
            p50: at(50.0),
            p75: at(75.0),
            p90: at(90.0),
            p95: at(95.0),
            p99: at(99.0),
        }
    }
}

/// Result of a cognitive complexity scan.
///
/// Counts are the contract: `functions_analyzed` is everything scored,
/// `functions_total` is everything matching the threshold filter, and
/// `returned`/`truncated` describe what is in `functions`. Nothing is dropped
/// without one of those numbers moving.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CognitiveReport {
    pub files_scanned: usize,
    pub files_analyzed: usize,
    /// Every function/method scored, before filtering or ranking cut-offs.
    pub functions_analyzed: usize,
    /// Functions clearing `threshold`, before the `top` cut-off.
    pub functions_total: usize,
    /// Length of `functions`.
    pub returned: usize,
    /// True when `functions_total > returned`.
    pub truncated: bool,
    /// Echo of the active `--max-cognitive` filter, if any.
    pub threshold: Option<u32>,
    /// Echo of the active `--top` limit, if any.
    pub top: Option<usize>,
    /// Sum of cognitive complexity across everything scored.
    pub total_cognitive: u32,
    /// Highest cognitive complexity found (not just among returned items).
    pub max_cognitive: u32,
    pub bands: BandCounts,
    /// Score distribution across everything scored, for threshold calibration.
    pub percentiles: Percentiles,
    /// Ranked refactoring targets: highest cognitive complexity first.
    pub functions: Vec<FunctionComplexity>,
    pub failures: Vec<FileFailure>,
    /// Disclosures about how the scan was narrowed.
    pub warnings: Vec<String>,
}

impl CognitiveReport {
    /// False when the scan produced no analysis at all and should not be
    /// presented to a caller as a clean result.
    pub fn is_trustworthy(&self) -> bool {
        self.files_analyzed > 0 || self.files_scanned == 0
    }

    /// Rank the scored functions: worst first, ties broken deterministically by
    /// name, then file, then line so repeated runs serialize identically.
    pub fn rank(functions: &mut [FunctionComplexity]) {
        functions.sort_by(|a, b| {
            b.cognitive
                .cmp(&a.cognitive)
                .then_with(|| a.name.cmp(&b.name))
                .then_with(|| a.file.cmp(&b.file))
                .then_with(|| a.line.cmp(&b.line))
        });
    }
}

/// A one-line summary suitable for logs and hints.
///
/// Used by callers that want a compact human-readable digest; the structured
/// fields remain the contract for machine consumers.
pub fn summarize(report: &CognitiveReport) -> String {
    format!(
        "{} functions across {} files: max cognitive {} ({} high, {} severe)",
        report.functions_analyzed,
        report.files_analyzed,
        report.max_cognitive,
        report.bands.high,
        report.bands.severe
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summarize_reports_the_population_and_hot_bands() {
        let report = CognitiveReport {
            files_scanned: 1,
            files_analyzed: 1,
            functions_analyzed: 4,
            functions_total: 4,
            returned: 4,
            truncated: false,
            threshold: None,
            top: None,
            total_cognitive: 30,
            max_cognitive: 20,
            bands: BandCounts {
                low: 2,
                moderate: 0,
                high: 1,
                severe: 1,
            },
            percentiles: Percentiles::default(),
            functions: Vec::new(),
            failures: Vec::new(),
            warnings: Vec::new(),
        };
        let text = summarize(&report);
        assert!(text.contains("4 functions across 1 files"), "{text}");
        assert!(text.contains("max cognitive 20"), "{text}");
        assert!(text.contains("1 high, 1 severe"), "{text}");
    }

    #[test]
    fn ranking_is_worst_first() {
        let mut items = vec![
            FunctionComplexity {
                file: "a.rs".into(),
                name: "small".into(),
                kind: FunctionKind::Function,
                line: 1,
                lines: 2,
                cognitive: 1,
                nesting: 1,
                band: Band::Low,
                is_test: false,
            },
            FunctionComplexity {
                file: "a.rs".into(),
                name: "big".into(),
                kind: FunctionKind::Function,
                line: 10,
                lines: 40,
                cognitive: 31,
                nesting: 4,
                band: Band::Severe,
                is_test: false,
            },
        ];
        CognitiveReport::rank(&mut items);
        assert_eq!(items[0].name, "big");
    }
}
