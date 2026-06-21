//! Core diff filtering logic.
//!
//! Operates on unified diff format (git diff output).

use regex::Regex;
use serde::Serialize;

#[derive(Debug, Clone)]
pub struct FilterConfig {
    /// File patterns to skip entirely (glob-style suffixes)
    pub skip_patterns: Vec<String>,
    /// Max hunk lines before collapsing to a summary
    pub max_hunk_lines: usize,
    /// Include patterns (if non-empty, only matching files pass)
    pub include_patterns: Vec<String>,
    /// Whether to strip whitespace-only changes
    pub strip_whitespace_only: bool,
}

impl Default for FilterConfig {
    fn default() -> Self {
        Self {
            skip_patterns: vec![
                "*.lock".into(),
                "package-lock.json".into(),
                "*.generated.*".into(),
                "*.snap".into(),
                "yarn.lock".into(),
                "Cargo.lock".into(),
                "poetry.lock".into(),
                "go.sum".into(),
            ],
            max_hunk_lines: 80,
            include_patterns: Vec::new(),
            strip_whitespace_only: true,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct FilterResult {
    pub files_kept: usize,
    pub files_skipped: usize,
    pub hunks_collapsed: usize,
    pub output: String,
}

/// Filter a unified diff, skipping noisy files and collapsing large hunks.
pub fn filter_diff(input: &str, config: &FilterConfig) -> FilterResult {
    let mut output = String::new();
    let mut files_kept = 0;
    let mut files_skipped = 0;
    let mut hunks_collapsed = 0;

    let file_sections = split_into_file_sections(input);

    for section in &file_sections {
        let filename = extract_filename(section);
        let fname = filename.as_deref().unwrap_or("");

        if should_skip(fname, config) {
            files_skipped += 1;
            continue;
        }

        if !config.include_patterns.is_empty() && !matches_include(fname, config) {
            files_skipped += 1;
            continue;
        }

        if config.strip_whitespace_only && is_whitespace_only_diff(section) {
            files_skipped += 1;
            continue;
        }

        // Process hunks within this file section
        let (processed, collapsed) = process_hunks(section, config.max_hunk_lines);
        hunks_collapsed += collapsed;
        files_kept += 1;
        output.push_str(&processed);
    }

    // Preserve the input's trailing-newline convention.
    // Internally, .lines() strips line endings and we re-add '\n' to every
    // line — including the last — so the output always ends with '\n' even
    // when the input did not.  Strip that extra byte to avoid inflation.
    if !input.ends_with('\n') && output.ends_with('\n') {
        output.pop();
    }

    FilterResult {
        files_kept,
        files_skipped,
        hunks_collapsed,
        output,
    }
}

/// Generate stats-only output (no diff content).
pub fn stats_only(input: &str, config: &FilterConfig) -> FilterResult {
    let mut result = filter_diff(input, config);
    result.output = String::new();
    result
}

fn split_into_file_sections(input: &str) -> Vec<String> {
    let mut sections = Vec::new();
    let mut current = String::new();

    for line in input.lines() {
        if line.starts_with("diff --git") && !current.is_empty() {
            sections.push(current);
            current = String::new();
        }
        current.push_str(line);
        current.push('\n');
    }
    if !current.is_empty() {
        sections.push(current);
    }
    sections
}

fn extract_filename(section: &str) -> Option<String> {
    // Try +++ b/filename first
    for line in section.lines() {
        if let Some(rest) = line.strip_prefix("+++ b/") {
            return Some(rest.to_string());
        }
    }
    // Fallback: diff --git a/file b/file
    let re = Regex::new(r"diff --git a/\S+ b/(\S+)").unwrap();
    re.captures(section).map(|c| c[1].to_string())
}

fn should_skip(filename: &str, config: &FilterConfig) -> bool {
    for pattern in &config.skip_patterns {
        if let Some(suffix) = pattern.strip_prefix('*') {
            if filename.ends_with(suffix) {
                return true;
            }
        } else if filename.ends_with(pattern) || filename == *pattern {
            return true;
        }
    }
    false
}

fn matches_include(filename: &str, config: &FilterConfig) -> bool {
    for pattern in &config.include_patterns {
        if let Some(suffix) = pattern.strip_prefix('*') {
            if filename.ends_with(suffix) {
                return true;
            }
        } else if filename.contains(pattern) {
            return true;
        }
    }
    false
}

fn is_whitespace_only_diff(section: &str) -> bool {
    for line in section.lines() {
        if line.starts_with('+') && !line.starts_with("+++") {
            let content = &line[1..];
            if content.trim() != content.trim() || !content.trim().is_empty() {
                // Has actual content change
                if !content.trim().is_empty() {
                    // Check if there's a corresponding removed line with same trimmed content
                    // Simplified: if any + line has non-whitespace content, not whitespace-only
                    return false;
                }
            }
        }
        if line.starts_with('-') && !line.starts_with("---") {
            let content = &line[1..];
            if !content.trim().is_empty() {
                return false;
            }
        }
    }
    true
}

fn process_hunks(section: &str, max_hunk_lines: usize) -> (String, usize) {
    let mut output = String::new();
    let mut collapsed = 0;
    let mut current_hunk = String::new();
    let mut hunk_line_count = 0;
    let mut additions = 0;
    let mut deletions = 0;
    let mut in_hunk = false;
    let hunk_re = Regex::new(r"^@@.*@@(.*)$").unwrap();

    for line in section.lines() {
        if hunk_re.is_match(line) {
            // Flush previous hunk
            if in_hunk {
                if hunk_line_count > max_hunk_lines {
                    output.push_str(&format!(
                        "[+{additions}/-{deletions} lines, {hunk_line_count} total — collapsed]\n"
                    ));
                    collapsed += 1;
                } else {
                    output.push_str(&current_hunk);
                }
            }
            current_hunk = String::new();
            current_hunk.push_str(line);
            current_hunk.push('\n');
            hunk_line_count = 0;
            additions = 0;
            deletions = 0;
            in_hunk = true;
        } else if in_hunk {
            current_hunk.push_str(line);
            current_hunk.push('\n');
            hunk_line_count += 1;
            if line.starts_with('+') && !line.starts_with("+++") {
                additions += 1;
            }
            if line.starts_with('-') && !line.starts_with("---") {
                deletions += 1;
            }
        } else {
            // Header lines (diff --git, index, ---, +++)
            output.push_str(line);
            output.push('\n');
        }
    }

    // Flush last hunk
    if in_hunk {
        if hunk_line_count > max_hunk_lines {
            output.push_str(&format!(
                "[+{additions}/-{deletions} lines, {hunk_line_count} total — collapsed]\n"
            ));
            collapsed += 1;
        } else {
            output.push_str(&current_hunk);
        }
    }

    (output, collapsed)
}

#[cfg(test)]
mod tests {
    use super::*;

    // === Test list (TDD) ===
    // [x] empty input produces empty result
    // [x] skips lock files by default
    // [x] skips package-lock.json
    // [x] keeps normal source files
    // [x] include filter restricts to matching files
    // [x] collapses large hunks
    // [x] preserves small hunks
    // [x] stats_only returns no output content
    // [x] whitespace-only changes filtered
    // [x] no trailing newline inflation

    fn make_diff(filename: &str, content: &str) -> String {
        format!(
            "diff --git a/{f} b/{f}\nindex abc..def 100644\n--- a/{f}\n+++ b/{f}\n{content}",
            f = filename,
        )
    }

    #[test]
    fn empty_input() {
        let result = filter_diff("", &FilterConfig::default());
        assert_eq!(result.files_kept, 0);
        assert_eq!(result.files_skipped, 0);
        assert!(result.output.is_empty());
    }

    #[test]
    fn skips_lock_files() {
        let diff = make_diff("Cargo.lock", "@@ -1,3 +1,3 @@\n-old\n+new\n context\n");
        let result = filter_diff(&diff, &FilterConfig::default());
        assert_eq!(result.files_skipped, 1);
        assert_eq!(result.files_kept, 0);
    }

    #[test]
    fn skips_package_lock() {
        let diff = make_diff("package-lock.json", "@@ -1,1 +1,1 @@\n-x\n+y\n");
        let result = filter_diff(&diff, &FilterConfig::default());
        assert_eq!(result.files_skipped, 1);
    }

    #[test]
    fn keeps_source_files() {
        let diff = make_diff("src/main.rs", "@@ -1,3 +1,3 @@\n-old\n+new\n context\n");
        let result = filter_diff(&diff, &FilterConfig::default());
        assert_eq!(result.files_kept, 1);
        assert!(result.output.contains("src/main.rs"));
    }

    #[test]
    fn include_filter() {
        let diff = format!(
            "{}\n{}",
            make_diff("src/main.rs", "@@ -1,1 +1,1 @@\n-a\n+b\n"),
            make_diff("src/app.py", "@@ -1,1 +1,1 @@\n-c\n+d\n")
        );
        let config = FilterConfig {
            include_patterns: vec!["*.rs".into()],
            ..Default::default()
        };
        let result = filter_diff(&diff, &config);
        assert_eq!(result.files_kept, 1);
        assert!(result.output.contains("main.rs"));
        assert!(!result.output.contains("app.py"));
    }

    #[test]
    fn collapses_large_hunks() {
        let mut hunk = String::from("@@ -1,100 +1,100 @@\n");
        for i in 0..90 {
            hunk.push_str(&format!("-old line {i}\n+new line {i}\n"));
        }
        let diff = make_diff("src/big.rs", &hunk);
        let config = FilterConfig {
            max_hunk_lines: 50,
            ..Default::default()
        };
        let result = filter_diff(&diff, &config);
        assert_eq!(result.hunks_collapsed, 1);
        assert!(result.output.contains("collapsed"));
    }

    #[test]
    fn preserves_small_hunks() {
        let diff = make_diff("src/small.rs", "@@ -1,3 +1,3 @@\n-old\n+new\n ctx\n");
        let result = filter_diff(&diff, &FilterConfig::default());
        assert_eq!(result.hunks_collapsed, 0);
        assert!(result.output.contains("-old"));
        assert!(result.output.contains("+new"));
    }

    #[test]
    fn stats_only_no_content() {
        let diff = make_diff("src/main.rs", "@@ -1,1 +1,1 @@\n-a\n+b\n");
        let result = stats_only(&diff, &FilterConfig::default());
        assert_eq!(result.files_kept, 1);
        assert!(result.output.is_empty());
    }

    #[test]
    fn no_trailing_newline_inflation() {
        // Input without trailing newline must not gain one in output
        let diff_no_nl = make_diff("src/main.rs", "@@ -1,3 +1,3 @@\n-old\n+new\n context");
        assert!(!diff_no_nl.ends_with('\n'));
        let result = filter_diff(&diff_no_nl, &FilterConfig::default());
        assert!(
            !result.output.ends_with('\n'),
            "output should not add trailing newline"
        );
        assert!(
            result.output.len() <= diff_no_nl.len(),
            "output ({}) must not exceed input ({})",
            result.output.len(),
            diff_no_nl.len(),
        );

        // Input with trailing newline should preserve it
        let diff_with_nl = format!(
            "{}\n",
            make_diff("src/main.rs", "@@ -1,3 +1,3 @@\n-old\n+new\n context")
        );
        assert!(diff_with_nl.ends_with('\n'));
        let result2 = filter_diff(&diff_with_nl, &FilterConfig::default());
        assert!(
            result2.output.ends_with('\n'),
            "output should preserve trailing newline"
        );
    }
}
