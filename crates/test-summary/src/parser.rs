//! Test output parsers for various runners.

use serde::Serialize;

#[derive(Debug, Serialize, PartialEq)]
pub struct TestSummary {
    pub runner: String,
    pub passed: usize,
    pub failed: usize,
    pub skipped: usize,
    pub duration_ms: Option<u64>,
    pub failures: Vec<TestFailure>,
}

#[derive(Debug, Serialize, PartialEq)]
pub struct TestFailure {
    pub test: String,
    pub file: Option<String>,
    pub line: Option<usize>,
    pub message: String,
}

/// Parse test runner output, auto-detecting the runner.
/// Detection order matters: more specific detectors come first to avoid
/// false matches by broader detectors (e.g. catch2 before pytest).
pub fn parse(input: &str) -> anyhow::Result<TestSummary> {
    if is_dotnet_test(input) {
        parse_dotnet_test(input)
    } else if is_cargo_test(input) {
        parse_cargo_test(input)
    } else if is_googletest(input) {
        parse_googletest(input)
    } else if is_catch2(input) {
        parse_catch2(input)
    } else if is_swift_testing(input) {
        parse_swift_testing(input)
    } else if is_xctest(input) {
        parse_xctest(input)
    } else if is_vitest(input) {
        parse_vitest(input)
    } else if is_jest(input) {
        parse_jest(input)
    } else if is_exunit(input) {
        parse_exunit(input)
    } else if is_go_test(input) {
        parse_go_test(input)
    } else if is_pytest(input) {
        parse_pytest(input)
    } else {
        anyhow::bail!("Could not detect test runner from output")
    }
}

fn is_cargo_test(input: &str) -> bool {
    input.contains("running ") && input.contains("test result:")
}

fn is_pytest(input: &str) -> bool {
    input.contains("passed")
        && (input.contains("====") || input.contains("collected") || input.contains("pytest"))
}

fn is_go_test(input: &str) -> bool {
    input.contains("--- PASS:")
        || input.contains("--- FAIL:")
        || input.contains("PASS\nok")
        || input.contains("ok  \t")
}

fn is_jest(input: &str) -> bool {
    input.contains("Tests:") && (input.contains("Suites:") || input.contains("Test Suites:"))
}

fn is_exunit(input: &str) -> bool {
    // ExUnit summary: "N tests, M failures" or "Finished in X seconds"
    // Exclude Catch2 which also has "test" and "failed" but uses "test cases:"
    !input.contains("test cases:")
        && (input.contains("tests,") || input.contains("test,"))
        && (input.contains("failure") || input.contains("Finished in"))
}

fn is_vitest(input: &str) -> bool {
    // Vitest uses "Test Files" + "Duration" or "Start at"
    input.contains("Test Files") && (input.contains("Duration") || input.contains("Start at"))
}

fn is_googletest(input: &str) -> bool {
    input.contains("[==========]") && input.contains("[  PASSED  ]")
}

fn is_catch2(input: &str) -> bool {
    input.contains("test cases:") && input.contains("assertions:")
}

fn is_swift_testing(input: &str) -> bool {
    // Swift Testing framework: "Test run with N tests completed"
    input.contains("Test run") && input.contains("tests completed")
}

fn is_xctest(input: &str) -> bool {
    // XCTest: "Executed N tests, with M failures"
    input.contains("Executed") && input.contains("tests, with") && input.contains("failure")
}

fn parse_cargo_test(input: &str) -> anyhow::Result<TestSummary> {
    use regex::Regex;

    let result_re =
        Regex::new(r"test result: \w+\.\s+(\d+) passed;\s+(\d+) failed;\s+(\d+) ignored")?;
    let failure_re = Regex::new(r"---- ([\w:]+) stdout ----")?;

    let mut passed = 0;
    let mut failed = 0;
    let mut skipped = 0;

    // Sum across all "test result:" lines (multiple test binaries)
    for cap in result_re.captures_iter(input) {
        passed += cap[1].parse::<usize>().unwrap_or(0);
        failed += cap[2].parse::<usize>().unwrap_or(0);
        skipped += cap[3].parse::<usize>().unwrap_or(0);
    }

    let mut failures = Vec::new();
    let sections: Vec<&str> = input.split("---- ").collect();
    for section in &sections[1..] {
        if let Some(cap) = failure_re.captures(&format!("---- {section}")) {
            let test_name = cap[1].to_string();
            // Extract the assertion/panic message
            let message = extract_cargo_failure_message(section);
            failures.push(TestFailure {
                test: test_name,
                file: None,
                line: None,
                message,
            });
        }
    }

    // Parse duration from "finished in X.XXs"
    let duration_re = Regex::new(r"finished in (\d+\.?\d*)s")?;
    let duration_ms = duration_re
        .captures(input)
        .map(|c| (c[1].parse::<f64>().unwrap_or(0.0) * 1000.0) as u64);

    Ok(TestSummary {
        runner: "cargo_test".to_string(),
        passed,
        failed,
        skipped,
        duration_ms,
        failures,
    })
}

fn extract_cargo_failure_message(section: &str) -> String {
    // Look for "assertion" or "panicked at" lines
    for line in section.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("assertion") || trimmed.contains("panicked at") {
            return trimmed.to_string();
        }
        if trimmed.starts_with("left:") || trimmed.starts_with("right:") {
            return trimmed.to_string();
        }
    }
    // Fallback: first non-empty line after the header
    section
        .lines()
        .skip(1)
        .find(|l| !l.trim().is_empty())
        .unwrap_or("(no message)")
        .trim()
        .to_string()
}

fn parse_pytest(input: &str) -> anyhow::Result<TestSummary> {
    use regex::Regex;

    let summary_re = Regex::new(r"(\d+) passed")?;
    let failed_re = Regex::new(r"(\d+) failed")?;
    let skipped_re = Regex::new(r"(\d+) skipped")?;
    let duration_re = Regex::new(r"in (\d+\.?\d*)s")?;

    let passed = summary_re
        .captures(input)
        .map_or(0, |c| c[1].parse().unwrap_or(0));
    let failed = failed_re
        .captures(input)
        .map_or(0, |c| c[1].parse().unwrap_or(0));
    let skipped = skipped_re
        .captures(input)
        .map_or(0, |c| c[1].parse().unwrap_or(0));
    let duration_ms = duration_re
        .captures(input)
        .map(|c| (c[1].parse::<f64>().unwrap_or(0.0) * 1000.0) as u64);

    // Parse FAILED tests
    let failure_re = Regex::new(r"FAILED ([\w/\\.]+)::([\w]+)")?;
    let mut failures = Vec::new();
    for cap in failure_re.captures_iter(input) {
        failures.push(TestFailure {
            test: cap[2].to_string(),
            file: Some(cap[1].to_string()),
            line: None,
            message: extract_pytest_failure(input, &cap[2]),
        });
    }

    Ok(TestSummary {
        runner: "pytest".to_string(),
        passed,
        failed,
        skipped,
        duration_ms,
        failures,
    })
}

fn extract_pytest_failure(input: &str, test_name: &str) -> String {
    // Find the AssertionError or relevant line near the test
    let marker = format!("_{test_name}_");
    let alt_marker = test_name;
    for line in input.lines() {
        let trimmed = line.trim();
        if (trimmed.contains(&marker) || trimmed.contains(alt_marker))
            && (trimmed.contains("assert") || trimmed.contains("Error"))
        {
            return trimmed.to_string();
        }
    }
    // Look for any AssertionError
    for line in input.lines() {
        if line.contains("AssertionError") || line.contains("assert ") {
            return line.trim().to_string();
        }
    }
    "(no message)".to_string()
}

fn parse_go_test(input: &str) -> anyhow::Result<TestSummary> {
    use regex::Regex;

    let pass_re = Regex::new(r"--- PASS: (\S+)")?;
    let fail_re = Regex::new(r"--- FAIL: (\S+)")?;
    let skip_re = Regex::new(r"--- SKIP: (\S+)")?;
    let duration_re = Regex::new(r"ok\s+\S+\s+(\d+\.?\d*)s")?;

    let passed = pass_re.captures_iter(input).count();
    let failed = fail_re.captures_iter(input).count();
    let skipped = skip_re.captures_iter(input).count();
    let duration_ms = duration_re
        .captures(input)
        .map(|c| (c[1].parse::<f64>().unwrap_or(0.0) * 1000.0) as u64);

    let mut failures = Vec::new();
    for cap in fail_re.captures_iter(input) {
        failures.push(TestFailure {
            test: cap[1].to_string(),
            file: None,
            line: None,
            message: "(see test output)".to_string(),
        });
    }

    Ok(TestSummary {
        runner: "go_test".to_string(),
        passed,
        failed,
        skipped,
        duration_ms,
        failures,
    })
}

fn parse_jest(input: &str) -> anyhow::Result<TestSummary> {
    use regex::Regex;

    let tests_re = Regex::new(
        r"Tests:\s+(?:(\d+) failed,\s+)?(?:(\d+) skipped,\s+)?(?:(\d+) passed,\s+)?(\d+) total",
    )?;
    let duration_re = Regex::new(r"Time:\s+(\d+\.?\d*)s")?;

    let (mut passed, mut failed, mut skipped) = (0, 0, 0);
    if let Some(cap) = tests_re.captures(input) {
        failed = cap.get(1).map_or(0, |m| m.as_str().parse().unwrap_or(0));
        skipped = cap.get(2).map_or(0, |m| m.as_str().parse().unwrap_or(0));
        passed = cap.get(3).map_or(0, |m| m.as_str().parse().unwrap_or(0));
    }

    let duration_ms = duration_re
        .captures(input)
        .map(|c| (c[1].parse::<f64>().unwrap_or(0.0) * 1000.0) as u64);

    // Parse FAIL sections
    let fail_re = Regex::new(r"● (.+)")?;
    let mut failures = Vec::new();
    for cap in fail_re.captures_iter(input) {
        failures.push(TestFailure {
            test: cap[1].to_string(),
            file: None,
            line: None,
            message: "(see test output)".to_string(),
        });
    }

    Ok(TestSummary {
        runner: "jest".to_string(),
        passed,
        failed,
        skipped,
        duration_ms,
        failures,
    })
}

fn parse_exunit(input: &str) -> anyhow::Result<TestSummary> {
    use regex::Regex;

    // ExUnit summary line: "4 tests, 0 failures" or "4 tests, 0 failures, 1 excluded"
    let summary_re = Regex::new(r"(\d+) tests?,\s+(\d+) failures?")?;
    let excluded_re = Regex::new(r"(\d+) excluded")?;
    let skipped_re = Regex::new(r"(\d+) skipped")?;
    // "Finished in 0.1 seconds" or "Finished in 0.03 seconds (0.02s async, 0.01s sync)"
    let duration_re = Regex::new(r"Finished in (\d+\.?\d*) seconds?")?;

    let (mut total, mut failed): (usize, usize) = (0, 0);
    if let Some(cap) = summary_re.captures(input) {
        total = cap[1].parse().unwrap_or(0);
        failed = cap[2].parse().unwrap_or(0);
    }

    let excluded = excluded_re
        .captures(input)
        .map_or(0usize, |c| c[1].parse().unwrap_or(0));
    let skipped_explicit = skipped_re
        .captures(input)
        .map_or(0usize, |c| c[1].parse().unwrap_or(0));
    let skipped = excluded + skipped_explicit;
    let passed = total.saturating_sub(failed);

    let duration_ms = duration_re
        .captures(input)
        .map(|c| (c[1].parse::<f64>().unwrap_or(0.0) * 1000.0) as u64);

    // Parse failure blocks:
    //   1) test description (Module.Test)
    //      test/path/file_test.exs:10
    let failure_re =
        Regex::new(r"(?m)^\s+\d+\)\s+test (.+?)(?:\s+\((\S+)\))?\s*\n\s+(\S+\.exs?):(\d+)")?;
    let mut failures = Vec::new();
    for cap in failure_re.captures_iter(input) {
        let test_name = cap[1].to_string();
        let file = Some(cap[3].to_string());
        let line = cap[4].parse().ok();

        // Extract the assertion message
        let message = extract_exunit_failure(input, &test_name);

        failures.push(TestFailure {
            test: test_name,
            file,
            line,
            message,
        });
    }

    Ok(TestSummary {
        runner: "exunit".to_string(),
        passed,
        failed,
        skipped,
        duration_ms,
        failures,
    })
}

fn parse_vitest(input: &str) -> anyhow::Result<TestSummary> {
    use regex::Regex;

    // Vitest summary formats:
    //   "Tests  1 failed | 3 passed | 4 total"
    //   "Tests  1 failed | 3 passed (4)"
    //   "Tests  5 passed (5)"
    let failed_re = Regex::new(r"Tests\s+.*?(\d+) failed")?;
    let passed_re = Regex::new(r"Tests\s+.*?(\d+) passed")?;
    let skipped_re = Regex::new(r"Tests\s+.*?(\d+) skipped")?;
    let todo_re = Regex::new(r"Tests\s+.*?(\d+) todo")?;
    let duration_re = Regex::new(r"Duration\s+(\d+\.?\d*)s")?;

    let (mut passed, mut failed, mut skipped) = (0, 0, 0);
    // Search for each count independently on lines starting with "Tests"
    for line in input.lines() {
        let trimmed = line.trim();
        if !trimmed.starts_with("Tests") {
            continue;
        }
        if let Some(cap) = failed_re.captures(trimmed) {
            failed = cap[1].parse().unwrap_or(0);
        }
        if let Some(cap) = passed_re.captures(trimmed) {
            passed = cap[1].parse().unwrap_or(0);
        }
        if let Some(cap) = skipped_re.captures(trimmed) {
            skipped = cap[1].parse().unwrap_or(0);
        }
        if let Some(cap) = todo_re.captures(trimmed) {
            skipped += cap[1].parse::<usize>().unwrap_or(0);
        }
        break; // Only process the first "Tests" line
    }

    let duration_ms = duration_re
        .captures(input)
        .map(|c| (c[1].parse::<f64>().unwrap_or(0.0) * 1000.0) as u64);

    // Parse failure lines: " × test name" or " ✗ test name"
    // Skip file-level markers like "× src/app.test.ts (1)" — they end with (N)
    let fail_re = Regex::new(r"(?m)^\s*[×✗]\s+(.+)")?;
    let mut failures = Vec::new();
    for cap in fail_re.captures_iter(input) {
        let name = cap[1].trim().to_string();
        // Skip file-level markers (contain file extensions or end with count)
        if name.contains(".test.") || name.contains(".spec.") || name.ends_with(')') {
            continue;
        }
        failures.push(TestFailure {
            test: name,
            file: None,
            line: None,
            message: "(see test output)".to_string(),
        });
    }

    Ok(TestSummary {
        runner: "vitest".to_string(),
        passed,
        failed,
        skipped,
        duration_ms,
        failures,
    })
}

fn parse_googletest(input: &str) -> anyhow::Result<TestSummary> {
    use regex::Regex;

    // "[  PASSED  ] 2 tests."
    let passed_re = Regex::new(r"\[\s+PASSED\s+\]\s+(\d+) tests?")?;
    // "[  FAILED  ] 1 test, listed below:"
    let failed_summary_re = Regex::new(r"\[\s+FAILED\s+\]\s+(\d+) tests?,\s+listed below")?;
    // "[  SKIPPED ] 1 test" (gtest 1.14+)
    let skipped_re = Regex::new(r"\[\s+SKIPPED\s+\]\s+(\d+) tests?")?;
    // "[==========] 5 tests from 2 test suites ran. (123 ms total)"
    let duration_re = Regex::new(r"\[==========\].*\((\d+) ms total\)")?;
    // Individual failures: "[  FAILED  ] TestSuite.TestName"
    let fail_re = Regex::new(r"(?m)^\[\s+FAILED\s+\]\s+(\S+)\s*$")?;

    let passed = passed_re
        .captures(input)
        .map_or(0, |c| c[1].parse().unwrap_or(0));
    let failed = failed_summary_re
        .captures(input)
        .map_or(0, |c| c[1].parse().unwrap_or(0));
    let skipped = skipped_re
        .captures(input)
        .map_or(0, |c| c[1].parse().unwrap_or(0));
    let duration_ms = duration_re
        .captures(input)
        .map(|c| c[1].parse::<u64>().unwrap_or(0));

    let mut failures = Vec::new();
    for cap in fail_re.captures_iter(input) {
        let name = cap[1].to_string();
        // Skip the summary line "N test, listed below:"
        if name.contains(',') {
            continue;
        }
        failures.push(TestFailure {
            test: name,
            file: None,
            line: None,
            message: extract_gtest_failure(input, &cap[1]),
        });
    }

    Ok(TestSummary {
        runner: "googletest".to_string(),
        passed,
        failed,
        skipped,
        duration_ms,
        failures,
    })
}

fn extract_gtest_failure(input: &str, test_name: &str) -> String {
    // Look for the RUN line followed by failure details
    let marker = format!("[ RUN      ] {test_name}");
    let mut in_section = false;
    for line in input.lines() {
        if line.contains(&marker) {
            in_section = true;
            continue;
        }
        if in_section {
            let trimmed = line.trim();
            if trimmed.contains("FAILED") || trimmed.contains("[       OK ]") {
                break;
            }
            if trimmed.contains("Expected:")
                || trimmed.contains("Actual:")
                || trimmed.contains("Value of:")
                || trimmed.contains("Which is:")
            {
                return trimmed.to_string();
            }
        }
    }
    "(see test output)".to_string()
}

fn parse_catch2(input: &str) -> anyhow::Result<TestSummary> {
    use regex::Regex;

    // "test cases: 3 | 2 passed | 1 failed"
    let cases_re = Regex::new(r"test cases:\s+(\d+)\s+\|(.+)")?;
    // "assertions: 5 | 4 passed | 1 failed"
    let _assertions_re = Regex::new(r"assertions:\s+(\d+)\s+\|(.+)")?;
    let passed_re = Regex::new(r"(\d+) passed")?;
    let failed_re = Regex::new(r"(\d+) failed")?;
    let skipped_re = Regex::new(r"(\d+) skipped")?;

    let (mut passed, mut failed, mut skipped) = (0, 0, 0);

    if let Some(cap) = cases_re.captures(input) {
        let detail = &cap[2];
        passed = passed_re
            .captures(detail)
            .map_or(0, |c| c[1].parse().unwrap_or(0));
        failed = failed_re
            .captures(detail)
            .map_or(0, |c| c[1].parse().unwrap_or(0));
        skipped = skipped_re
            .captures(detail)
            .map_or(0, |c| c[1].parse().unwrap_or(0));
    }

    // Catch2 doesn't print a total duration in a standard place, but some versions do
    let duration_re = Regex::new(r"in (\d+\.?\d*)s")?;
    let duration_ms = duration_re
        .captures(input)
        .map(|c| (c[1].parse::<f64>().unwrap_or(0.0) * 1000.0) as u64);

    // Parse "FAILED:" lines with file:line info
    let fail_block_re = Regex::new(r"(?m)^(.+):(\d+): FAILED:")?;
    let mut failures = Vec::new();
    for cap in fail_block_re.captures_iter(input) {
        failures.push(TestFailure {
            test: format!("{}:{}", &cap[1], &cap[2]),
            file: Some(cap[1].to_string()),
            line: cap[2].parse().ok(),
            message: extract_catch2_failure(input, &cap[1], &cap[2]),
        });
    }

    Ok(TestSummary {
        runner: "catch2".to_string(),
        passed,
        failed,
        skipped,
        duration_ms,
        failures,
    })
}

fn extract_catch2_failure(input: &str, file: &str, line: &str) -> String {
    let marker = format!("{file}:{line}: FAILED:");
    let mut in_section = false;
    for input_line in input.lines() {
        if input_line.contains(&marker) {
            in_section = true;
            // The FAILED: line itself often has the assertion
            let after = input_line.split("FAILED:").nth(1).unwrap_or("").trim();
            if !after.is_empty() {
                return after.to_string();
            }
            continue;
        }
        if in_section {
            let trimmed = input_line.trim();
            if trimmed.is_empty() || trimmed.starts_with("===") || trimmed.starts_with("---") {
                break;
            }
            if trimmed.starts_with("REQUIRE")
                || trimmed.starts_with("CHECK")
                || trimmed.contains("==")
                || trimmed.starts_with("with expansion:")
            {
                return trimmed.to_string();
            }
        }
    }
    "(see test output)".to_string()
}

fn parse_xctest(input: &str) -> anyhow::Result<TestSummary> {
    use regex::Regex;

    // "Executed 5 tests, with 1 failure (0 unexpected) in 0.003 (0.005) seconds"
    let summary_re = Regex::new(
        r"Executed (\d+) tests?, with (\d+) failures? \(\d+ unexpected\) in (\d+\.?\d*) \(",
    )?;
    // "Test Case '-[MyTests testFailing]' failed (0.002 seconds)."
    let fail_case_re = Regex::new(r"Test Case '(.+)' failed")?;
    let skip_re = Regex::new(r"Test Case .+ skipped")?;

    let (mut total, mut failed) = (0usize, 0usize);
    let mut duration_ms = None;

    // Use the last "Executed" line (top-level suite summary)
    for cap in summary_re.captures_iter(input) {
        total = cap[1].parse().unwrap_or(0);
        failed = cap[2].parse().unwrap_or(0);
        duration_ms = Some((cap[3].parse::<f64>().unwrap_or(0.0) * 1000.0) as u64);
    }

    let skipped = skip_re.captures_iter(input).count();
    let passed = total.saturating_sub(failed);

    let mut failures = Vec::new();
    for cap in fail_case_re.captures_iter(input) {
        let test_name = cap[1].to_string();
        failures.push(TestFailure {
            test: test_name.clone(),
            file: None,
            line: None,
            message: extract_xctest_failure(input, &test_name),
        });
    }

    // Deduplicate failures (XCTest may report the same failure multiple times across suite levels)
    failures.dedup_by(|a, b| a.test == b.test);

    Ok(TestSummary {
        runner: "xctest".to_string(),
        passed,
        failed,
        skipped,
        duration_ms,
        failures,
    })
}

fn extract_xctest_failure(input: &str, test_name: &str) -> String {
    // XCTest failure lines look like: "path/file.swift:12: error: -[MyTests testFoo] : XCTAssertEqual failed..."
    for line in input.lines() {
        if line.contains(test_name) && line.contains("XCTAssert") {
            // Extract the assertion part after the test name
            if let Some(pos) = line.find("XCTAssert") {
                return line[pos..].trim().to_string();
            }
        }
        if line.contains(test_name)
            && (line.contains("failed") || line.contains("error:"))
            && !line.contains("Test Case")
        {
            return line.trim().to_string();
        }
    }
    "(see test output)".to_string()
}

fn parse_swift_testing(input: &str) -> anyhow::Result<TestSummary> {
    use regex::Regex;

    // "Test run with 5 tests completed after 0.003 seconds with 1 issue."
    let summary_re = Regex::new(
        r"Test run with (\d+) tests? completed after (\d+\.?\d*) seconds? with (\d+) issues?",
    )?;
    // "◇ Test testExample() started." / "✔ Test testExample() passed" / "✘ Test testFailing() failed"
    let pass_re = Regex::new(r"✔ Test (.+?) passed")?;
    let fail_re = Regex::new(r"✘ Test (.+?) failed")?;
    // "↳ Expectation failed: ..."
    let issue_re = Regex::new(r"[↳⌊]\s+(.+)")?;

    let (mut total, mut issues) = (0usize, 0usize);
    let mut duration_ms = None;

    if let Some(cap) = summary_re.captures(input) {
        total = cap[1].parse().unwrap_or(0);
        duration_ms = Some((cap[2].parse::<f64>().unwrap_or(0.0) * 1000.0) as u64);
        issues = cap[3].parse().unwrap_or(0);
    }

    // Count passed/failed from markers if available
    let pass_count = pass_re.captures_iter(input).count();
    let fail_count = fail_re.captures_iter(input).count();

    let (passed, failed) = if pass_count + fail_count > 0 {
        (pass_count, fail_count)
    } else {
        (total.saturating_sub(issues), issues)
    };

    let mut failures = Vec::new();
    let lines: Vec<&str> = input.lines().collect();
    for (i, line) in lines.iter().enumerate() {
        if let Some(cap) = fail_re.captures(line) {
            let test_name = cap[1].trim().to_string();
            // Look at next line for issue detail
            let message = if i + 1 < lines.len() {
                if let Some(issue_cap) = issue_re.captures(lines[i + 1]) {
                    issue_cap[1].trim().to_string()
                } else {
                    "(see test output)".to_string()
                }
            } else {
                "(see test output)".to_string()
            };
            failures.push(TestFailure {
                test: test_name,
                file: None,
                line: None,
                message,
            });
        }
    }

    Ok(TestSummary {
        runner: "swift_testing".to_string(),
        passed,
        failed,
        skipped: 0,
        duration_ms,
        failures,
    })
}

fn is_dotnet_test(input: &str) -> bool {
    input.contains("Test Run Successful")
        || input.contains("Test Run Failed")
        || input.contains("Total tests:")
}

fn parse_dotnet_test(input: &str) -> anyhow::Result<TestSummary> {
    use regex::Regex;

    // "Total tests: 5. Passed: 4. Failed: 1. Skipped: 0."
    // Also handles: "Total tests: 5\n     Passed: 4\n     Failed: 1\n     Skipped: 0"
    let total_re = Regex::new(r"Total tests:\s*(\d+)")?;
    let passed_re = Regex::new(r"Passed:\s*(\d+)")?;
    let failed_re = Regex::new(r"Failed:\s*(\d+)")?;
    let skipped_re = Regex::new(r"Skipped:\s*(\d+)")?;
    // "Duration: 1.234 s" or "Test Run Successful. Total time: 1.2345 Seconds"
    let duration_re = Regex::new(r"(?:Duration|Total time):\s*(\d+\.?\d*)\s*[sS]")?;

    let _total = total_re
        .captures(input)
        .map_or(0usize, |c| c[1].parse().unwrap_or(0));
    let passed = passed_re
        .captures(input)
        .map_or(0, |c| c[1].parse().unwrap_or(0));
    let failed = failed_re
        .captures(input)
        .map_or(0, |c| c[1].parse().unwrap_or(0));
    let skipped = skipped_re
        .captures(input)
        .map_or(0, |c| c[1].parse().unwrap_or(0));
    let duration_ms = duration_re
        .captures(input)
        .map(|c| (c[1].parse::<f64>().unwrap_or(0.0) * 1000.0) as u64);

    // Parse individual test failures: "Failed TestNamespace.TestClass.TestMethod [< 1 ms]"
    let fail_re = Regex::new(r"(?m)^\s*Failed\s+([\w.]+)")?;
    let mut failures = Vec::new();
    for cap in fail_re.captures_iter(input) {
        let test_name = cap[1].to_string();
        failures.push(TestFailure {
            test: test_name.clone(),
            file: None,
            line: None,
            message: extract_dotnet_failure(input, &test_name),
        });
    }

    Ok(TestSummary {
        runner: "dotnet_test".to_string(),
        passed,
        failed,
        skipped,
        duration_ms,
        failures,
    })
}

fn extract_dotnet_failure(input: &str, test_name: &str) -> String {
    let marker = format!("Failed {test_name}");
    let mut in_section = false;
    for line in input.lines() {
        if line.contains(&marker) {
            in_section = true;
            continue;
        }
        if in_section {
            let trimmed = line.trim();
            // Stop at next test or summary
            if trimmed.starts_with("Failed ")
                || trimmed.starts_with("Passed ")
                || trimmed.starts_with("Total tests:")
            {
                break;
            }
            // Skip empty lines and the "Error Message:" / "Stack Trace:" headers
            if trimmed.is_empty() || trimmed == "Error Message:" || trimmed == "Stack Trace:" {
                continue;
            }
            if trimmed.contains("Assert")
                || trimmed.contains("Expected")
                || trimmed.contains("Exception")
            {
                return trimmed.to_string();
            }
        }
    }
    "(see test output)".to_string()
}

fn extract_exunit_failure(input: &str, test_name: &str) -> String {
    // Find lines near the test name that contain assertion info
    let mut in_section = false;
    for line in input.lines() {
        if line.contains(test_name) {
            in_section = true;
            continue;
        }
        if in_section {
            let trimmed = line.trim();
            if trimmed.starts_with("Assertion")
                || trimmed.starts_with("assertion")
                || trimmed.starts_with("Expected")
                || trimmed.starts_with("expected")
                || trimmed.starts_with("** (")
                || trimmed.starts_with("code:")
            {
                return trimmed.to_string();
            }
            // Stop if we hit the next test or summary
            if trimmed.starts_with("Finished in") || trimmed.contains("tests,") {
                break;
            }
        }
    }
    "(no message)".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    // === Test list (TDD roadmap) ===
    // [x] cargo test: all passing
    // [x] cargo test: with failures
    // [x] cargo test: multiple test binaries (summed results)
    // [x] pytest: all passing
    // [x] pytest: with failures
    // [x] go test: all passing
    // [x] go test: with failures
    // [x] jest: all passing
    // [x] jest: with failures
    // [x] exunit: all passing
    // [x] exunit: with failures
    // [x] exunit: with excluded/skipped
    // [x] vitest: all passing
    // [x] vitest: with failures
    // [x] googletest: all passing
    // [x] googletest: with failures
    // [x] catch2: all passing
    // [x] catch2: with failures
    // [x] xctest: all passing
    // [x] xctest: with failures
    // [x] swift_testing: all passing
    // [x] swift_testing: with failures
    // [x] unknown runner returns error
    // [x] empty input returns error

    #[test]
    fn cargo_test_all_passing() {
        let input = r#"
running 3 tests
test tests::test_one ... ok
test tests::test_two ... ok
test tests::test_three ... ok

test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.02s
"#;
        let result = parse(input).unwrap();
        assert_eq!(result.runner, "cargo_test");
        assert_eq!(result.passed, 3);
        assert_eq!(result.failed, 0);
        assert_eq!(result.skipped, 0);
        assert_eq!(result.duration_ms, Some(20));
        assert!(result.failures.is_empty());
    }

    #[test]
    fn cargo_test_with_failures() {
        let input = r#"
running 3 tests
test tests::test_one ... ok
test tests::test_two ... FAILED
test tests::test_three ... ok

failures:

---- tests::test_two stdout ----
thread 'tests::test_two' panicked at 'assertion failed: false'

failures:
    tests::test_two

test result: FAILED. 2 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s
"#;
        let result = parse(input).unwrap();
        assert_eq!(result.passed, 2);
        assert_eq!(result.failed, 1);
        assert_eq!(result.failures.len(), 1);
        assert_eq!(result.failures[0].test, "tests::test_two");
        assert!(result.failures[0].message.contains("assertion failed"));
    }

    #[test]
    fn cargo_test_multiple_binaries() {
        let input = r#"
running 2 tests
test a::test_a ... ok
test a::test_b ... ok

test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s

running 3 tests
test b::test_c ... ok
test b::test_d ... ok
test b::test_e ... ok

test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.02s
"#;
        let result = parse(input).unwrap();
        assert_eq!(result.passed, 5);
        assert_eq!(result.failed, 0);
    }

    #[test]
    fn pytest_all_passing() {
        let input = r#"
============================= test session starts ==============================
collected 5 items

tests/test_app.py .....                                                  [100%]

============================== 5 passed in 1.23s ===============================
"#;
        let result = parse(input).unwrap();
        assert_eq!(result.runner, "pytest");
        assert_eq!(result.passed, 5);
        assert_eq!(result.failed, 0);
        assert_eq!(result.duration_ms, Some(1230));
    }

    #[test]
    fn pytest_with_failures() {
        let input = r#"
============================= test session starts ==============================
collected 3 items

tests/test_app.py .F.                                                    [100%]

=================================== FAILURES ===================================
___________________________ test_create_user ___________________________________

    def test_create_user():
>       assert response.status_code == 201
E       assert 500 == 201

FAILED tests/test_app.py::test_create_user
==================== 1 failed, 2 passed in 0.45s ==============================
"#;
        let result = parse(input).unwrap();
        assert_eq!(result.passed, 2);
        assert_eq!(result.failed, 1);
        assert_eq!(result.failures.len(), 1);
        assert_eq!(result.failures[0].test, "test_create_user");
        assert_eq!(
            result.failures[0].file,
            Some("tests/test_app.py".to_string())
        );
    }

    #[test]
    fn go_test_all_passing() {
        let input = r#"
=== RUN   TestAdd
--- PASS: TestAdd (0.00s)
=== RUN   TestSubtract
--- PASS: TestSubtract (0.00s)
PASS
ok  	example.com/calc	0.003s
"#;
        let result = parse(input).unwrap();
        assert_eq!(result.runner, "go_test");
        assert_eq!(result.passed, 2);
        assert_eq!(result.failed, 0);
        assert_eq!(result.duration_ms, Some(3));
    }

    #[test]
    fn go_test_with_failures() {
        let input = r#"
=== RUN   TestAdd
--- PASS: TestAdd (0.00s)
=== RUN   TestBroken
--- FAIL: TestBroken (0.00s)
    calc_test.go:15: expected 4, got 5
FAIL
"#;
        let result = parse(input).unwrap();
        assert_eq!(result.passed, 1);
        assert_eq!(result.failed, 1);
        assert_eq!(result.failures[0].test, "TestBroken");
    }

    #[test]
    fn jest_all_passing() {
        let input = r#"
Test Suites: 1 passed, 1 total
Tests:       3 passed, 3 total
Time:        2.5s
"#;
        let result = parse(input).unwrap();
        assert_eq!(result.runner, "jest");
        assert_eq!(result.passed, 3);
        assert_eq!(result.failed, 0);
        assert_eq!(result.duration_ms, Some(2500));
    }

    #[test]
    fn jest_with_failures() {
        let input = r#"
 FAIL  src/app.test.js
  ● should return 200

    expect(received).toBe(expected)

Test Suites: 1 failed, 1 total
Tests:       1 failed, 2 passed, 3 total
Time:        1.2s
"#;
        let result = parse(input).unwrap();
        assert_eq!(result.passed, 2);
        assert_eq!(result.failed, 1);
        assert_eq!(result.failures.len(), 1);
    }

    #[test]
    fn exunit_all_passing() {
        let input = r#"
Compiling 1 file (.ex)
....

Finished in 0.1 seconds (0.05s async, 0.05s sync)
4 tests, 0 failures
"#;
        let result = parse(input).unwrap();
        assert_eq!(result.runner, "exunit");
        assert_eq!(result.passed, 4);
        assert_eq!(result.failed, 0);
        assert_eq!(result.skipped, 0);
        assert_eq!(result.duration_ms, Some(100));
        assert!(result.failures.is_empty());
    }

    #[test]
    fn exunit_with_failures() {
        let input = r#"
..

  1) test creates a user (MyApp.UserTest)
     test/my_app/user_test.exs:10
     Assertion with == failed
     code:  assert result == :ok
     left:  :error
     right: :ok
     stacktrace:
       test/my_app/user_test.exs:12: (test)

.

Finished in 0.2 seconds (0.1s async, 0.1s sync)
4 tests, 1 failure
"#;
        let result = parse(input).unwrap();
        assert_eq!(result.passed, 3);
        assert_eq!(result.failed, 1);
        assert_eq!(result.failures.len(), 1);
        assert_eq!(result.failures[0].test, "creates a user");
        assert_eq!(
            result.failures[0].file,
            Some("test/my_app/user_test.exs".to_string())
        );
        assert_eq!(result.failures[0].line, Some(10));
        assert!(result.failures[0]
            .message
            .contains("Assertion with == failed"));
    }

    #[test]
    fn exunit_with_excluded() {
        let input = r#"
...

Finished in 0.05 seconds (0.03s async, 0.02s sync)
3 tests, 0 failures, 2 excluded
"#;
        let result = parse(input).unwrap();
        assert_eq!(result.runner, "exunit");
        assert_eq!(result.passed, 3);
        assert_eq!(result.failed, 0);
        assert_eq!(result.skipped, 2);
    }

    #[test]
    fn vitest_all_passing() {
        let input = r#"
 ✓ src/utils.test.ts (3)
 ✓ src/app.test.ts (2)

 Test Files  2 passed (2)
      Tests  5 passed (5)
   Start at  14:23:45
   Duration  1.52s (transform 213ms, setup 0ms, collect 312ms, tests 1.01s, environment 0ms, prepare 89ms)
"#;
        let result = parse(input).unwrap();
        assert_eq!(result.runner, "vitest");
        assert_eq!(result.passed, 5);
        assert_eq!(result.failed, 0);
        assert_eq!(result.duration_ms, Some(1520));
    }

    #[test]
    fn vitest_with_failures() {
        let input = r#"
 ✓ src/utils.test.ts (3)
 × src/app.test.ts (1)

   × should return 200

    expect(received).toBe(expected)

 Test Files  1 failed | 1 passed (2)
      Tests  1 failed | 3 passed | 4 total
   Start at  14:23:45
   Duration  1.23s
"#;
        let result = parse(input).unwrap();
        assert_eq!(result.runner, "vitest");
        assert_eq!(result.passed, 3);
        assert_eq!(result.failed, 1);
        assert_eq!(result.failures.len(), 1);
        assert_eq!(result.failures[0].test, "should return 200");
    }

    #[test]
    fn googletest_all_passing() {
        let input = r#"
[==========] Running 3 tests from 1 test suite.
[----------] Global test environment set-up.
[----------] 3 tests from CalcTest
[ RUN      ] CalcTest.TestAdd
[       OK ] CalcTest.TestAdd (0 ms)
[ RUN      ] CalcTest.TestSub
[       OK ] CalcTest.TestSub (0 ms)
[ RUN      ] CalcTest.TestMul
[       OK ] CalcTest.TestMul (0 ms)
[----------] 3 tests from CalcTest (0 ms total)
[==========] 3 tests from 1 test suite ran. (1 ms total)
[  PASSED  ] 3 tests.
"#;
        let result = parse(input).unwrap();
        assert_eq!(result.runner, "googletest");
        assert_eq!(result.passed, 3);
        assert_eq!(result.failed, 0);
        assert_eq!(result.duration_ms, Some(1));
        assert!(result.failures.is_empty());
    }

    #[test]
    fn googletest_with_failures() {
        let input = r#"
[==========] Running 3 tests from 1 test suite.
[----------] 3 tests from CalcTest
[ RUN      ] CalcTest.TestAdd
[       OK ] CalcTest.TestAdd (0 ms)
[ RUN      ] CalcTest.TestBroken
calc_test.cpp:15: Failure
Expected: 4
  Actual: 5
[  FAILED  ] CalcTest.TestBroken (0 ms)
[ RUN      ] CalcTest.TestMul
[       OK ] CalcTest.TestMul (0 ms)
[----------] 3 tests from CalcTest (1 ms total)
[==========] 3 tests from 1 test suite ran. (1 ms total)
[  PASSED  ] 2 tests.
[  FAILED  ] 1 test, listed below:
[  FAILED  ] CalcTest.TestBroken

 1 FAILED TEST
"#;
        let result = parse(input).unwrap();
        assert_eq!(result.passed, 2);
        assert_eq!(result.failed, 1);
        assert_eq!(result.failures.len(), 1);
        assert_eq!(result.failures[0].test, "CalcTest.TestBroken");
        assert!(result.failures[0].message.contains("Expected:"));
    }

    #[test]
    fn catch2_all_passing() {
        let input = r#"
===============================================================================
All tests passed (5 assertions in 3 test cases)
test cases: 3 | 3 passed
assertions: 5 | 5 passed
"#;
        let result = parse(input).unwrap();
        assert_eq!(result.runner, "catch2");
        assert_eq!(result.passed, 3);
        assert_eq!(result.failed, 0);
        assert!(result.failures.is_empty());
    }

    #[test]
    fn catch2_with_failures() {
        let input = r#"
~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~
test_app is a Catch2 v3.4.0 host application

-------------------------------------------------------------------------------
TestMath
-------------------------------------------------------------------------------
test_math.cpp:10
...............................................................................

test_math.cpp:12: FAILED:
  REQUIRE( result == 42 )
with expansion:
  41 == 42

===============================================================================
test cases: 3 | 2 passed | 1 failed
assertions: 5 | 4 passed | 1 failed
"#;
        let result = parse(input).unwrap();
        assert_eq!(result.runner, "catch2");
        assert_eq!(result.passed, 2);
        assert_eq!(result.failed, 1);
        assert_eq!(result.failures.len(), 1);
        assert_eq!(result.failures[0].file, Some("test_math.cpp".to_string()));
        assert_eq!(result.failures[0].line, Some(12));
    }

    #[test]
    fn xctest_all_passing() {
        let input = r#"
Test Suite 'All tests' started at 2024-01-01 10:00:00.000.
Test Suite 'MyTests.xctest' started at 2024-01-01 10:00:00.000.
Test Suite 'MyTests' started at 2024-01-01 10:00:00.000.
Test Case '-[MyTests testExample]' passed (0.001 seconds).
Test Case '-[MyTests testAnother]' passed (0.002 seconds).
Test Suite 'MyTests' passed at 2024-01-01 10:00:00.003.
	 Executed 2 tests, with 0 failures (0 unexpected) in 0.003 (0.005) seconds
Test Suite 'All tests' passed at 2024-01-01 10:00:00.005.
	 Executed 2 tests, with 0 failures (0 unexpected) in 0.003 (0.005) seconds
"#;
        let result = parse(input).unwrap();
        assert_eq!(result.runner, "xctest");
        assert_eq!(result.passed, 2);
        assert_eq!(result.failed, 0);
        assert_eq!(result.duration_ms, Some(3));
        assert!(result.failures.is_empty());
    }

    #[test]
    fn xctest_with_failures() {
        let input = r#"
Test Suite 'All tests' started at 2024-01-01 10:00:00.000.
Test Suite 'MyTests' started at 2024-01-01 10:00:00.000.
Test Case '-[MyTests testExample]' passed (0.001 seconds).
/path/to/MyTests.swift:15: error: -[MyTests testFailing] : XCTAssertEqual failed: ("41") is not equal to ("42")
Test Case '-[MyTests testFailing]' failed (0.002 seconds).
Test Suite 'MyTests' failed at 2024-01-01 10:00:00.003.
	 Executed 2 tests, with 1 failure (1 unexpected) in 0.003 (0.005) seconds
Test Suite 'All tests' failed at 2024-01-01 10:00:00.005.
	 Executed 2 tests, with 1 failure (1 unexpected) in 0.003 (0.005) seconds
"#;
        let result = parse(input).unwrap();
        assert_eq!(result.passed, 1);
        assert_eq!(result.failed, 1);
        assert_eq!(result.failures.len(), 1);
        assert_eq!(result.failures[0].test, "-[MyTests testFailing]");
        assert!(result.failures[0].message.contains("XCTAssertEqual failed"));
    }

    #[test]
    fn swift_testing_all_passing() {
        let input = r#"
Build complete! (0.12s)
Test run started.
◇ Test testExample() started.
✔ Test testExample() passed after 0.001 seconds.
◇ Test testAnother() started.
✔ Test testAnother() passed after 0.002 seconds.
◇ Test run with 2 tests completed after 0.003 seconds with 0 issues.
"#;
        let result = parse(input).unwrap();
        assert_eq!(result.runner, "swift_testing");
        assert_eq!(result.passed, 2);
        assert_eq!(result.failed, 0);
        assert_eq!(result.duration_ms, Some(3));
        assert!(result.failures.is_empty());
    }

    #[test]
    fn swift_testing_with_failures() {
        let input = r#"
Build complete! (0.12s)
Test run started.
◇ Test testExample() started.
✔ Test testExample() passed after 0.001 seconds.
◇ Test testFailing() started.
✘ Test testFailing() failed after 0.002 seconds.
  ↳ Expectation failed: (result → 41) == 42
◇ Test run with 2 tests completed after 0.003 seconds with 1 issue.
"#;
        let result = parse(input).unwrap();
        assert_eq!(result.passed, 1);
        assert_eq!(result.failed, 1);
        assert_eq!(result.failures.len(), 1);
        assert_eq!(result.failures[0].test, "testFailing()");
        assert!(result.failures[0].message.contains("Expectation failed"));
    }

    #[test]
    fn dotnet_test_all_passing() {
        let input = r#"
  Determining projects to restore...
  All projects are up-to-date for restore.
  MyProject -> /src/bin/Debug/net8.0/MyProject.dll
  MyProject.Tests -> /src/tests/bin/Debug/net8.0/MyProject.Tests.dll
Test run for /src/tests/bin/Debug/net8.0/MyProject.Tests.dll (.NETCoreApp,Version=v8.0)
Microsoft (R) Test Execution Engine
Starting test execution, please wait...
A total of 1 test files matched the specified pattern.

Passed!  - Failed:     0, Passed:     5, Skipped:     0, Total:     5, Duration: 1.234 s - MyProject.Tests.dll
Total tests: 5. Passed: 5. Failed: 0. Skipped: 0.
Test Run Successful.
Total time: 2.5678 Seconds
"#;
        let result = parse(input).unwrap();
        assert_eq!(result.runner, "dotnet_test");
        assert_eq!(result.passed, 5);
        assert_eq!(result.failed, 0);
        assert_eq!(result.skipped, 0);
        assert!(result.failures.is_empty());
        assert!(result.duration_ms.is_some());
    }

    #[test]
    fn dotnet_test_with_failures() {
        let input = r#"
Test run for /src/tests/bin/Debug/net8.0/MyProject.Tests.dll (.NETCoreApp,Version=v8.0)
Starting test execution, please wait...
A total of 1 test files matched the specified pattern.

  Failed MyApp.Tests.UserServiceTests.GetUser_ReturnsNull_WhenNotFound [< 1 ms]
  Error Message:
   Assert.Equal() Failure: Expected: 42, Actual: 0
  Stack Trace:
     at MyApp.Tests.UserServiceTests.GetUser_ReturnsNull_WhenNotFound()

  Failed MyApp.Tests.UserServiceTests.CreateUser_ThrowsOnDuplicate [2 ms]
  Error Message:
   Expected exception of type 'DuplicateException'

Failed!  - Failed:     2, Passed:     3, Skipped:     1, Total:     6, Duration: 1.5 s
Total tests: 6. Passed: 3. Failed: 2. Skipped: 1.
Test Run Failed.
"#;
        let result = parse(input).unwrap();
        assert_eq!(result.runner, "dotnet_test");
        assert_eq!(result.passed, 3);
        assert_eq!(result.failed, 2);
        assert_eq!(result.skipped, 1);
        assert_eq!(result.failures.len(), 2);
        assert_eq!(
            result.failures[0].test,
            "MyApp.Tests.UserServiceTests.GetUser_ReturnsNull_WhenNotFound"
        );
        assert!(result.failures[0].message.contains("Assert.Equal()"));
        assert_eq!(
            result.failures[1].test,
            "MyApp.Tests.UserServiceTests.CreateUser_ThrowsOnDuplicate"
        );
    }

    #[test]
    fn dotnet_test_total_tests_only() {
        let input = r#"
Total tests: 10. Passed: 10. Failed: 0. Skipped: 0.
"#;
        let result = parse(input).unwrap();
        assert_eq!(result.runner, "dotnet_test");
        assert_eq!(result.passed, 10);
        assert_eq!(result.failed, 0);
    }

    #[test]
    fn unknown_runner_returns_error() {
        let result = parse("some random output that is not from any runner");
        assert!(result.is_err());
    }

    #[test]
    fn empty_input_returns_error() {
        let result = parse("");
        assert!(result.is_err());
    }
}
