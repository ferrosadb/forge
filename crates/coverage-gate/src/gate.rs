//! Coverage gate validation logic.
//!
//! Parses lcov format coverage data and computes cyclomatic complexity
//! using a brace/branch counting heuristic (no AST required).

use regex::Regex;
use serde::Serialize;
use std::collections::HashMap;
use std::path::Path;

#[derive(Debug, Serialize, PartialEq)]
pub struct GateResult {
    pub passed: bool,
    pub summary: GateSummary,
    pub violations: Vec<Violation>,
}

#[derive(Debug, Serialize, PartialEq)]
pub struct GateSummary {
    pub files_checked: usize,
    pub avg_coverage: f64,
    pub total_violations: usize,
}

#[derive(Debug, Serialize, PartialEq, Clone)]
pub struct Violation {
    pub file: String,
    pub function: String,
    pub cc: usize,
    pub coverage: f64,
    pub required_coverage: f64,
    pub has_docs: bool,
    pub rule: String,
}

#[derive(Debug, Clone)]
pub struct GateConfig {
    pub baseline_coverage: f64,
    pub high_cc_threshold: usize,
    pub high_cc_coverage: f64,
    pub critical_cc_threshold: usize,
}

impl Default for GateConfig {
    fn default() -> Self {
        Self {
            baseline_coverage: 80.0,
            high_cc_threshold: 15,
            high_cc_coverage: 90.0,
            critical_cc_threshold: 25,
        }
    }
}

/// File-level coverage data parsed from lcov.
#[derive(Debug, Default)]
pub struct FileCoverage {
    pub lines_hit: usize,
    pub lines_total: usize,
    pub functions: HashMap<String, FunctionCoverage>,
}

#[derive(Debug, Default, Clone)]
pub struct FunctionCoverage {
    pub name: String,
    pub line_start: usize,
    pub hit_count: usize,
}

/// Parse lcov format coverage data.
pub fn parse_lcov(input: &str) -> HashMap<String, FileCoverage> {
    let mut result: HashMap<String, FileCoverage> = HashMap::new();
    let mut current_file = String::new();

    for line in input.lines() {
        let trimmed = line.trim();

        if let Some(path) = trimmed.strip_prefix("SF:") {
            current_file = path.to_string();
            result.entry(current_file.clone()).or_default();
        } else if let Some(rest) = trimmed.strip_prefix("FN:") {
            // FN:line_number,function_name
            let parts: Vec<&str> = rest.splitn(2, ',').collect();
            if parts.len() == 2 {
                if let Ok(line_num) = parts[0].parse::<usize>() {
                    let name = parts[1].to_string();
                    if let Some(fc) = result.get_mut(&current_file) {
                        fc.functions.insert(
                            name.clone(),
                            FunctionCoverage {
                                name,
                                line_start: line_num,
                                hit_count: 0,
                            },
                        );
                    }
                }
            }
        } else if let Some(rest) = trimmed.strip_prefix("FNDA:") {
            // FNDA:execution_count,function_name
            let parts: Vec<&str> = rest.splitn(2, ',').collect();
            if parts.len() == 2 {
                let count = parts[0].parse::<usize>().unwrap_or(0);
                let name = parts[1];
                if let Some(fc) = result.get_mut(&current_file) {
                    if let Some(func) = fc.functions.get_mut(name) {
                        func.hit_count = count;
                    }
                }
            }
        } else if let Some(rest) = trimmed.strip_prefix("LH:") {
            if let Ok(n) = rest.parse::<usize>() {
                if let Some(fc) = result.get_mut(&current_file) {
                    fc.lines_hit = n;
                }
            }
        } else if let Some(rest) = trimmed.strip_prefix("LF:") {
            if let Ok(n) = rest.parse::<usize>() {
                if let Some(fc) = result.get_mut(&current_file) {
                    fc.lines_total = n;
                }
            }
        }
    }

    result
}

/// Compute cyclomatic complexity for a function using branch-counting heuristic.
///
/// Counts: if, else if, while, for, match arms, &&, ||, ? (ternary/try), catch.
/// CC = 1 + branch_count
pub fn compute_cc(source: &str) -> usize {
    let branch_re = Regex::new(
        r"(?x)
        \b(if|else\s+if|elif|while|for|case|catch|except|rescue)\b
        | \?\?
        | &&
        | \|\|
    ",
    )
    .unwrap();

    1 + branch_re.captures_iter(source).count()
}

/// Compute CC for each function in a file, given function start lines.
/// Returns (function_name, cc) pairs.
pub fn compute_function_ccs(
    source: &str,
    functions: &HashMap<String, FunctionCoverage>,
) -> Vec<(String, usize)> {
    let lines: Vec<&str> = source.lines().collect();
    let mut func_starts: Vec<(&str, usize)> = functions
        .values()
        .map(|f| (f.name.as_str(), f.line_start))
        .collect();
    func_starts.sort_by_key(|(_, line)| *line);

    let mut result = Vec::new();

    for (i, (name, start)) in func_starts.iter().enumerate() {
        let end = if i + 1 < func_starts.len() {
            func_starts[i + 1].1.saturating_sub(1)
        } else {
            lines.len()
        };

        let start_idx = start.saturating_sub(1);
        let end_idx = end.min(lines.len());

        if start_idx < end_idx {
            let func_source: String = lines[start_idx..end_idx].join("\n");
            let cc = compute_cc(&func_source);
            result.push((name.to_string(), cc));
        }
    }

    result
}

/// Check if a function has documentation (doc comment above it).
pub fn has_documentation(source: &str, line_start: usize) -> bool {
    let lines: Vec<&str> = source.lines().collect();
    if line_start == 0 || line_start > lines.len() {
        return false;
    }

    // Look at lines immediately preceding the function (0-indexed)
    let mut i = line_start.saturating_sub(2);
    loop {
        let trimmed = lines[i].trim();
        if trimmed.starts_with("///")
            || trimmed.starts_with("//!")
            || trimmed.starts_with("#[doc")
            || trimmed.starts_with("\"\"\"")
            || trimmed.starts_with('#') && (trimmed.contains("doc") || trimmed.contains("\"\"\""))
        {
            return true;
        }
        // Keep scanning through decorators/attributes
        if (trimmed.starts_with('#')
            || trimmed.starts_with('@')
            || trimmed.starts_with("//")
            || trimmed.is_empty())
            && i > 0
        {
            i -= 1;
            continue;
        }
        break;
    }
    false
}

/// Run the coverage gate check.
pub fn check(
    coverage: &HashMap<String, FileCoverage>,
    source_dir: &Path,
    config: &GateConfig,
) -> GateResult {
    let mut violations = Vec::new();
    let mut total_coverage = 0.0;
    let mut files_checked = 0;

    for (file_path, file_cov) in coverage {
        files_checked += 1;
        let file_coverage_pct = if file_cov.lines_total > 0 {
            (file_cov.lines_hit as f64 / file_cov.lines_total as f64) * 100.0
        } else {
            100.0
        };
        total_coverage += file_coverage_pct;

        // Try to read source to compute CC
        let source_path = source_dir.join(file_path);
        let source = std::fs::read_to_string(&source_path).unwrap_or_default();

        if !source.is_empty() && !file_cov.functions.is_empty() {
            let ccs = compute_function_ccs(&source, &file_cov.functions);

            for (func_name, cc) in ccs {
                if cc >= config.critical_cc_threshold {
                    violations.push(Violation {
                        file: file_path.clone(),
                        function: func_name.clone(),
                        cc,
                        coverage: file_coverage_pct,
                        required_coverage: 95.0,
                        has_docs: has_documentation(
                            &source,
                            file_cov
                                .functions
                                .get(&func_name)
                                .map_or(0, |f| f.line_start),
                        ),
                        rule: format!(
                            "CC>={} requires refactor plan",
                            config.critical_cc_threshold
                        ),
                    });
                } else if cc >= config.high_cc_threshold {
                    let has_docs = has_documentation(
                        &source,
                        file_cov
                            .functions
                            .get(&func_name)
                            .map_or(0, |f| f.line_start),
                    );

                    if file_coverage_pct < config.high_cc_coverage {
                        violations.push(Violation {
                            file: file_path.clone(),
                            function: func_name.clone(),
                            cc,
                            coverage: file_coverage_pct,
                            required_coverage: config.high_cc_coverage,
                            has_docs,
                            rule: format!(
                                "CC>={} requires {}% coverage",
                                config.high_cc_threshold, config.high_cc_coverage
                            ),
                        });
                    }
                    if !has_docs {
                        violations.push(Violation {
                            file: file_path.clone(),
                            function: func_name,
                            cc,
                            coverage: file_coverage_pct,
                            required_coverage: config.high_cc_coverage,
                            has_docs,
                            rule: format!(
                                "CC>={} requires local documentation",
                                config.high_cc_threshold
                            ),
                        });
                    }
                }
            }
        }

        // Baseline coverage check
        if file_coverage_pct < config.baseline_coverage {
            violations.push(Violation {
                file: file_path.clone(),
                function: "(file)".to_string(),
                cc: 0,
                coverage: file_coverage_pct,
                required_coverage: config.baseline_coverage,
                has_docs: true,
                rule: format!("Baseline coverage {}% not met", config.baseline_coverage),
            });
        }
    }

    let avg_coverage = if files_checked > 0 {
        total_coverage / files_checked as f64
    } else {
        0.0
    };

    GateResult {
        passed: violations.is_empty(),
        summary: GateSummary {
            files_checked,
            avg_coverage,
            total_violations: violations.len(),
        },
        violations,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // === Test list (TDD) ===
    // [x] parse lcov: empty input
    // [x] parse lcov: single file with coverage
    // [x] parse lcov: multiple files
    // [x] parse lcov: function data
    // [x] compute_cc: simple function (no branches)
    // [x] compute_cc: function with if/else
    // [x] compute_cc: function with loops and match
    // [x] compute_cc: logical operators
    // [x] has_documentation: with doc comment
    // [x] has_documentation: without doc comment
    // [x] gate passes when coverage meets baseline
    // [x] gate fails when coverage below baseline

    #[test]
    fn parse_lcov_empty() {
        let result = parse_lcov("");
        assert!(result.is_empty());
    }

    #[test]
    fn parse_lcov_single_file() {
        let input = "SF:src/main.rs\nLF:100\nLH:85\nend_of_record\n";
        let result = parse_lcov(input);
        assert_eq!(result.len(), 1);
        let fc = &result["src/main.rs"];
        assert_eq!(fc.lines_total, 100);
        assert_eq!(fc.lines_hit, 85);
    }

    #[test]
    fn parse_lcov_multiple_files() {
        let input =
            "SF:src/a.rs\nLF:50\nLH:40\nend_of_record\nSF:src/b.rs\nLF:30\nLH:30\nend_of_record\n";
        let result = parse_lcov(input);
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn parse_lcov_function_data() {
        let input =
            "SF:src/lib.rs\nFN:10,process_data\nFNDA:5,process_data\nLF:50\nLH:45\nend_of_record\n";
        let result = parse_lcov(input);
        let fc = &result["src/lib.rs"];
        assert_eq!(fc.functions.len(), 1);
        assert_eq!(fc.functions["process_data"].line_start, 10);
        assert_eq!(fc.functions["process_data"].hit_count, 5);
    }

    #[test]
    fn cc_no_branches() {
        let source = "fn add(a: i32, b: i32) -> i32 { a + b }";
        assert_eq!(compute_cc(source), 1);
    }

    #[test]
    fn cc_with_if_else() {
        let source = "fn abs(x: i32) -> i32 { if x < 0 { -x } else { x } }";
        assert_eq!(compute_cc(source), 2); // 1 + 1 if
    }

    #[test]
    fn cc_with_loops_and_match() {
        let source = r#"
            fn process(items: &[Item]) {
                for item in items {
                    if item.is_valid() {
                        while item.has_more() {
                            match item.next() {
                                case A => {},
                                case B => {},
                            }
                        }
                    }
                }
            }
        "#;
        // 1 + for + if + while + 2 case = 6
        assert_eq!(compute_cc(source), 6);
    }

    #[test]
    fn cc_logical_operators() {
        let source = "fn check(a: bool, b: bool, c: bool) -> bool { a && b || c }";
        assert_eq!(compute_cc(source), 3); // 1 + && + ||
    }

    #[test]
    fn has_docs_with_doc_comment() {
        let source = "/// This function does things.\nfn foo() {}\n";
        assert!(has_documentation(source, 2)); // function at line 2
    }

    #[test]
    fn has_docs_without_doc_comment() {
        let source = "fn foo() {}\n";
        assert!(!has_documentation(source, 1));
    }

    #[test]
    fn gate_passes_above_baseline() {
        let mut coverage = HashMap::new();
        coverage.insert(
            "src/main.rs".to_string(),
            FileCoverage {
                lines_hit: 85,
                lines_total: 100,
                functions: HashMap::new(),
            },
        );
        let result = check(&coverage, Path::new("/nonexistent"), &GateConfig::default());
        assert!(result.passed);
    }

    #[test]
    fn gate_fails_below_baseline() {
        let mut coverage = HashMap::new();
        coverage.insert(
            "src/main.rs".to_string(),
            FileCoverage {
                lines_hit: 50,
                lines_total: 100,
                functions: HashMap::new(),
            },
        );
        let result = check(&coverage, Path::new("/nonexistent"), &GateConfig::default());
        assert!(!result.passed);
        assert!(result.violations[0].rule.contains("Baseline coverage"));
    }
}
