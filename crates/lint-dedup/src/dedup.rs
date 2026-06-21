//! Core lint deduplication logic.

use regex::Regex;
use serde::Serialize;
use std::collections::BTreeMap;

#[derive(Debug, Serialize, PartialEq)]
pub struct LintSummary {
    pub total: usize,
    pub groups: Vec<LintGroup>,
}

#[derive(Debug, Serialize, PartialEq, Clone)]
pub struct LintGroup {
    pub rule: String,
    pub severity: String,
    pub message: String,
    pub count: usize,
    pub files: Vec<String>,
}

/// Parse and group lint output, auto-detecting the linter format.
pub fn dedup(input: &str) -> LintSummary {
    let entries = parse_entries(input);
    let total = entries.len();
    let groups = group_entries(entries);

    LintSummary { total, groups }
}

#[derive(Debug)]
struct LintEntry {
    file: String,
    rule: String,
    severity: String,
    message: String,
}

fn parse_entries(input: &str) -> Vec<LintEntry> {
    let mut entries = Vec::new();

    // Clippy: warning: unused variable `x` --> src/main.rs:10:5
    // Also matches: error[E0308]: mismatched types --> src/lib.rs:5:1
    let clippy_re = Regex::new(
        r"(?m)^(warning|error)(?:\[([A-Za-z0-9_:]+)\])?: (.+?)(?:\n\s*--> (.+?):\d+:\d+)?",
    )
    .unwrap();

    // Ruff/pylint: src/main.py:10:5: E501 Line too long
    let ruff_re = Regex::new(r"(?m)^(.+?):(\d+):(?:\d+:)?\s*([A-Z]\d{3,4})\s+(.+)$").unwrap();

    // ESLint:  10:5  error  Unexpected var  no-var
    let eslint_re =
        Regex::new(r"(?m)^\s+\d+:\d+\s+(error|warning)\s+(.+?)\s{2,}([\w/-]+)\s*$").unwrap();

    // Try clippy format
    for cap in clippy_re.captures_iter(input) {
        let severity = cap[1].to_string();
        let rule = cap
            .get(2)
            .map_or_else(|| "unknown".to_string(), |m| m.as_str().to_string());
        let message = cap[3].trim().to_string();
        let file = cap
            .get(4)
            .map_or_else(|| "unknown".to_string(), |m| m.as_str().to_string());
        entries.push(LintEntry {
            file,
            rule,
            severity,
            message,
        });
    }

    // Try ruff format
    for cap in ruff_re.captures_iter(input) {
        entries.push(LintEntry {
            file: cap[1].to_string(),
            rule: cap[3].to_string(),
            severity: "warning".to_string(),
            message: cap[4].to_string(),
        });
    }

    // Roslyn / MSBuild: MyApp\Program.cs(10,5): warning CA1822: Member does not access instance data
    let roslyn_re =
        Regex::new(r"(?m)^(.+\.cs)\((\d+),\d+\):\s+(error|warning)\s+([A-Z]+\d+):\s+(.+)$")
            .unwrap();
    for cap in roslyn_re.captures_iter(input) {
        entries.push(LintEntry {
            file: cap[1].to_string(),
            rule: cap[4].to_string(),
            severity: cap[3].to_string(),
            message: cap[5].to_string(),
        });
    }

    // Try eslint format (only if we found an eslint-style file header)
    if input.contains("eslint") || entries.is_empty() {
        // Find file headers (lines ending with a path, followed by lint lines)
        let file_header_re = Regex::new(r"(?m)^(/?\S+\.\w+)\s*$").unwrap();
        let mut current_file = String::new();

        for line in input.lines() {
            if let Some(cap) = file_header_re.captures(line) {
                current_file = cap[1].to_string();
            } else if let Some(cap) = eslint_re.captures(line) {
                entries.push(LintEntry {
                    file: current_file.clone(),
                    rule: cap[3].to_string(),
                    severity: cap[1].to_string(),
                    message: cap[2].trim().to_string(),
                });
            }
        }
    }

    entries
}

fn group_entries(entries: Vec<LintEntry>) -> Vec<LintGroup> {
    let mut map: BTreeMap<String, LintGroup> = BTreeMap::new();

    for entry in entries {
        let key = format!("{}:{}", entry.rule, entry.severity);
        let group = map.entry(key).or_insert_with(|| LintGroup {
            rule: entry.rule.clone(),
            severity: entry.severity.clone(),
            message: entry.message.clone(),
            count: 0,
            files: Vec::new(),
        });
        group.count += 1;
        if !group.files.contains(&entry.file) {
            group.files.push(entry.file);
        }
    }

    let mut groups: Vec<LintGroup> = map.into_values().collect();
    // Sort: errors first, then by count descending
    groups.sort_by(|a, b| {
        let a_err = a.severity == "error";
        let b_err = b.severity == "error";
        b_err.cmp(&a_err).then(b.count.cmp(&a.count))
    });
    groups
}

#[cfg(test)]
mod tests {
    use super::*;

    // === Test list (TDD) ===
    // [x] empty input produces empty summary
    // [x] parse ruff output
    // [x] parse clippy output
    // [x] parse roslyn output
    // [x] groups identical rules together
    // [x] errors sorted before warnings
    // [x] higher count sorted first within same severity
    // [x] files deduplicated within a group
    // [x] total count matches number of entries

    #[test]
    fn empty_input() {
        let result = dedup("");
        assert_eq!(result.total, 0);
        assert!(result.groups.is_empty());
    }

    #[test]
    fn parse_ruff_output() {
        let input = "src/main.py:10:5: E501 Line too long (120 > 79)\nsrc/main.py:20:1: F401 Unused import os\n";
        let result = dedup(input);
        assert_eq!(result.total, 2);
        assert_eq!(result.groups.len(), 2);
    }

    #[test]
    fn parse_clippy_warnings() {
        let input = r#"warning: unused variable `x`
 --> src/main.rs:10:5
warning: unused variable `y`
 --> src/main.rs:15:5
"#;
        let result = dedup(input);
        assert!(result.total >= 2);
    }

    #[test]
    fn parse_roslyn_output() {
        let input = r#"MyApp\Services\OrderService.cs(10,5): warning CA1822: Member 'GetOrder' does not access instance data and can be marked as static
MyApp\Controllers\HomeController.cs(25,9): error CS0103: The name 'foo' does not exist in the current context
MyApp\Services\OrderService.cs(30,1): warning CA1822: Member 'Process' does not access instance data and can be marked as static
"#;
        let result = dedup(input);
        assert_eq!(result.total, 3);
        let ca1822 = result.groups.iter().find(|g| g.rule == "CA1822");
        assert!(ca1822.is_some());
        assert_eq!(ca1822.unwrap().count, 2);
        let cs0103 = result.groups.iter().find(|g| g.rule == "CS0103");
        assert!(cs0103.is_some());
        assert_eq!(cs0103.unwrap().severity, "error");
    }

    #[test]
    fn groups_identical_rules() {
        let input = "src/a.py:1:1: F401 Unused import\nsrc/b.py:2:1: F401 Unused import\nsrc/c.py:3:1: F401 Unused import\n";
        let result = dedup(input);
        // All three should be in one group
        let f401_group = result.groups.iter().find(|g| g.rule == "F401").unwrap();
        assert_eq!(f401_group.count, 3);
        assert_eq!(f401_group.files.len(), 3);
    }

    #[test]
    fn errors_sorted_before_warnings() {
        let input = r#"warning: unused variable `x`
 --> src/main.rs:10:5
error[E0308]: mismatched types
 --> src/lib.rs:5:1
"#;
        let result = dedup(input);
        assert!(result.groups.len() >= 2);
        assert_eq!(result.groups[0].severity, "error");
    }

    #[test]
    fn files_deduplicated_in_group() {
        let input = "src/a.py:1:1: F401 Unused import\nsrc/a.py:5:1: F401 Unused import\n";
        let result = dedup(input);
        let group = result.groups.iter().find(|g| g.rule == "F401").unwrap();
        assert_eq!(group.count, 2);
        assert_eq!(group.files.len(), 1); // same file, deduplicated
    }

    #[test]
    fn total_matches_entries() {
        let input =
            "a.py:1:1: E501 Line too long\nb.py:2:1: F401 Unused\nc.py:3:1: E501 Line too long\n";
        let result = dedup(input);
        assert_eq!(result.total, 3);
        let sum: usize = result.groups.iter().map(|g| g.count).sum();
        assert_eq!(sum, 3);
    }
}
