//! Core log distillation logic.

use regex::Regex;
use serde::Serialize;

#[derive(Debug, Serialize, PartialEq)]
pub struct DistilledLog {
    pub total_lines: usize,
    pub kept_lines: usize,
    pub errors: Vec<LogEntry>,
    pub warnings: Vec<LogEntry>,
}

#[derive(Debug, Serialize, PartialEq, Clone)]
pub struct LogEntry {
    pub line_number: usize,
    pub text: String,
    pub context: Vec<String>,
}

/// Distill a log, extracting only actionable lines (errors/warnings) with context.
pub fn distill(input: &str, context_lines: usize) -> DistilledLog {
    let cleaned = strip_ansi(input);
    let lines: Vec<&str> = cleaned.lines().collect();
    let total_lines = lines.len();

    let mut errors = Vec::new();
    let mut warnings = Vec::new();

    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        if is_noise(trimmed) {
            continue;
        }
        if is_error(trimmed) {
            errors.push(make_entry(i, trimmed, &lines, context_lines));
        } else if is_warning(trimmed) {
            warnings.push(make_entry(i, trimmed, &lines, context_lines));
        }
    }

    DistilledLog {
        total_lines,
        kept_lines: errors.len() + warnings.len(),
        errors,
        warnings,
    }
}

/// Strip ANSI escape codes from input.
pub fn strip_ansi(input: &str) -> String {
    let re = Regex::new(r"\x1b\[[0-9;]*[a-zA-Z]").unwrap();
    re.replace_all(input, "").to_string()
}

fn is_noise(line: &str) -> bool {
    // Progress bars
    if line.contains("[==") || line.contains("[##") || line.contains("...") && line.ends_with('%') {
        return true;
    }
    // Timestamp-only lines
    let ts_re = Regex::new(r"^\d{4}-\d{2}-\d{2}[T ]\d{2}:\d{2}:\d{2}\s*$").unwrap();
    if ts_re.is_match(line) {
        return true;
    }
    // Blank or whitespace-only
    if line.trim().is_empty() {
        return true;
    }
    false
}

fn is_error(line: &str) -> bool {
    let lower = line.to_lowercase();
    lower.contains("error")
        || lower.contains("failed")
        || lower.contains("panic")
        || lower.contains("fatal")
        || lower.contains("exception")
        || lower.starts_with("e ")
        || lower.starts_with("error[")
}

fn is_warning(line: &str) -> bool {
    let lower = line.to_lowercase();
    lower.contains("warning") || lower.contains("warn") || lower.starts_with("w ")
}

fn make_entry(idx: usize, text: &str, lines: &[&str], context: usize) -> LogEntry {
    let start = idx.saturating_sub(context);
    let end = (idx + context + 1).min(lines.len());

    let ctx: Vec<String> = (start..end)
        .filter(|&j| j != idx)
        .map(|j| strip_timestamp(lines[j]).to_string())
        .filter(|l| !l.trim().is_empty())
        .collect();

    LogEntry {
        line_number: idx + 1,
        text: text.to_string(),
        context: ctx,
    }
}

fn strip_timestamp(line: &str) -> &str {
    // Strip common timestamp prefixes like "2024-01-01T12:00:00 " or "[12:00:00] "
    let ts_re = Regex::new(
        r"^(\d{4}-\d{2}-\d{2}[T ]\d{2}:\d{2}:\d{2}[\.,]?\d*\s*|^\[\d{2}:\d{2}:\d{2}\]\s*)",
    )
    .unwrap();
    if let Some(m) = ts_re.find(line) {
        &line[m.end()..]
    } else {
        line
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // === Test list (TDD) ===
    // [x] strip ANSI codes
    // [x] empty input produces empty result
    // [x] extracts error lines
    // [x] extracts warning lines
    // [x] skips progress bars
    // [x] skips blank lines
    // [x] includes context lines around errors
    // [x] strips timestamps from context
    // [x] handles mixed errors and warnings

    #[test]
    fn strip_ansi_removes_escape_codes() {
        let input = "\x1b[31merror\x1b[0m: something failed";
        assert_eq!(strip_ansi(input), "error: something failed");
    }

    #[test]
    fn strip_ansi_preserves_plain_text() {
        assert_eq!(strip_ansi("hello world"), "hello world");
    }

    #[test]
    fn empty_input() {
        let result = distill("", 1);
        assert_eq!(result.total_lines, 0);
        assert!(result.errors.is_empty());
        assert!(result.warnings.is_empty());
    }

    #[test]
    fn extracts_error_lines() {
        let input = "compiling foo\nerror[E0308]: mismatched types\nfinished";
        let result = distill(input, 0);
        assert_eq!(result.errors.len(), 1);
        assert!(result.errors[0].text.contains("mismatched types"));
    }

    #[test]
    fn extracts_warning_lines() {
        let input = "compiling bar\nwarning: unused variable\ndone";
        let result = distill(input, 0);
        assert_eq!(result.warnings.len(), 1);
        assert!(result.warnings[0].text.contains("unused variable"));
    }

    #[test]
    fn skips_progress_bars() {
        let input = "[======>    ] 60%\nerror: build failed\n[========== ] 100%";
        let result = distill(input, 0);
        assert_eq!(result.errors.len(), 1);
        assert_eq!(result.warnings.len(), 0);
    }

    #[test]
    fn includes_context_lines() {
        let input = "line one\nline two\nerror: something broke\nline four\nline five";
        let result = distill(input, 1);
        assert_eq!(result.errors.len(), 1);
        assert!(!result.errors[0].context.is_empty());
    }

    #[test]
    fn strips_timestamps_from_context() {
        let input = "2024-01-01T12:00:00 setup done\nerror: crash\n2024-01-01T12:00:01 cleanup";
        let result = distill(input, 1);
        let ctx = &result.errors[0].context;
        // Context should not start with timestamps
        for c in ctx {
            assert!(!c.starts_with("2024-"));
        }
    }

    #[test]
    fn mixed_errors_and_warnings() {
        let input = "warning: deprecated API\nerror: compilation failed\nwarning: unused import";
        let result = distill(input, 0);
        assert_eq!(result.errors.len(), 1);
        assert_eq!(result.warnings.len(), 2);
    }
}
