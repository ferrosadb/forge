//! .NET/dotnet CLI wrappers: build, test, format_check.

use crate::runner::*;
use regex::Regex;
use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct DotnetResult {
    #[serde(flatten)]
    pub base: ToolOutput,
    #[serde(flatten)]
    pub detail: DotnetDetail,
}

#[derive(Debug, Serialize)]
#[serde(untagged)]
pub enum DotnetDetail {
    Build {
        errors: Vec<String>,
        warnings: Vec<String>,
        error_count: usize,
        warning_count: usize,
    },
    Test {
        total: u32,
        passed: u32,
        failed: u32,
        skipped: u32,
        #[serde(skip_serializing_if = "Vec::is_empty")]
        failures: Vec<String>,
    },
    Fmt {
        formatted: bool,
    },
}

pub fn run(command: &str, dir: &str, extra_args: &str, container: Option<&str>) -> DotnetResult {
    let extras = split_args(extra_args);
    let extra_refs: Vec<&str> = extras.iter().map(|s| s.as_str()).collect();

    match command {
        "build" => run_build(dir, &extra_refs, container),
        "test" => run_test(dir, &extra_refs, container),
        "format_check" => run_format_check(dir, container),
        _ => DotnetResult {
            base: ToolOutput {
                command: format!("dotnet {command}"),
                exit_code: -1,
                success: false,
                hint: Some("Valid commands: build, test, format_check".to_string()),
                raw_output: None,
                raw_input_bytes: 0,
            },
            detail: DotnetDetail::Build {
                errors: vec![format!("unknown command: {command}")],
                warnings: vec![],
                error_count: 1,
                warning_count: 0,
            },
        },
    }
}

fn run_build(dir: &str, extra: &[&str], container: Option<&str>) -> DotnetResult {
    let mut args = vec!["build"];
    args.extend_from_slice(extra);
    let cmd_str = format!("dotnet {}", args.join(" "));
    let r = run_cmd("dotnet", &args, dir, container);
    let detail = parse_build_output(&r.output);
    finish_build(cmd_str, r, detail)
}

fn run_test(dir: &str, extra: &[&str], container: Option<&str>) -> DotnetResult {
    let mut args = vec!["test"];
    args.extend_from_slice(extra);
    let cmd_str = format!("dotnet {}", args.join(" "));
    let r = run_cmd("dotnet", &args, dir, container);

    // Parse: "Total tests: N. Passed: N. Failed: N. Skipped: N."
    // Also handles "Test Run Successful." and "Test Run Failed."
    let summary_re = Regex::new(
        r"Total tests:\s*(\d+)\.\s*Passed:\s*(\d+)\.\s*Failed:\s*(\d+)\.\s*Skipped:\s*(\d+)",
    )
    .unwrap();

    let (total, passed, failed, skipped) = if let Some(caps) = summary_re.captures(&r.output) {
        (
            caps[1].parse().unwrap_or(0),
            caps[2].parse().unwrap_or(0),
            caps[3].parse().unwrap_or(0),
            caps[4].parse().unwrap_or(0),
        )
    } else {
        (0, 0, 0, 0)
    };

    // Extract individual failure lines: "  Failed MethodName [< 1 ms]"
    let fail_re = Regex::new(r"(?m)^\s*Failed\s+(.+)$").unwrap();
    let failures: Vec<String> = cap(
        fail_re
            .captures_iter(&r.output)
            .map(|c| c[1].trim().to_string())
            .collect(),
        MAX_ERRORS,
    );

    let success = failed == 0 && r.exit_code == 0;
    let hint = if r.exit_code == -1 {
        Some(binary_missing_hint("dotnet"))
    } else if !success {
        Some(test_failure_hint("dotnet test"))
    } else {
        None
    };
    let raw_output = if r.exit_code != 0 {
        Some(truncate(&r.output, MAX_RAW_TEST))
    } else {
        None
    };
    DotnetResult {
        base: ToolOutput {
            command: cmd_str,
            exit_code: r.exit_code,
            success,
            hint,
            raw_output,
            raw_input_bytes: r.output.len(),
        },
        detail: DotnetDetail::Test {
            total,
            passed,
            failed,
            skipped,
            failures,
        },
    }
}

fn run_format_check(dir: &str, container: Option<&str>) -> DotnetResult {
    let args = vec!["format", "--verify-no-changes"];
    let cmd_str = "dotnet format --verify-no-changes".to_string();
    let r = run_cmd("dotnet", &args, dir, container);

    let formatted = r.exit_code == 0;
    let hint = if r.exit_code == -1 {
        Some(binary_missing_hint("dotnet"))
    } else if !formatted {
        Some(format_hint("dotnet format"))
    } else {
        None
    };
    DotnetResult {
        base: ToolOutput::new(cmd_str, r.exit_code, formatted, hint, &r.output),
        detail: DotnetDetail::Fmt { formatted },
    }
}

/// Parse Roslyn-format diagnostics from dotnet build output.
///
/// Format: `File.cs(line,col): error|warning CODE: message`
fn parse_build_output(output: &str) -> DotnetDetail {
    let diag_re = Regex::new(r"\w+\.cs\(\d+,\d+\):\s*(error|warning)\s+\w+:").unwrap();

    let errors: Vec<String> = cap(
        output
            .lines()
            .filter(|l| diag_re.is_match(l) && l.contains("error"))
            .map(|l| l.trim().to_string())
            .collect(),
        MAX_ERRORS,
    );
    let warnings: Vec<String> = cap(
        output
            .lines()
            .filter(|l| diag_re.is_match(l) && l.contains("warning"))
            .map(|l| l.trim().to_string())
            .collect(),
        MAX_WARNINGS,
    );
    DotnetDetail::Build {
        error_count: errors.len(),
        warning_count: warnings.len(),
        errors,
        warnings,
    }
}

fn finish_build(cmd: String, r: CmdResult, detail: DotnetDetail) -> DotnetResult {
    let success = r.exit_code == 0;
    let hint = if r.exit_code == -1 {
        Some(binary_missing_hint("dotnet"))
    } else if !success {
        Some(build_error_hint("dotnet build"))
    } else {
        None
    };
    DotnetResult {
        base: ToolOutput::new(cmd, r.exit_code, success, hint, &r.output),
        detail,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_build_errors_and_warnings() {
        let output = r#"Microsoft (R) Build Engine version 17.8.0
Build started.
Program.cs(10,5): error CS1002: ; expected
Program.cs(15,1): warning CS0168: The variable 'x' is declared but never used
Build FAILED.
"#;
        let detail = parse_build_output(output);
        match detail {
            DotnetDetail::Build {
                errors,
                warnings,
                error_count,
                warning_count,
            } => {
                assert_eq!(error_count, 1);
                assert_eq!(warning_count, 1);
                assert!(errors[0].contains("CS1002"));
                assert!(warnings[0].contains("CS0168"));
            }
            _ => panic!("expected Build detail"),
        }
    }

    #[test]
    fn parse_build_no_diagnostics() {
        let output = "Build succeeded.\n    0 Warning(s)\n    0 Error(s)\n";
        let detail = parse_build_output(output);
        match detail {
            DotnetDetail::Build {
                error_count,
                warning_count,
                ..
            } => {
                assert_eq!(error_count, 0);
                assert_eq!(warning_count, 0);
            }
            _ => panic!("expected Build detail"),
        }
    }

    #[test]
    fn unknown_command_returns_error() {
        let result = run("bogus", ".", "", None);
        assert!(!result.base.success);
        assert_eq!(result.base.exit_code, -1);
        assert!(result
            .base
            .hint
            .as_deref()
            .unwrap()
            .contains("Valid commands"));
    }

    #[test]
    fn parse_test_summary_successful() {
        let output = r#"Starting test execution, please wait...
A total of 1 test files matched the specified pattern.

Passed!  - Failed:     0, Passed:     5, Skipped:     1, Total:     6, Duration: 42 ms
Total tests: 6. Passed: 5. Failed: 0. Skipped: 1.
Test Run Successful.
"#;
        let summary_re = Regex::new(
            r"Total tests:\s*(\d+)\.\s*Passed:\s*(\d+)\.\s*Failed:\s*(\d+)\.\s*Skipped:\s*(\d+)",
        )
        .unwrap();
        let caps = summary_re.captures(output).expect("should match");
        assert_eq!(&caps[1], "6");
        assert_eq!(&caps[2], "5");
        assert_eq!(&caps[3], "0");
        assert_eq!(&caps[4], "1");
    }

    #[test]
    fn parse_test_summary_failed() {
        let output = r#"  Failed TestMethod1 [< 1 ms]
  Error Message:
   Assert.Equal() Failure
Total tests: 3. Passed: 2. Failed: 1. Skipped: 0.
Test Run Failed.
"#;
        let summary_re = Regex::new(
            r"Total tests:\s*(\d+)\.\s*Passed:\s*(\d+)\.\s*Failed:\s*(\d+)\.\s*Skipped:\s*(\d+)",
        )
        .unwrap();
        let caps = summary_re.captures(output).expect("should match");
        assert_eq!(&caps[1], "3");
        assert_eq!(&caps[3], "1");

        let fail_re = Regex::new(r"(?m)^\s*Failed\s+(.+)$").unwrap();
        let failures: Vec<String> = fail_re
            .captures_iter(output)
            .map(|c| c[1].trim().to_string())
            .collect();
        assert_eq!(failures.len(), 1);
        assert!(failures[0].contains("TestMethod1"));
    }
}
