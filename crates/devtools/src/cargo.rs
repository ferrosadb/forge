//! Rust/Cargo tool wrappers: build, check, test, clippy, fmt_check.

use crate::runner::*;
use regex::Regex;
use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct CargoResult {
    #[serde(flatten)]
    pub base: ToolOutput,
    #[serde(flatten)]
    pub detail: CargoDetail,
}

#[derive(Debug, Serialize)]
#[serde(untagged)]
pub enum CargoDetail {
    Build {
        errors: Vec<String>,
        warnings: Vec<String>,
        error_count: usize,
        warning_count: usize,
    },
    Test {
        passed: u32,
        failed: u32,
        ignored: u32,
        #[serde(skip_serializing_if = "Vec::is_empty")]
        failures: Vec<TestFailure>,
    },
    Lint {
        clean: bool,
        warnings: Vec<String>,
        warning_count: usize,
    },
    Fmt {
        formatted: bool,
        unformatted_files: Vec<String>,
    },
}

/// A single test failure with name and error excerpt.
#[derive(Debug, Serialize)]
pub struct TestFailure {
    pub test_name: String,
    pub error: String,
}

pub fn run(command: &str, dir: &str, extra_args: &str, container: Option<&str>) -> CargoResult {
    let extras = split_args(extra_args);
    let extra_refs: Vec<&str> = extras.iter().map(|s| s.as_str()).collect();

    match command {
        "build" => run_build(dir, &extra_refs, container),
        "check" => run_check(dir, &extra_refs, container),
        "test" => run_test(dir, &extra_refs, container),
        "clippy" => run_clippy(dir, &extra_refs, container),
        "fmt_check" => run_fmt_check(dir, container),
        _ => CargoResult {
            base: ToolOutput {
                command: format!("cargo {command}"),
                exit_code: -1,
                success: false,
                hint: Some("Valid commands: build, check, test, clippy, fmt_check".to_string()),
                raw_output: None,
                raw_input_bytes: 0,
            },
            detail: CargoDetail::Build {
                errors: vec![format!("unknown command: {command}")],
                warnings: vec![],
                error_count: 1,
                warning_count: 0,
            },
        },
    }
}

fn run_build(dir: &str, extra: &[&str], container: Option<&str>) -> CargoResult {
    let mut args = vec!["build", "--message-format=short"];
    args.extend_from_slice(extra);
    let cmd_str = format!("cargo {}", args.join(" "));
    let r = run_cmd("cargo", &args, dir, container);
    let detail = parse_build_output(&r.output);
    finish_build(cmd_str, r, detail)
}

fn run_check(dir: &str, extra: &[&str], container: Option<&str>) -> CargoResult {
    let mut args = vec!["check", "--message-format=short"];
    args.extend_from_slice(extra);
    let cmd_str = format!("cargo {}", args.join(" "));
    let r = run_cmd("cargo", &args, dir, container);
    let detail = parse_build_output(&r.output);
    finish_build(cmd_str, r, detail)
}

fn run_test(dir: &str, extra: &[&str], container: Option<&str>) -> CargoResult {
    let mut args = vec!["test"];
    args.extend_from_slice(extra);
    let cmd_str = format!("cargo {}", args.join(" "));
    let r = run_cmd("cargo", &args, dir, container);

    let re =
        Regex::new(r"test result: (\w+)\.\s+(\d+)\s+passed;\s+(\d+)\s+failed;\s+(\d+)\s+ignored")
            .unwrap();
    let (passed, failed, ignored) = if let Some(caps) = re.captures(&r.output) {
        (
            caps[2].parse().unwrap_or(0),
            caps[3].parse().unwrap_or(0),
            caps[4].parse().unwrap_or(0),
        )
    } else {
        (0, 0, 0)
    };

    // Parse individual test failures: "test <name> ... FAILED" followed by error output
    let failures = parse_test_failures(&r.output);

    let detail = CargoDetail::Test {
        passed,
        failed,
        ignored,
        failures,
    };
    let success = failed == 0 && r.exit_code == 0;
    let hint = if r.exit_code == -1 {
        Some(binary_missing_hint("cargo"))
    } else if !success {
        Some(test_failure_hint("cargo test"))
    } else {
        None
    };
    // Use larger truncation limit for test failures to include stack traces
    let raw_output = if r.exit_code != 0 {
        Some(truncate(&r.output, MAX_RAW_TEST))
    } else {
        None
    };
    CargoResult {
        base: ToolOutput {
            command: cmd_str,
            exit_code: r.exit_code,
            success,
            hint,
            raw_output,
            raw_input_bytes: r.output.len(),
        },
        detail,
    }
}

/// Extract individual test failures with their error messages.
fn parse_test_failures(output: &str) -> Vec<TestFailure> {
    let mut failures = Vec::new();
    let re = Regex::new(r"test (\S+) \.\.\. FAILED").unwrap();

    for cap in re.captures_iter(output) {
        let test_name = cap[1].to_string();
        // Find the error section for this test (from "thread '<test>' panicked" to next test or end)
        if let Some(start) = output.find(&format!("test {} ... FAILED", test_name)) {
            let rest = &output[start..];
            // Extract up to 50 lines after the failure marker
            let error_lines: Vec<&str> = rest.lines().skip(1).take(50).collect();
            let error = error_lines.join("\n").trim().to_string();
            failures.push(TestFailure { test_name, error });
        }
    }

    // Cap at 10 failures to avoid oversized output
    if failures.len() > 10 {
        failures.truncate(10);
    }
    failures
}

/// Split extra clippy args into (cargo_flags, lint_flags).
///
/// If `extra` contains `--`, everything before it is a cargo flag
/// (`--all-targets`, `--workspace`, etc.) and everything after is a
/// rustc/lint flag (`-W clippy::pedantic`, `-D warnings`, etc.).
/// When no `--` is present every arg is treated as a cargo flag.
fn split_clippy_extra<'a>(extra: &[&'a str]) -> (Vec<&'a str>, Vec<&'a str>) {
    match extra.iter().position(|&s| s == "--") {
        Some(pos) => (extra[..pos].to_vec(), extra[pos + 1..].to_vec()),
        None => (extra.to_vec(), vec![]),
    }
}

/// Build the full argument list for `cargo clippy`.
///
/// Layout: `clippy [cargo_flags] --message-format=short -- -W clippy::all [lint_flags]`
fn build_clippy_args<'a>(extra: &[&'a str]) -> Vec<&'a str> {
    let (cargo_extra, lint_extra) = split_clippy_extra(extra);
    let mut args = vec!["clippy"];
    args.extend_from_slice(&cargo_extra);
    args.push("--message-format=short");
    args.extend_from_slice(&["--", "-W", "clippy::all"]);
    args.extend_from_slice(&lint_extra);
    args
}

fn run_clippy(dir: &str, extra: &[&str], container: Option<&str>) -> CargoResult {
    let args = build_clippy_args(extra);
    let cmd_str = format!("cargo {}", args.join(" "));
    let r = run_cmd("cargo", &args, dir, container);

    let warnings: Vec<String> = r
        .output
        .lines()
        .filter(|l| l.contains("warning"))
        .filter(|l| !l.contains("generated"))
        .map(|l| l.trim().to_string())
        .collect();
    let warnings = cap(warnings, MAX_WARNINGS);
    let wc = warnings.len();
    let clean = wc == 0;
    let detail = CargoDetail::Lint {
        clean,
        warning_count: wc,
        warnings,
    };
    let hint = if r.exit_code == -1 {
        Some(binary_missing_hint("cargo"))
    } else if !clean {
        Some(lint_hint("cargo clippy"))
    } else {
        None
    };
    CargoResult {
        base: ToolOutput::new(cmd_str, r.exit_code, r.exit_code == 0, hint, &r.output),
        detail,
    }
}

fn run_fmt_check(dir: &str, container: Option<&str>) -> CargoResult {
    let args = vec!["fmt", "--check"];
    let cmd_str = "cargo fmt --check".to_string();
    let r = run_cmd("cargo", &args, dir, container);

    let re = Regex::new(r"Diff in (.+):").unwrap();
    let files: Vec<String> = r
        .output
        .lines()
        .filter_map(|l| re.captures(l).map(|c| c[1].to_string()))
        .collect();
    let formatted = files.is_empty() && r.exit_code == 0;
    let hint = if r.exit_code == -1 {
        Some(binary_missing_hint("cargo"))
    } else if !formatted {
        Some(format_hint("cargo fmt"))
    } else {
        None
    };
    CargoResult {
        base: ToolOutput::new(cmd_str, r.exit_code, formatted, hint, &r.output),
        detail: CargoDetail::Fmt {
            formatted,
            unformatted_files: files,
        },
    }
}

fn parse_build_output(output: &str) -> CargoDetail {
    let errors: Vec<String> = cap(
        output
            .lines()
            .filter(|l| l.contains("error"))
            .map(|l| l.trim().to_string())
            .collect(),
        MAX_ERRORS,
    );
    let warnings: Vec<String> = cap(
        output
            .lines()
            .filter(|l| l.contains("warning"))
            .filter(|l| !l.contains("generated"))
            .map(|l| l.trim().to_string())
            .collect(),
        MAX_WARNINGS,
    );
    CargoDetail::Build {
        error_count: errors.len(),
        warning_count: warnings.len(),
        errors,
        warnings,
    }
}

fn finish_build(cmd: String, r: CmdResult, detail: CargoDetail) -> CargoResult {
    let success = r.exit_code == 0;
    let hint = if r.exit_code == -1 {
        Some(binary_missing_hint("cargo"))
    } else if !success {
        Some(build_error_hint("cargo build"))
    } else {
        None
    };
    CargoResult {
        base: ToolOutput::new(cmd, r.exit_code, success, hint, &r.output),
        detail,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── split_clippy_extra ────────────────────────────────────────────────────

    #[test]
    fn split_empty_extra() {
        let (cargo, lint) = split_clippy_extra(&[]);
        assert!(cargo.is_empty());
        assert!(lint.is_empty());
    }

    #[test]
    fn split_cargo_only_flag() {
        let (cargo, lint) = split_clippy_extra(&["--all-targets"]);
        assert_eq!(cargo, vec!["--all-targets"]);
        assert!(lint.is_empty());
    }

    #[test]
    fn split_multiple_cargo_flags() {
        let (cargo, lint) = split_clippy_extra(&["--workspace", "--all-targets"]);
        assert_eq!(cargo, vec!["--workspace", "--all-targets"]);
        assert!(lint.is_empty());
    }

    #[test]
    fn split_lint_flags_only() {
        let (cargo, lint) = split_clippy_extra(&["--", "-W", "clippy::pedantic"]);
        assert!(cargo.is_empty());
        assert_eq!(lint, vec!["-W", "clippy::pedantic"]);
    }

    #[test]
    fn split_mixed_flags() {
        let extra = ["--all-targets", "--", "-W", "clippy::pedantic"];
        let (cargo, lint) = split_clippy_extra(&extra);
        assert_eq!(cargo, vec!["--all-targets"]);
        assert_eq!(lint, vec!["-W", "clippy::pedantic"]);
    }

    // ── build_clippy_args ─────────────────────────────────────────────────────

    #[test]
    fn build_baseline_no_extra() {
        let args = build_clippy_args(&[]);
        assert_eq!(
            args,
            vec![
                "clippy",
                "--message-format=short",
                "--",
                "-W",
                "clippy::all"
            ]
        );
    }

    #[test]
    fn build_all_targets_before_separator() {
        let args = build_clippy_args(&["--all-targets"]);
        let sep = args.iter().position(|&s| s == "--").expect("-- missing");
        let at_pos = args
            .iter()
            .position(|&s| s == "--all-targets")
            .expect("--all-targets missing");
        assert!(at_pos < sep, "--all-targets must appear before --");
    }

    #[test]
    fn build_lint_flag_after_builtin_lint() {
        let args = build_clippy_args(&["--", "-W", "clippy::pedantic"]);
        let all_pos = args
            .windows(2)
            .position(|w| w == ["-W", "clippy::all"])
            .expect("-W clippy::all missing");
        let ped_pos = args
            .windows(2)
            .position(|w| w == ["-W", "clippy::pedantic"])
            .expect("-W clippy::pedantic missing");
        assert!(
            ped_pos > all_pos,
            "user lint flags must come after -W clippy::all"
        );
    }

    #[test]
    fn build_message_format_always_present() {
        let args = build_clippy_args(&["--all-targets"]);
        assert!(
            args.contains(&"--message-format=short"),
            "--message-format=short must always be present"
        );
    }
}
