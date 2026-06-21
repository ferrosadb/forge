//! Core log monitoring logic.
//!
//! Analyzes log content and produces a structured report of actionable events.
//! Works on complete log snapshots (not streaming) — designed for piping log
//! files or command output through the tool.

use regex::Regex;
use serde::Serialize;
use std::collections::HashMap;

#[derive(Debug, Serialize, PartialEq)]
pub struct MonitorReport {
    pub total_lines: usize,
    pub status: LogStatus,
    pub events: Vec<LogEvent>,
    pub repeated_lines: Vec<RepeatedLine>,
    pub summary: MonitorSummary,
}

#[derive(Debug, Serialize, PartialEq)]
pub struct MonitorSummary {
    pub error_count: usize,
    pub warning_count: usize,
    pub stall_detected: bool,
    pub completion_detected: bool,
    pub resource_warnings: usize,
}

#[derive(Debug, Serialize, PartialEq, Clone)]
pub enum LogStatus {
    /// Process appears healthy — no errors, no stalls
    Healthy,
    /// Errors detected but process may still be running
    Errors,
    /// Process appears stalled (repeated identical output or long gaps)
    Stalled,
    /// Process completed (success markers detected)
    Completed,
    /// Process failed (failure markers detected)
    Failed,
    /// Resource issue detected (OOM, disk full, etc.)
    ResourceWarning,
}

#[derive(Debug, Serialize, PartialEq, Clone)]
pub struct LogEvent {
    pub line_number: usize,
    pub kind: EventKind,
    pub text: String,
}

#[derive(Debug, Serialize, PartialEq, Clone)]
pub enum EventKind {
    Error,
    Warning,
    Completion,
    Failure,
    ResourceWarning,
    Timeout,
    Stall,
}

#[derive(Debug, Serialize, PartialEq)]
pub struct RepeatedLine {
    pub text: String,
    pub count: usize,
    pub first_seen: usize,
    pub last_seen: usize,
}

#[derive(Debug, Clone)]
pub struct MonitorConfig {
    /// Number of consecutive identical lines to flag as a stall
    pub stall_threshold: usize,
    /// Minimum repeat count to report a repeated line
    pub repeat_threshold: usize,
    /// Maximum number of events to report (keeps most recent)
    pub max_events: usize,
}

impl Default for MonitorConfig {
    fn default() -> Self {
        Self {
            stall_threshold: 5,
            repeat_threshold: 3,
            max_events: 50,
        }
    }
}

/// Analyze a log for actionable events.
pub fn analyze(input: &str, config: &MonitorConfig) -> MonitorReport {
    let ansi_re = Regex::new(r"\x1b\[[0-9;]*[a-zA-Z]").unwrap();
    let cleaned = ansi_re.replace_all(input, "");
    let lines: Vec<&str> = cleaned.lines().collect();
    let total_lines = lines.len();

    let mut events = Vec::new();
    let mut line_counts: HashMap<String, (usize, usize, usize)> = HashMap::new(); // text -> (count, first, last)
    let mut consecutive_same = 0;
    let mut prev_line = "";
    let mut stall_detected = false;

    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        // Track consecutive identical lines
        if trimmed == prev_line {
            consecutive_same += 1;
            if consecutive_same >= config.stall_threshold && !stall_detected {
                stall_detected = true;
                events.push(LogEvent {
                    line_number: i + 1,
                    kind: EventKind::Stall,
                    text: format!(
                        "Stall detected: line repeated {} times: {}",
                        consecutive_same,
                        truncate(trimmed, 80)
                    ),
                });
            }
        } else {
            consecutive_same = 1;
        }
        prev_line = trimmed;

        // Track repeated lines
        let entry = line_counts
            .entry(trimmed.to_string())
            .or_insert((0, i + 1, i + 1));
        entry.0 += 1;
        entry.2 = i + 1;

        // Classify line
        if let Some(kind) = classify_line(trimmed) {
            events.push(LogEvent {
                line_number: i + 1,
                kind,
                text: truncate(trimmed, 200).to_string(),
            });
        }
    }

    // Trim events to max
    if events.len() > config.max_events {
        let skip = events.len() - config.max_events;
        events = events.into_iter().skip(skip).collect();
    }

    // Build repeated lines report
    let mut repeated_lines: Vec<RepeatedLine> = line_counts
        .into_iter()
        .filter(|(_, (count, _, _))| *count >= config.repeat_threshold)
        .map(|(text, (count, first, last))| RepeatedLine {
            text: truncate(&text, 100).to_string(),
            count,
            first_seen: first,
            last_seen: last,
        })
        .collect();
    repeated_lines.sort_by_key(|r| std::cmp::Reverse(r.count));

    // Compute summary
    let error_count = events
        .iter()
        .filter(|e| matches!(e.kind, EventKind::Error))
        .count();
    let warning_count = events
        .iter()
        .filter(|e| matches!(e.kind, EventKind::Warning))
        .count();
    let completion_detected = events
        .iter()
        .any(|e| matches!(e.kind, EventKind::Completion));
    let failed = events.iter().any(|e| matches!(e.kind, EventKind::Failure));
    let resource_warnings = events
        .iter()
        .filter(|e| matches!(e.kind, EventKind::ResourceWarning | EventKind::Timeout))
        .count();

    let status = if failed {
        LogStatus::Failed
    } else if resource_warnings > 0 {
        LogStatus::ResourceWarning
    } else if stall_detected {
        LogStatus::Stalled
    } else if completion_detected {
        LogStatus::Completed
    } else if error_count > 0 {
        LogStatus::Errors
    } else {
        LogStatus::Healthy
    };

    MonitorReport {
        total_lines,
        status,
        events,
        repeated_lines,
        summary: MonitorSummary {
            error_count,
            warning_count,
            stall_detected,
            completion_detected,
            resource_warnings,
        },
    }
}

fn classify_line(line: &str) -> Option<EventKind> {
    let lower = line.to_lowercase();

    // Resource warnings (check first — more specific)
    if lower.contains("out of memory")
        || lower.contains("oom")
        || lower.contains("cannot allocate")
        || lower.contains("memory allocation failed")
    {
        return Some(EventKind::ResourceWarning);
    }
    if lower.contains("no space left on device")
        || lower.contains("disk full")
        || lower.contains("disk quota exceeded")
    {
        return Some(EventKind::ResourceWarning);
    }
    if lower.contains("timed out")
        || lower.contains("timeout")
        || lower.contains("deadline exceeded")
    {
        return Some(EventKind::Timeout);
    }
    if lower.contains("too many open files") || lower.contains("ulimit") {
        return Some(EventKind::ResourceWarning);
    }
    if lower.contains("killed") && lower.contains("signal 9") {
        return Some(EventKind::ResourceWarning);
    }

    // Failure markers (check before completion — more specific)
    if lower.starts_with("test result: failed") || lower.starts_with("test result: fail") {
        return Some(EventKind::Failure);
    }
    if lower.contains("build failed")
        || lower.contains("compilation failed")
        || lower.contains("exit code 1")
        || lower.contains("exit status 1")
    {
        return Some(EventKind::Failure);
    }
    if lower.contains("fatal error") || lower.contains("panic") || lower.contains("segfault") {
        return Some(EventKind::Failure);
    }
    if lower.contains("abort") && !lower.contains("aborted") {
        return Some(EventKind::Failure);
    }

    // Completion markers
    if lower.contains("build succeeded")
        || lower.contains("build successful")
        || lower.contains("compilation successful")
        || (lower.contains("finished") && lower.contains("target"))
    {
        return Some(EventKind::Completion);
    }
    if lower.starts_with("test result: ok") {
        return Some(EventKind::Completion);
    }
    if (lower.contains("passed") && lower.contains("failed"))
        || lower.contains("tests passed")
        || lower.contains("all tests passed")
    {
        return Some(EventKind::Completion);
    }
    if lower.contains("successfully deployed") || lower.contains("deploy complete") {
        return Some(EventKind::Completion);
    }

    // Errors
    if lower.starts_with("error") || lower.contains("error[") || lower.contains("error:") {
        return Some(EventKind::Error);
    }
    if lower.contains("exception") && !lower.contains("no exception") {
        return Some(EventKind::Error);
    }
    if lower.contains("traceback") {
        return Some(EventKind::Error);
    }

    // Warnings
    if lower.starts_with("warning") || lower.contains("warning:") {
        return Some(EventKind::Warning);
    }
    if lower.contains("deprecated") {
        return Some(EventKind::Warning);
    }

    None
}

fn truncate(s: &str, max: usize) -> &str {
    if s.len() <= max {
        s
    } else {
        // Find a valid char boundary
        let mut end = max;
        while end > 0 && !s.is_char_boundary(end) {
            end -= 1;
        }
        &s[..end]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // === Test list (TDD) ===
    // [x] empty input produces healthy status
    // [x] detects error lines
    // [x] detects warning lines
    // [x] detects completion markers (build succeeded)
    // [x] detects test completion (test result: ok)
    // [x] detects failure markers (build failed)
    // [x] detects OOM / resource warnings
    // [x] detects timeout
    // [x] detects disk full
    // [x] detects stalls (consecutive identical lines)
    // [x] tracks repeated lines with counts
    // [x] status is Failed when failure detected
    // [x] status is Completed when completion detected
    // [x] status is Stalled when stall detected
    // [x] status is ResourceWarning when resource issue detected
    // [x] strips ANSI codes before analysis
    // [x] truncates long lines
    // [x] max_events limits output

    #[test]
    fn empty_input_healthy() {
        let result = analyze("", &MonitorConfig::default());
        assert_eq!(result.status, LogStatus::Healthy);
        assert_eq!(result.total_lines, 0);
        assert!(result.events.is_empty());
    }

    #[test]
    fn detects_errors() {
        let input = "compiling...\nerror[E0308]: mismatched types\ndone";
        let result = analyze(input, &MonitorConfig::default());
        assert_eq!(result.summary.error_count, 1);
        assert_eq!(result.status, LogStatus::Errors);
    }

    #[test]
    fn detects_warnings() {
        let input = "compiling...\nwarning: unused variable\ndone";
        let result = analyze(input, &MonitorConfig::default());
        assert_eq!(result.summary.warning_count, 1);
    }

    #[test]
    fn detects_build_completion() {
        let input = "compiling foo...\nBuild succeeded";
        let result = analyze(input, &MonitorConfig::default());
        assert!(result.summary.completion_detected);
        assert_eq!(result.status, LogStatus::Completed);
    }

    #[test]
    fn detects_test_completion() {
        let input = "running 5 tests\ntest result: ok. 5 passed; 0 failed;";
        let result = analyze(input, &MonitorConfig::default());
        assert!(result.summary.completion_detected);
        assert_eq!(result.status, LogStatus::Completed);
    }

    #[test]
    fn detects_failure() {
        let input = "compiling...\nbuild failed\n";
        let result = analyze(input, &MonitorConfig::default());
        assert_eq!(result.status, LogStatus::Failed);
    }

    #[test]
    fn detects_test_failure() {
        let input = "test result: FAILED. 3 passed; 2 failed;";
        let result = analyze(input, &MonitorConfig::default());
        assert_eq!(result.status, LogStatus::Failed);
    }

    #[test]
    fn detects_oom() {
        let input = "Processing data...\nOut of memory\n";
        let result = analyze(input, &MonitorConfig::default());
        assert_eq!(result.summary.resource_warnings, 1);
        assert_eq!(result.status, LogStatus::ResourceWarning);
    }

    #[test]
    fn detects_timeout() {
        let input = "Connecting to server...\nRequest timed out\n";
        let result = analyze(input, &MonitorConfig::default());
        assert_eq!(result.summary.resource_warnings, 1);
    }

    #[test]
    fn detects_disk_full() {
        let input = "Writing output...\nNo space left on device\n";
        let result = analyze(input, &MonitorConfig::default());
        assert_eq!(result.summary.resource_warnings, 1);
        assert_eq!(result.status, LogStatus::ResourceWarning);
    }

    #[test]
    fn detects_stall() {
        let mut lines = String::new();
        for _ in 0..7 {
            lines.push_str("Waiting for connection...\n");
        }
        let config = MonitorConfig {
            stall_threshold: 5,
            ..Default::default()
        };
        let result = analyze(&lines, &config);
        assert!(result.summary.stall_detected);
        assert_eq!(result.status, LogStatus::Stalled);
    }

    #[test]
    fn tracks_repeated_lines() {
        let input = "line A\nline B\nline A\nline B\nline A\nline B\nline A\n";
        let config = MonitorConfig {
            repeat_threshold: 3,
            ..Default::default()
        };
        let result = analyze(input, &config);
        assert!(!result.repeated_lines.is_empty());
        let a = result
            .repeated_lines
            .iter()
            .find(|r| r.text == "line A")
            .unwrap();
        assert_eq!(a.count, 4);
    }

    #[test]
    fn failure_overrides_completion() {
        // If both completion and failure markers exist, failure wins
        let input = "test result: ok. 5 passed; 0 failed;\nbuild failed";
        let result = analyze(input, &MonitorConfig::default());
        assert_eq!(result.status, LogStatus::Failed);
    }

    #[test]
    fn strips_ansi_before_analysis() {
        let input = "\x1b[31merror: something broke\x1b[0m\n";
        let result = analyze(input, &MonitorConfig::default());
        assert_eq!(result.summary.error_count, 1);
    }

    #[test]
    fn max_events_limits_output() {
        let mut input = String::new();
        for i in 0..100 {
            input.push_str(&format!("error: problem {i}\n"));
        }
        let config = MonitorConfig {
            max_events: 10,
            ..Default::default()
        };
        let result = analyze(&input, &config);
        assert!(result.events.len() <= 10);
    }

    #[test]
    fn detects_panics_as_failure() {
        let input = "thread 'main' panicked at 'index out of bounds'\n";
        let result = analyze(input, &MonitorConfig::default());
        assert_eq!(result.status, LogStatus::Failed);
    }

    #[test]
    fn detects_segfault_as_failure() {
        let input = "Segfault at address 0x0000\n";
        let result = analyze(input, &MonitorConfig::default());
        assert_eq!(result.status, LogStatus::Failed);
    }

    #[test]
    fn detects_python_traceback() {
        let input =
            "Traceback (most recent call last):\n  File \"app.py\", line 10\nValueError: invalid\n";
        let result = analyze(input, &MonitorConfig::default());
        assert!(result.summary.error_count >= 1);
    }

    #[test]
    fn detects_deprecation_warning() {
        let input = "DeprecationWarning: method foo() is deprecated\n";
        let result = analyze(input, &MonitorConfig::default());
        assert_eq!(result.summary.warning_count, 1);
    }

    #[test]
    fn detects_deploy_completion() {
        let input = "Deploying v1.2.3...\nSuccessfully deployed\n";
        let result = analyze(input, &MonitorConfig::default());
        assert!(result.summary.completion_detected);
    }
}
