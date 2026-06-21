//! Node/NPM tool wrappers: test, typecheck, lint, format_check, deps, build, audit.

use crate::runner::*;
use regex::Regex;
use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct NpmResult {
    #[serde(flatten)]
    pub base: ToolOutput,
    #[serde(flatten)]
    pub detail: NpmDetail,
}

#[derive(Debug, Serialize)]
#[serde(untagged)]
pub enum NpmDetail {
    Test {
        passed: u32,
        failed: u32,
        suites_passed: u32,
        suites_failed: u32,
    },
    Typecheck {
        errors: Vec<String>,
        error_count: usize,
    },
    Lint {
        errors: Vec<String>,
        warnings: Vec<String>,
        error_count: usize,
        warning_count: usize,
    },
    Fmt {
        formatted: bool,
        unformatted_files: Vec<String>,
    },
    Deps {
        dependencies: Vec<DepEntry>,
        total: usize,
    },
    Build {
        errors: Vec<String>,
        error_count: usize,
    },
    Audit {
        vulnerabilities: VulnSummary,
        total: usize,
    },
}

#[derive(Debug, Serialize)]
pub struct DepEntry {
    pub name: String,
    pub version: String,
}

#[derive(Debug, Serialize)]
pub struct VulnSummary {
    #[serde(skip_serializing_if = "is_zero")]
    pub critical: u32,
    #[serde(skip_serializing_if = "is_zero")]
    pub high: u32,
    #[serde(skip_serializing_if = "is_zero")]
    pub moderate: u32,
    #[serde(skip_serializing_if = "is_zero")]
    pub low: u32,
    #[serde(skip_serializing_if = "is_zero")]
    pub info: u32,
}

fn is_zero(v: &u32) -> bool {
    *v == 0
}

pub fn run(command: &str, dir: &str, extra_args: &str, container: Option<&str>) -> NpmResult {
    let extras = split_args(extra_args);
    let extra_refs: Vec<&str> = extras.iter().map(|s| s.as_str()).collect();

    match command {
        "test" => run_test(dir, &extra_refs, container),
        "typecheck" => run_typecheck(dir, &extra_refs, container),
        "lint" => run_lint(dir, &extra_refs, container),
        "format_check" => run_format_check(dir, container),
        "deps" => run_deps(dir, container),
        "build" => run_build(dir, &extra_refs, container),
        "audit" => run_audit(dir, container),
        _ => NpmResult {
            base: ToolOutput {
                command: format!("npm {command}"),
                exit_code: -1,
                success: false,
                hint: Some(
                    "Valid commands: test, typecheck, lint, format_check, deps, build, audit"
                        .to_string(),
                ),
                raw_output: None,
                raw_input_bytes: 0,
            },
            detail: NpmDetail::Build {
                errors: vec![format!("unknown command: {command}")],
                error_count: 1,
            },
        },
    }
}

fn run_test(dir: &str, extra: &[&str], container: Option<&str>) -> NpmResult {
    let mut args = vec!["--no-install", "jest", "--run"];
    args.extend_from_slice(extra);
    let cmd_str = format!("npx {}", args.join(" "));
    let r = run_cmd("npx", &args, dir, container);

    let pass_re = Regex::new(r"Tests:\s+(\d+)\s+passed").unwrap();
    let fail_re = Regex::new(r"Tests:\s+(\d+)\s+failed").unwrap();
    let suite_pass_re = Regex::new(r"Test Suites:\s+(\d+)\s+passed").unwrap();
    let suite_fail_re = Regex::new(r"Test Suites:\s+(\d+)\s+failed").unwrap();

    let passed = pass_re
        .captures(&r.output)
        .and_then(|c| c[1].parse().ok())
        .unwrap_or(0);
    let failed = fail_re
        .captures(&r.output)
        .and_then(|c| c[1].parse().ok())
        .unwrap_or(0);
    let sp = suite_pass_re
        .captures(&r.output)
        .and_then(|c| c[1].parse().ok())
        .unwrap_or(0);
    let sf = suite_fail_re
        .captures(&r.output)
        .and_then(|c| c[1].parse().ok())
        .unwrap_or(0);

    let success = failed == 0 && passed > 0;
    let hint = if r.exit_code == -1 {
        Some(binary_missing_hint("npx"))
    } else if !success && passed == 0 && failed == 0 {
        Some("No test results parsed. Jest may not be installed — run `npm install` or check that jest is configured.".to_string())
    } else if !success {
        Some(test_failure_hint("npx jest"))
    } else {
        None
    };
    NpmResult {
        base: ToolOutput::new(cmd_str, r.exit_code, success, hint, &r.output),
        detail: NpmDetail::Test {
            passed,
            failed,
            suites_passed: sp,
            suites_failed: sf,
        },
    }
}

fn run_typecheck(dir: &str, extra: &[&str], container: Option<&str>) -> NpmResult {
    let mut args = vec!["--no-install", "tsc", "--noEmit"];
    args.extend_from_slice(extra);
    let cmd_str = format!("npx {}", args.join(" "));
    let r = run_cmd("npx", &args, dir, container);

    let err_re = Regex::new(r"\.tsx?[\(:]").unwrap();
    let errors: Vec<String> = cap(
        r.output
            .lines()
            .filter(|l| err_re.is_match(l))
            .map(|l| l.trim().to_string())
            .collect(),
        MAX_ERRORS,
    );
    let ec = errors.len();
    let hint = if r.exit_code == -1 {
        Some(binary_missing_hint("npx"))
    } else if ec > 0 {
        Some(build_error_hint("npx tsc --noEmit"))
    } else {
        None
    };
    NpmResult {
        base: ToolOutput::new(cmd_str, r.exit_code, r.exit_code == 0, hint, &r.output),
        detail: NpmDetail::Typecheck {
            errors,
            error_count: ec,
        },
    }
}

fn run_lint(dir: &str, extra: &[&str], container: Option<&str>) -> NpmResult {
    let mut args = vec!["--no-install", "eslint", "."];
    args.extend_from_slice(extra);
    let cmd_str = format!("npx {}", args.join(" "));
    let r = run_cmd("npx", &args, dir, container);

    let line_re = Regex::new(r"\d+:\d+\s+(error|warning)").unwrap();
    let mut errors = Vec::new();
    let mut warnings = Vec::new();
    for line in r.output.lines() {
        if let Some(caps) = line_re.captures(line) {
            let trimmed = line.trim().to_string();
            if &caps[1] == "error" {
                errors.push(trimmed);
            } else {
                warnings.push(trimmed);
            }
        }
    }
    let errors = cap(errors, MAX_ERRORS);
    let warnings = cap(warnings, MAX_WARNINGS);
    let ec = errors.len();
    let wc = warnings.len();
    let hint = if r.exit_code == -1 {
        Some(binary_missing_hint("npx"))
    } else if ec > 0 || wc > 0 {
        Some(lint_hint("npx eslint ."))
    } else {
        None
    };
    NpmResult {
        base: ToolOutput::new(cmd_str, r.exit_code, r.exit_code == 0, hint, &r.output),
        detail: NpmDetail::Lint {
            errors,
            warnings,
            error_count: ec,
            warning_count: wc,
        },
    }
}

fn run_format_check(dir: &str, container: Option<&str>) -> NpmResult {
    let args = vec!["--no-install", "prettier", "--check", "."];
    let cmd_str = "npx prettier --check .".to_string();
    let r = run_cmd("npx", &args, dir, container);

    let files: Vec<String> = r
        .output
        .lines()
        .filter(|l| !l.is_empty())
        .filter(|l| !l.starts_with("Checking") && !l.starts_with("All") && !l.starts_with('['))
        .map(|l| l.trim().to_string())
        .collect();
    let formatted = files.is_empty() && r.exit_code == 0;
    let hint = if r.exit_code == -1 {
        Some(binary_missing_hint("npx"))
    } else if !formatted {
        Some(format_hint("npx prettier --write ."))
    } else {
        None
    };
    NpmResult {
        base: ToolOutput::new(cmd_str, r.exit_code, formatted, hint, &r.output),
        detail: NpmDetail::Fmt {
            formatted,
            unformatted_files: files,
        },
    }
}

fn run_deps(dir: &str, container: Option<&str>) -> NpmResult {
    let args = vec!["ls", "--json", "--depth=0"];
    let cmd_str = "npm ls --json --depth=0".to_string();
    let r = run_cmd("npm", &args, dir, container);

    let mut deps = Vec::new();
    if let Ok(val) = serde_json::from_str::<serde_json::Value>(&r.output) {
        if let Some(obj) = val.get("dependencies").and_then(|d| d.as_object()) {
            let mut entries: Vec<_> = obj.iter().collect();
            entries.sort_by_key(|(k, _)| k.to_string());
            for (name, info) in entries {
                let version = info
                    .get("version")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown")
                    .to_string();
                deps.push(DepEntry {
                    name: name.clone(),
                    version,
                });
            }
        }
    }

    let total = deps.len();
    let hint = if r.exit_code == -1 {
        Some(binary_missing_hint("npm"))
    } else if total == 0 && r.exit_code != 0 {
        Some("Run `npm install` to install dependencies.".to_string())
    } else {
        None
    };
    NpmResult {
        base: ToolOutput::new(cmd_str, r.exit_code, true, hint, &r.output),
        detail: NpmDetail::Deps {
            dependencies: deps,
            total,
        },
    }
}

fn run_build(dir: &str, extra: &[&str], container: Option<&str>) -> NpmResult {
    let mut args = vec!["run", "build"];
    args.extend_from_slice(extra);
    let cmd_str = format!("npm {}", args.join(" "));
    let r = run_cmd("npm", &args, dir, container);

    let errors: Vec<String> = cap(
        r.output
            .lines()
            .filter(|l| l.to_lowercase().contains("error"))
            .map(|l| l.trim().to_string())
            .collect(),
        MAX_ERRORS,
    );
    let ec = errors.len();
    let hint = if r.exit_code == -1 {
        Some(binary_missing_hint("npm"))
    } else if r.exit_code != 0 {
        Some(build_error_hint("npm run build"))
    } else {
        None
    };
    NpmResult {
        base: ToolOutput::new(cmd_str, r.exit_code, r.exit_code == 0, hint, &r.output),
        detail: NpmDetail::Build {
            errors,
            error_count: ec,
        },
    }
}

fn run_audit(dir: &str, container: Option<&str>) -> NpmResult {
    let args = vec!["audit", "--json"];
    let cmd_str = "npm audit --json".to_string();
    let r = run_cmd("npm", &args, dir, container);

    let mut vulns = VulnSummary {
        critical: 0,
        high: 0,
        moderate: 0,
        low: 0,
        info: 0,
    };

    if let Ok(val) = serde_json::from_str::<serde_json::Value>(&r.output) {
        if let Some(v) = val.pointer("/metadata/vulnerabilities") {
            vulns.critical = v.get("critical").and_then(|n| n.as_u64()).unwrap_or(0) as u32;
            vulns.high = v.get("high").and_then(|n| n.as_u64()).unwrap_or(0) as u32;
            vulns.moderate = v.get("moderate").and_then(|n| n.as_u64()).unwrap_or(0) as u32;
            vulns.low = v.get("low").and_then(|n| n.as_u64()).unwrap_or(0) as u32;
            vulns.info = v.get("info").and_then(|n| n.as_u64()).unwrap_or(0) as u32;
        }
    }

    let total = (vulns.critical + vulns.high + vulns.moderate + vulns.low + vulns.info) as usize;
    let success = total == 0;
    let hint = if r.exit_code == -1 {
        Some(binary_missing_hint("npm"))
    } else if vulns.critical > 0 || vulns.high > 0 {
        Some(
            "Critical/high vulnerabilities found. Run `npm audit fix` or update affected packages."
                .to_string(),
        )
    } else if !success {
        Some("Vulnerabilities found. Run `npm audit fix` to auto-fix where possible.".to_string())
    } else {
        None
    };
    NpmResult {
        base: ToolOutput::new(cmd_str, r.exit_code, success, hint, &r.output),
        detail: NpmDetail::Audit {
            vulnerabilities: vulns,
            total,
        },
    }
}
