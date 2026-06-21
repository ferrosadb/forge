//! Python tool wrappers: test, lint, format_check, deps, typecheck.

use crate::runner::*;
use regex::Regex;
use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct PythonResult {
    #[serde(flatten)]
    pub base: ToolOutput,
    #[serde(flatten)]
    pub detail: PythonDetail,
}

#[derive(Debug, Serialize)]
#[serde(untagged)]
pub enum PythonDetail {
    Test {
        passed: u32,
        failed: u32,
    },
    Lint {
        errors: Vec<String>,
        error_count: usize,
    },
    Fmt {
        formatted: bool,
        unformatted_files: Vec<String>,
    },
    Deps {
        packages: Vec<PkgEntry>,
        total: usize,
    },
    Typecheck {
        errors: Vec<String>,
        error_count: usize,
    },
}

#[derive(Debug, Serialize)]
pub struct PkgEntry {
    pub name: String,
    pub version: String,
}

pub fn run(command: &str, dir: &str, extra_args: &str, container: Option<&str>) -> PythonResult {
    let extras = split_args(extra_args);
    let extra_refs: Vec<&str> = extras.iter().map(|s| s.as_str()).collect();

    match command {
        "test" => run_test(dir, &extra_refs, container),
        "lint" => run_lint(dir, &extra_refs, container),
        "format_check" => run_format_check(dir, container),
        "deps" => run_deps(dir, container),
        "typecheck" => run_typecheck(dir, &extra_refs, container),
        _ => PythonResult {
            base: ToolOutput {
                command: format!("python {command}"),
                exit_code: -1,
                success: false,
                hint: Some("Valid commands: test, lint, format_check, deps, typecheck".to_string()),
                raw_output: None,
                raw_input_bytes: 0,
            },
            detail: PythonDetail::Test {
                passed: 0,
                failed: 0,
            },
        },
    }
}

fn run_test(dir: &str, extra: &[&str], container: Option<&str>) -> PythonResult {
    let mut args = vec!["-m", "pytest", "-v", "--tb=short"];
    args.extend_from_slice(extra);
    let cmd_str = format!("python {}", args.join(" "));
    let r = run_cmd("python", &args, dir, container);

    let pass_re = Regex::new(r"(\d+)\s+passed").unwrap();
    let fail_re = Regex::new(r"(\d+)\s+failed").unwrap();
    let passed = pass_re
        .captures(&r.output)
        .and_then(|c| c[1].parse().ok())
        .unwrap_or(0);
    let failed = fail_re
        .captures(&r.output)
        .and_then(|c| c[1].parse().ok())
        .unwrap_or(0);

    let success = r.exit_code == 0 && failed == 0;
    let hint = if r.exit_code == -1 {
        Some(binary_missing_hint("python"))
    } else if !success && passed == 0 && failed == 0 {
        Some("No test results parsed. Ensure pytest is installed (`pip install pytest`) and tests exist.".to_string())
    } else if !success {
        Some(test_failure_hint("python -m pytest"))
    } else {
        None
    };
    PythonResult {
        base: ToolOutput::new(cmd_str, r.exit_code, success, hint, &r.output),
        detail: PythonDetail::Test { passed, failed },
    }
}

fn run_lint(dir: &str, extra: &[&str], container: Option<&str>) -> PythonResult {
    let mut args = vec!["-m", "ruff", "check", "."];
    args.extend_from_slice(extra);
    let cmd_str = format!("python {}", args.join(" "));
    let r = run_cmd("python", &args, dir, container);

    let line_re = Regex::new(r"\.py:\d+:\d+:").unwrap();
    let errors: Vec<String> = cap(
        r.output
            .lines()
            .filter(|l| line_re.is_match(l))
            .map(|l| l.trim().to_string())
            .collect(),
        MAX_WARNINGS,
    );
    let ec = errors.len();
    let hint = if r.exit_code == -1 {
        Some(binary_missing_hint("python"))
    } else if ec > 0 {
        Some(lint_hint("python -m ruff check ."))
    } else {
        None
    };
    PythonResult {
        base: ToolOutput::new(cmd_str, r.exit_code, r.exit_code == 0, hint, &r.output),
        detail: PythonDetail::Lint {
            errors,
            error_count: ec,
        },
    }
}

fn run_format_check(dir: &str, container: Option<&str>) -> PythonResult {
    let args = vec!["-m", "black", "--check", "--diff", "."];
    let cmd_str = "python -m black --check --diff .".to_string();
    let r = run_cmd("python", &args, dir, container);

    let reformat_re = Regex::new(r"would reformat\s+(.+)").unwrap();
    let files: Vec<String> = r
        .output
        .lines()
        .filter_map(|l| reformat_re.captures(l).map(|c| c[1].trim().to_string()))
        .collect();
    let formatted = files.is_empty() && r.exit_code == 0;

    let hint = if r.exit_code == -1 {
        Some(binary_missing_hint("python"))
    } else if !formatted {
        Some(format_hint("python -m black ."))
    } else {
        None
    };
    PythonResult {
        base: ToolOutput::new(cmd_str, r.exit_code, formatted, hint, &r.output),
        detail: PythonDetail::Fmt {
            formatted,
            unformatted_files: files,
        },
    }
}

fn run_deps(dir: &str, container: Option<&str>) -> PythonResult {
    let args = vec!["list", "--format=json"];
    let cmd_str = "pip list --format=json".to_string();
    let r = run_cmd("pip", &args, dir, container);

    let mut packages = Vec::new();
    if let Ok(arr) = serde_json::from_str::<Vec<serde_json::Value>>(&r.output) {
        for item in arr {
            let name = item
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("?")
                .to_string();
            let version = item
                .get("version")
                .and_then(|v| v.as_str())
                .unwrap_or("?")
                .to_string();
            packages.push(PkgEntry { name, version });
        }
    }

    let total = packages.len();
    let hint = if r.exit_code == -1 {
        Some(binary_missing_hint("pip"))
    } else {
        None
    };
    PythonResult {
        base: ToolOutput::new(cmd_str, r.exit_code, true, hint, &r.output),
        detail: PythonDetail::Deps { packages, total },
    }
}

fn run_typecheck(dir: &str, extra: &[&str], container: Option<&str>) -> PythonResult {
    let mut args = vec!["-m", "mypy", "."];
    args.extend_from_slice(extra);
    let cmd_str = format!("python {}", args.join(" "));
    let r = run_cmd("python", &args, dir, container);

    let line_re = Regex::new(r"\.py:\d+:\s+error:").unwrap();
    let errors: Vec<String> = cap(
        r.output
            .lines()
            .filter(|l| line_re.is_match(l))
            .map(|l| l.trim().to_string())
            .collect(),
        MAX_WARNINGS,
    );
    let ec = errors.len();
    let hint = if r.exit_code == -1 {
        Some(binary_missing_hint("python"))
    } else if ec > 0 {
        Some(build_error_hint("python -m mypy ."))
    } else {
        None
    };
    PythonResult {
        base: ToolOutput::new(cmd_str, r.exit_code, r.exit_code == 0, hint, &r.output),
        detail: PythonDetail::Typecheck {
            errors,
            error_count: ec,
        },
    }
}
