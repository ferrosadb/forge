//! Go tool wrappers: build, test, vet, fmt_check, mod_tidy.

use crate::runner::*;
use regex::Regex;
use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct GoResult {
    #[serde(flatten)]
    pub base: ToolOutput,
    #[serde(flatten)]
    pub detail: GoDetail,
}

#[derive(Debug, Serialize)]
#[serde(untagged)]
pub enum GoDetail {
    Build {
        errors: Vec<String>,
        error_count: usize,
    },
    Test {
        passed_packages: Vec<String>,
        failed_packages: Vec<String>,
        failures: Vec<String>,
    },
    Vet {
        issues: Vec<String>,
        issue_count: usize,
    },
    Fmt {
        formatted: bool,
        unformatted_files: Vec<String>,
    },
    Simple {
        output: String,
    },
}

pub fn run(command: &str, dir: &str, extra_args: &str, container: Option<&str>) -> GoResult {
    let extras = split_args(extra_args);
    let extra_refs: Vec<&str> = extras.iter().map(|s| s.as_str()).collect();

    match command {
        "build" => run_build(dir, &extra_refs, container),
        "test" => run_test(dir, &extra_refs, container),
        "vet" => run_vet(dir, &extra_refs, container),
        "fmt_check" => run_fmt_check(dir, container),
        "mod_tidy" => run_mod_tidy(dir, container),
        _ => GoResult {
            base: ToolOutput {
                command: format!("go {command}"),
                exit_code: -1,
                success: false,
                hint: Some("Valid commands: build, test, vet, fmt_check, mod_tidy".to_string()),
                raw_output: None,
                raw_input_bytes: 0,
            },
            detail: GoDetail::Build {
                errors: vec![format!("unknown command: {command}")],
                error_count: 1,
            },
        },
    }
}

fn run_build(dir: &str, extra: &[&str], container: Option<&str>) -> GoResult {
    let mut args = vec!["build", "./..."];
    args.extend_from_slice(extra);
    let cmd_str = format!("go {}", args.join(" "));
    let r = run_cmd("go", &args, dir, container);

    let re = Regex::new(r"\.go:\d+:\d+:").unwrap();
    let errors: Vec<String> = cap(
        r.output
            .lines()
            .filter(|l| re.is_match(l))
            .map(|l| l.trim().to_string())
            .collect(),
        MAX_ERRORS,
    );
    let ec = errors.len();
    let hint = if r.exit_code == -1 {
        Some(binary_missing_hint("go"))
    } else if r.exit_code != 0 {
        Some(build_error_hint("go build"))
    } else {
        None
    };
    GoResult {
        base: ToolOutput::new(cmd_str, r.exit_code, r.exit_code == 0, hint, &r.output),
        detail: GoDetail::Build {
            errors,
            error_count: ec,
        },
    }
}

fn run_test(dir: &str, extra: &[&str], container: Option<&str>) -> GoResult {
    let mut args = vec!["test", "-v", "./..."];
    args.extend_from_slice(extra);
    let cmd_str = format!("go {}", args.join(" "));
    let r = run_cmd("go", &args, dir, container);

    let ok_re = Regex::new(r"^ok\s+(\S+)").unwrap();
    let fail_re = Regex::new(r"^FAIL\s+(\S+)").unwrap();

    let mut passed = Vec::new();
    let mut failed = Vec::new();
    let mut failures = Vec::new();

    for line in r.output.lines() {
        if let Some(caps) = ok_re.captures(line) {
            passed.push(caps[1].to_string());
        } else if let Some(caps) = fail_re.captures(line) {
            failed.push(caps[1].to_string());
            failures.push(line.trim().to_string());
        }
    }

    let success = r.exit_code == 0 && failed.is_empty();
    let hint = if r.exit_code == -1 {
        Some(binary_missing_hint("go"))
    } else if !success {
        Some(test_failure_hint("go test"))
    } else {
        None
    };
    GoResult {
        base: ToolOutput::new(cmd_str, r.exit_code, success, hint, &r.output),
        detail: GoDetail::Test {
            passed_packages: passed,
            failed_packages: failed,
            failures,
        },
    }
}

fn run_vet(dir: &str, extra: &[&str], container: Option<&str>) -> GoResult {
    let mut args = vec!["vet", "./..."];
    args.extend_from_slice(extra);
    let cmd_str = format!("go {}", args.join(" "));
    let r = run_cmd("go", &args, dir, container);

    let issues: Vec<String> = cap(
        r.output
            .lines()
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .map(|l| l.trim().to_string())
            .collect(),
        MAX_WARNINGS,
    );
    let ic = issues.len();
    let hint = if r.exit_code == -1 {
        Some(binary_missing_hint("go"))
    } else if ic > 0 {
        Some(lint_hint("go vet"))
    } else {
        None
    };
    GoResult {
        base: ToolOutput::new(cmd_str, r.exit_code, r.exit_code == 0, hint, &r.output),
        detail: GoDetail::Vet {
            issues,
            issue_count: ic,
        },
    }
}

fn run_fmt_check(dir: &str, container: Option<&str>) -> GoResult {
    let args = vec!["-l", "."];
    let cmd_str = "gofmt -l .".to_string();
    let r = run_cmd("gofmt", &args, dir, container);

    let files: Vec<String> = r
        .output
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| l.to_string())
        .collect();
    let formatted = files.is_empty();
    let hint = if r.exit_code == -1 {
        Some(binary_missing_hint("gofmt"))
    } else if !formatted {
        Some(format_hint("gofmt -w ."))
    } else {
        None
    };
    GoResult {
        base: ToolOutput::new(cmd_str, r.exit_code, formatted, hint, &r.output),
        detail: GoDetail::Fmt {
            formatted,
            unformatted_files: files,
        },
    }
}

fn run_mod_tidy(dir: &str, container: Option<&str>) -> GoResult {
    let args = vec!["mod", "tidy"];
    let cmd_str = "go mod tidy".to_string();
    let r = run_cmd("go", &args, dir, container);
    let output = r.output.trim().to_string();
    let hint = if r.exit_code == -1 {
        Some(binary_missing_hint("go"))
    } else if r.exit_code != 0 {
        Some("Check that go.mod exists and module dependencies are valid.".to_string())
    } else {
        None
    };
    GoResult {
        base: ToolOutput::new(cmd_str, r.exit_code, r.exit_code == 0, hint, &r.output),
        detail: GoDetail::Simple { output },
    }
}
