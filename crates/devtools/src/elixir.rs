//! Elixir/Mix tool wrappers: compile, test, format_check, deps.

use crate::runner::*;
use regex::Regex;
use serde::Serialize;

// ── mix compile ────────────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct CompileResult {
    #[serde(flatten)]
    pub base: ToolOutput,
    pub errors: Vec<CompileMessage>,
    pub warnings: Vec<CompileMessage>,
    pub error_count: usize,
    pub warning_count: usize,
}

#[derive(Debug, Serialize)]
pub struct CompileMessage {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    pub message: String,
}

pub fn compile(dir: &str, container: Option<&str>) -> CompileResult {
    let args = vec!["compile", "--all-warnings"];
    let cmd_str = "mix compile --all-warnings".to_string();
    let r = run_cmd("mix", &args, dir, container);

    let loc_re = Regex::new(r"([^:]+):(\d+)(?::\d+)?:\s*(warning|error):\s*(.+)").unwrap();
    let warn_re = Regex::new(r"warning:\s*(.+)").unwrap();
    let err_re = Regex::new(r"(?:\*\* \(|error:)\s*(.+)").unwrap();

    let mut errors = Vec::new();
    let mut warnings = Vec::new();

    for line in r.output.lines() {
        if let Some(caps) = loc_re.captures(line) {
            let msg = CompileMessage {
                file: Some(caps[1].to_string()),
                line: caps[2].parse().ok(),
                message: caps[4].trim().to_string(),
            };
            if &caps[3] == "error" {
                errors.push(msg);
            } else {
                warnings.push(msg);
            }
        } else if line.contains("** (") || line.contains("error:") {
            if let Some(caps) = err_re.captures(line) {
                errors.push(CompileMessage {
                    file: None,
                    line: None,
                    message: caps[1].trim().to_string(),
                });
            }
        } else if line.contains("warning:") {
            if let Some(caps) = warn_re.captures(line) {
                warnings.push(CompileMessage {
                    file: None,
                    line: None,
                    message: caps[1].trim().to_string(),
                });
            }
        }
    }

    let errors = cap(errors, MAX_ERRORS);
    let warnings = cap(warnings, MAX_WARNINGS);
    let ec = errors.len();
    let wc = warnings.len();
    let success = r.exit_code == 0 && ec == 0;

    let hint = if r.exit_code == -1 {
        Some(binary_missing_hint("mix"))
    } else if ec > 0 {
        Some(build_error_hint("mix compile"))
    } else if wc > 0 {
        Some(
            "Compilation succeeded with warnings. Fix warnings to keep the codebase clean."
                .to_string(),
        )
    } else {
        None
    };

    CompileResult {
        base: ToolOutput::new(cmd_str, r.exit_code, success, hint, &r.output),
        error_count: ec,
        warning_count: wc,
        errors,
        warnings,
    }
}

// ── mix test ───────────────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct TestResult {
    #[serde(flatten)]
    pub base: ToolOutput,
    pub tests: u32,
    pub failures: u32,
    pub skipped: u32,
    pub failure_details: Vec<FailureDetail>,
}

#[derive(Debug, Serialize)]
pub struct FailureDetail {
    pub test: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    pub detail: String,
}

pub fn test(
    dir: &str,
    file: Option<&str>,
    extra_args: &str,
    container: Option<&str>,
) -> TestResult {
    let extras = split_args(extra_args);
    let mut args: Vec<&str> = vec!["test"];
    if let Some(f) = file {
        args.push(f);
    }
    let extra_refs: Vec<&str> = extras.iter().map(|s| s.as_str()).collect();
    args.extend_from_slice(&extra_refs);
    let cmd_str = format!("mix {}", args.join(" "));
    let r = run_cmd("mix", &args, dir, container);

    let sum_re =
        Regex::new(r"(\d+)\s+tests?,\s+(\d+)\s+failures?(?:,\s+(\d+)\s+skipped)?").unwrap();
    let (tests, failures, skipped) = sum_re
        .captures(&r.output)
        .map(|c| {
            (
                c[1].parse().unwrap_or(0),
                c[2].parse().unwrap_or(0),
                c.get(3)
                    .and_then(|m| m.as_str().parse().ok())
                    .unwrap_or(0u32),
            )
        })
        .unwrap_or((0, 0, 0));

    let fail_re =
        Regex::new(r"(?m)^\s+\d+\)\s+test (.+)\n\s+(.+):(\d+)\n((?:.*\n)*?)\s*(?:\s+\d+\)|\z)")
            .unwrap();
    let failure_details: Vec<FailureDetail> = cap(
        fail_re
            .captures_iter(&r.output)
            .map(|c| FailureDetail {
                test: c[1].trim().to_string(),
                file: Some(c[2].to_string()),
                line: c[3].parse().ok(),
                detail: truncate(c[4].trim(), 500),
            })
            .collect(),
        MAX_ERRORS,
    );

    let success = r.exit_code == 0 && failures == 0;
    let hint = if r.exit_code == -1 {
        Some(binary_missing_hint("mix"))
    } else if failures > 0 {
        Some(test_failure_hint("mix test"))
    } else if tests == 0 && r.exit_code != 0 {
        Some("No tests found or compilation failed. Run mix_compile first to check for build errors.".to_string())
    } else {
        None
    };

    TestResult {
        base: ToolOutput::new(cmd_str, r.exit_code, success, hint, &r.output),
        tests,
        failures,
        skipped,
        failure_details,
    }
}

// ── mix format --check-formatted ───────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct FormatResult {
    #[serde(flatten)]
    pub base: ToolOutput,
    pub formatted: bool,
    pub unformatted_files: Vec<String>,
    pub unformatted_count: usize,
}

pub fn format_check(dir: &str, container: Option<&str>) -> FormatResult {
    let args = vec!["format", "--check-formatted"];
    let cmd_str = "mix format --check-formatted".to_string();
    let r = run_cmd("mix", &args, dir, container);

    let files: Vec<String> = r
        .output
        .lines()
        .filter(|l| l.ends_with(".ex") || l.ends_with(".exs"))
        .map(|l| l.trim().to_string())
        .collect();
    let fc = files.len();
    let formatted = files.is_empty() && r.exit_code == 0;

    let hint = if r.exit_code == -1 {
        Some(binary_missing_hint("mix"))
    } else if !formatted {
        Some(format_hint("mix format"))
    } else {
        None
    };

    FormatResult {
        base: ToolOutput::new(cmd_str, r.exit_code, formatted, hint, &r.output),
        formatted,
        unformatted_count: fc,
        unformatted_files: files,
    }
}

// ── mix deps ───────────────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct DepsResult {
    #[serde(flatten)]
    pub base: ToolOutput,
    pub total: usize,
    pub dependencies: Vec<DepInfo>,
}

#[derive(Debug, Serialize)]
pub struct DepInfo {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    pub status: String,
}

pub fn deps(dir: &str, container: Option<&str>) -> DepsResult {
    let args = vec!["deps"];
    let r = run_cmd("mix", &args, dir, container);

    let dep_re = Regex::new(r"^\*\s+(\S+)\s+(\S+)\s+\(([^)]+)\)\s+\((\w+)\)").unwrap();
    let dep_simple_re = Regex::new(r"^\*\s+(\S+)\s+(.+)").unwrap();

    let mut dependencies = Vec::new();
    for line in r.output.lines() {
        if let Some(caps) = dep_re.captures(line) {
            dependencies.push(DepInfo {
                name: caps[1].to_string(),
                version: Some(caps[2].to_string()),
                source: Some(caps[3].to_string()),
                status: caps[4].to_string(),
            });
        } else if let Some(caps) = dep_simple_re.captures(line) {
            dependencies.push(DepInfo {
                name: caps[1].to_string(),
                version: None,
                source: None,
                status: caps[2].trim().to_string(),
            });
        }
    }

    let total = dependencies.len();
    let hint = if r.exit_code == -1 {
        Some(binary_missing_hint("mix"))
    } else if r.exit_code != 0 {
        Some("Run `mix deps.get` to fetch missing dependencies.".to_string())
    } else {
        None
    };

    DepsResult {
        base: ToolOutput::new(
            "mix deps".to_string(),
            r.exit_code,
            r.exit_code == 0,
            hint,
            &r.output,
        ),
        total,
        dependencies,
    }
}
