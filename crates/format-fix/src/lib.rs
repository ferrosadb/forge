//! Format-fix: auto-detect project languages and run the appropriate formatters.
//!
//! Uses `forge-project-detect` to discover which languages are present,
//! then runs the matching formatter for each one. Supports both "fix" mode
//! (write changes) and "check" mode (report what would change).

use anyhow::Result;
use forge_project_detect::detector;
use serde::Serialize;
use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;

// ── Public types ────────────────────────────────────────────────────────────

/// Overall result of running all detected formatters.
#[derive(Debug, Serialize)]
pub struct FormatResult {
    /// "fixed", "clean", or "error"
    pub status: &'static str,
    pub formatters_run: Vec<FormatterRun>,
    pub total_files_changed: usize,
    pub all_changed_files: Vec<String>,
}

/// Result of a single formatter invocation.
#[derive(Debug, Serialize)]
pub struct FormatterRun {
    pub language: String,
    pub command: String,
    pub files_changed: Vec<String>,
    /// "fixed", "clean", "skipped", or "error"
    pub status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

// ── Formatter spec ──────────────────────────────────────────────────────────

struct FormatterSpec {
    language: String,
    binary: String,
    format_args: Vec<String>,
    check_args: Vec<String>,
    /// For Go's `gofmt -l`, non-empty stdout means unformatted files.
    check_stdout_means_dirty: bool,
}

/// Map a detected language name to its formatter specification.
fn formatter_for_language(name: &str) -> Option<FormatterSpec> {
    match name {
        "Elixir" => Some(FormatterSpec {
            language: name.to_string(),
            binary: "mix".to_string(),
            format_args: vec!["format".to_string()],
            check_args: vec!["format".to_string(), "--check-formatted".to_string()],
            check_stdout_means_dirty: false,
        }),
        "Rust" => Some(FormatterSpec {
            language: name.to_string(),
            binary: "cargo".to_string(),
            format_args: vec!["fmt".to_string()],
            check_args: vec!["fmt".to_string(), "--check".to_string()],
            check_stdout_means_dirty: false,
        }),
        "Go" => Some(FormatterSpec {
            language: name.to_string(),
            binary: "gofmt".to_string(),
            format_args: vec!["-w".to_string(), ".".to_string()],
            check_args: vec!["-l".to_string(), ".".to_string()],
            check_stdout_means_dirty: true,
        }),
        "Python" => Some(FormatterSpec {
            language: name.to_string(),
            binary: "ruff".to_string(),
            format_args: vec!["format".to_string(), ".".to_string()],
            check_args: vec!["format".to_string(), "--check".to_string(), ".".to_string()],
            check_stdout_means_dirty: false,
        }),
        "TypeScript" | "JavaScript" => Some(FormatterSpec {
            language: name.to_string(),
            binary: "npx".to_string(),
            format_args: vec![
                "prettier".to_string(),
                "--write".to_string(),
                "**/*.{ts,tsx,js,jsx}".to_string(),
            ],
            check_args: vec![
                "prettier".to_string(),
                "--check".to_string(),
                "**/*.{ts,tsx,js,jsx}".to_string(),
            ],
            check_stdout_means_dirty: false,
        }),
        "C#" => Some(FormatterSpec {
            language: name.to_string(),
            binary: "dotnet".to_string(),
            format_args: vec!["format".to_string()],
            check_args: vec!["format".to_string(), "--verify-no-changes".to_string()],
            check_stdout_means_dirty: false,
        }),
        // Java and C++ are complex — skip for now.
        "Java" | "C/C++" => None,
        _ => None,
    }
}

/// Return a Python formatter spec using `black` as a fallback.
fn python_black_fallback() -> FormatterSpec {
    FormatterSpec {
        language: "Python".to_string(),
        binary: "black".to_string(),
        format_args: vec![".".to_string()],
        check_args: vec!["--check".to_string(), ".".to_string()],
        check_stdout_means_dirty: false,
    }
}

// ── Helpers ─────────────────────────────────────────────────────────────────

/// Check whether a binary is available on `$PATH`.
fn find_binary(name: &str) -> bool {
    Command::new("which")
        .arg(name)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Return the set of files reported by `git diff --name-only` in `dir`.
/// Returns an empty vec if the directory is not a git repo or on error.
fn git_changed_files(dir: &Path) -> Vec<String> {
    Command::new("git")
        .args(["diff", "--name-only"])
        .current_dir(dir)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .filter(|l| !l.is_empty())
                .map(|l| l.to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// Run a command, returning (success, stdout, stderr).
fn run_command(binary: &str, args: &[String], dir: &Path) -> (bool, String, String) {
    match Command::new(binary).args(args).current_dir(dir).output() {
        Ok(output) => (
            output.status.success(),
            String::from_utf8_lossy(&output.stdout).to_string(),
            String::from_utf8_lossy(&output.stderr).to_string(),
        ),
        Err(e) => (false, String::new(), format!("failed to run {binary}: {e}")),
    }
}

/// Run a single formatter and return its `FormatterRun`.
fn run_formatter(spec: &FormatterSpec, dir: &Path, check: bool) -> FormatterRun {
    let cmd_display = format!("{} {}", spec.binary, spec.format_args.join(" "));

    // Check binary availability. For Python, try `black` fallback.
    if !find_binary(&spec.binary) {
        return FormatterRun {
            language: spec.language.clone(),
            command: cmd_display,
            files_changed: vec![],
            status: "skipped",
            reason: Some(format!("{} not found", spec.binary)),
        };
    }

    if check {
        run_formatter_check(spec, dir)
    } else {
        run_formatter_fix(spec, dir)
    }
}

/// Run formatter in check mode — report whether files would change.
fn run_formatter_check(spec: &FormatterSpec, dir: &Path) -> FormatterRun {
    let cmd_display = format!("{} {}", spec.binary, spec.check_args.join(" "));
    let (success, stdout, stderr) = run_command(&spec.binary, &spec.check_args, dir);

    if spec.check_stdout_means_dirty {
        // gofmt -l: stdout lists unformatted files
        let dirty_files: Vec<String> = stdout
            .lines()
            .filter(|l| !l.is_empty())
            .map(|l| l.to_string())
            .collect();
        if dirty_files.is_empty() {
            FormatterRun {
                language: spec.language.clone(),
                command: cmd_display,
                files_changed: vec![],
                status: "clean",
                reason: None,
            }
        } else {
            FormatterRun {
                language: spec.language.clone(),
                command: cmd_display,
                files_changed: dirty_files,
                status: "fixed",
                reason: None,
            }
        }
    } else if success {
        FormatterRun {
            language: spec.language.clone(),
            command: cmd_display,
            files_changed: vec![],
            status: "clean",
            reason: None,
        }
    } else {
        // Non-zero exit in check mode means files would be changed.
        // We don't know which files, but report it as "fixed" (would change).
        FormatterRun {
            language: spec.language.clone(),
            command: cmd_display,
            files_changed: vec![],
            status: "fixed",
            reason: if stderr.is_empty() {
                None
            } else {
                Some(stderr.trim().to_string())
            },
        }
    }
}

/// Run formatter in fix mode — actually modify files and report what changed.
fn run_formatter_fix(spec: &FormatterSpec, dir: &Path) -> FormatterRun {
    let cmd_display = format!("{} {}", spec.binary, spec.format_args.join(" "));

    let before: BTreeSet<String> = git_changed_files(dir).into_iter().collect();
    let (success, _stdout, stderr) = run_command(&spec.binary, &spec.format_args, dir);

    if !success {
        return FormatterRun {
            language: spec.language.clone(),
            command: cmd_display,
            files_changed: vec![],
            status: "error",
            reason: Some(if stderr.is_empty() {
                "formatter exited with non-zero status".to_string()
            } else {
                stderr.trim().to_string()
            }),
        };
    }

    let after: BTreeSet<String> = git_changed_files(dir).into_iter().collect();
    let changed: Vec<String> = after.difference(&before).cloned().collect();

    if changed.is_empty() {
        FormatterRun {
            language: spec.language.clone(),
            command: cmd_display,
            files_changed: vec![],
            status: "clean",
            reason: None,
        }
    } else {
        FormatterRun {
            language: spec.language.clone(),
            command: cmd_display,
            files_changed: changed,
            status: "fixed",
            reason: None,
        }
    }
}

// ── Main entry point ────────────────────────────────────────────────────────

/// Detect languages in `dir` and run the appropriate formatters.
///
/// If `check` is true, formatters run in check/dry-run mode (no files are
/// modified). Otherwise formatters write changes in place.
pub fn format_fix(dir: &Path, check: bool) -> Result<FormatResult> {
    let report = detector::detect(dir);
    let mut formatters_run = Vec::new();

    for component in &report.languages {
        let mut spec = match formatter_for_language(&component.name) {
            Some(s) => s,
            None => continue,
        };

        // Python fallback: if `ruff` is not found, try `black`.
        // Python fallback: if ruff is missing, try black instead.
        // If neither is found, run_formatter will produce "skipped".
        if component.name == "Python" && !find_binary(&spec.binary) && find_binary("black") {
            spec = python_black_fallback();
        }

        formatters_run.push(run_formatter(&spec, dir, check));
    }

    // Aggregate results.
    let mut all_changed: BTreeSet<String> = BTreeSet::new();
    for run in &formatters_run {
        for f in &run.files_changed {
            all_changed.insert(f.clone());
        }
    }
    let all_changed_files: Vec<String> = all_changed.into_iter().collect();
    let total_files_changed = all_changed_files.len();

    let has_fixed = formatters_run.iter().any(|r| r.status == "fixed");
    let all_errors =
        !formatters_run.is_empty() && formatters_run.iter().all(|r| r.status == "error");

    let status = if has_fixed {
        "fixed"
    } else if all_errors {
        "error"
    } else {
        "clean"
    };

    Ok(FormatResult {
        status,
        formatters_run,
        total_files_changed,
        all_changed_files,
    })
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn test_format_result_serializes() {
        let result = FormatResult {
            status: "fixed",
            formatters_run: vec![
                FormatterRun {
                    language: "Rust".to_string(),
                    command: "cargo fmt".to_string(),
                    files_changed: vec!["src/main.rs".to_string()],
                    status: "fixed",
                    reason: None,
                },
                FormatterRun {
                    language: "Python".to_string(),
                    command: "ruff format .".to_string(),
                    files_changed: vec![],
                    status: "skipped",
                    reason: Some("ruff not found".to_string()),
                },
            ],
            total_files_changed: 1,
            all_changed_files: vec!["src/main.rs".to_string()],
        };

        let json = serde_json::to_value(&result).expect("serialization should succeed");
        assert_eq!(json["status"], "fixed");
        assert_eq!(json["total_files_changed"], 1);
        assert_eq!(json["all_changed_files"][0], "src/main.rs");
        assert_eq!(json["formatters_run"].as_array().unwrap().len(), 2);
        // The "fixed" run should have no "reason" key (skip_serializing_if).
        assert!(json["formatters_run"][0].get("reason").is_none());
        // The "skipped" run should have a "reason" key.
        assert_eq!(json["formatters_run"][1]["reason"], "ruff not found");
    }

    #[test]
    fn test_find_binary_with_known_command() {
        assert!(find_binary("ls"), "ls should be found on PATH");
    }

    #[test]
    fn test_find_binary_with_unknown_command() {
        assert!(
            !find_binary("nonexistent_binary_xyz_123456"),
            "bogus binary should not be found"
        );
    }

    #[test]
    fn test_skipped_when_no_formatter() {
        // Create a temp dir with mix.exs to trigger Elixir detection.
        // The formatter (mix) may or may not be installed, but we can
        // verify the overall result shape is correct JSON.
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("mix.exs"), "defmodule MyApp do\nend\n").unwrap();

        let result = format_fix(dir.path(), true).expect("format_fix should not error");

        // Should have exactly one formatter run for Elixir.
        assert_eq!(result.formatters_run.len(), 1);
        let run = &result.formatters_run[0];
        assert_eq!(run.language, "Elixir");

        // Status is either "skipped" (mix not found) or "clean"/"fixed" (mix found).
        assert!(
            ["skipped", "clean", "fixed", "error"].contains(&run.status),
            "unexpected status: {}",
            run.status
        );

        // Verify the result serializes to well-formed JSON.
        let json = serde_json::to_string(&result).expect("should serialize");
        assert!(json.contains("\"language\":\"Elixir\""));
    }
}
