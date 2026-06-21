//! Shared command-runner infrastructure with Docker container support,
//! output truncation, and list capping.

use serde::Serialize;
use std::process::Command;

/// Result of running a shell command.
pub struct CmdResult {
    pub exit_code: i32,
    pub output: String, // combined stdout+stderr
}

/// Run a command locally or inside a Docker container.
///
/// When `container` is `Some`, the command is rewritten to:
///   `docker exec -w <dir> <container> <binary> <args...>`
///
/// stdout and stderr are merged (matching the Erlang originals).
pub fn run_cmd(binary: &str, args: &[&str], dir: &str, container: Option<&str>) -> CmdResult {
    let result = if let Some(ctr) = container {
        let mut docker_args: Vec<&str> = vec!["exec", "-w", dir, ctr, binary];
        docker_args.extend_from_slice(args);
        Command::new("docker").args(&docker_args).output()
    } else {
        Command::new(binary).args(args).current_dir(dir).output()
    };

    match result {
        Ok(out) => {
            let mut combined = String::from_utf8_lossy(&out.stdout).into_owned();
            let stderr = String::from_utf8_lossy(&out.stderr);
            if !stderr.is_empty() {
                if !combined.is_empty() {
                    combined.push('\n');
                }
                combined.push_str(&stderr);
            }
            CmdResult {
                exit_code: out.status.code().unwrap_or(-1),
                output: combined,
            }
        }
        Err(e) => CmdResult {
            exit_code: -1,
            output: format!("failed to run {binary}: {e}"),
        },
    }
}

/// Truncate a string to `max` bytes, appending a marker if truncated.
pub fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    // Find a valid UTF-8 boundary at or before `max`.
    let end = s.floor_char_boundary(max);
    format!("{}… (truncated)", &s[..end])
}

/// Keep at most `max` items from a Vec.
pub fn cap<T>(items: Vec<T>, max: usize) -> Vec<T> {
    items.into_iter().take(max).collect()
}

/// Split an extra-args string on whitespace, returning an empty vec for "".
pub fn split_args(s: &str) -> Vec<String> {
    if s.is_empty() {
        vec![]
    } else {
        s.split_whitespace().map(|s| s.to_string()).collect()
    }
}

/// The maximum number of error/warning items to include.
pub const MAX_ERRORS: usize = 20;
pub const MAX_WARNINGS: usize = 30;
/// Maximum raw output bytes to include on failure.
/// Increased from 3000 to 8000 to accommodate test failure stack traces.
pub const MAX_RAW: usize = 8000;
/// Maximum raw output bytes for test failures (larger to include full stack traces).
pub const MAX_RAW_TEST: usize = 16000;

/// Generate a hint for when a binary is not found (exit_code -1).
pub fn binary_missing_hint(binary: &str) -> String {
    format!("'{binary}' not found on PATH. Install it or run the command directly via Bash.")
}

/// Generate a hint for compilation/build errors.
pub fn build_error_hint(tool: &str) -> String {
    format!(
        "Fix the first error shown above, then re-run via the MCP tool (not Bash). \
         Errors cascade — fixing the first often resolves later ones. \
         Re-run: `{tool}`"
    )
}

/// Generate a hint for test failures.
pub fn test_failure_hint(tool: &str) -> String {
    format!(
        "Read the failure details to identify the root cause. \
         Fix the failing code (not the test unless the test is wrong), \
         then re-run via the MCP tool: `{tool}`"
    )
}

/// Generate a hint for lint/vet issues.
pub fn lint_hint(tool: &str) -> String {
    format!(
        "Fix the reported issues in priority order (errors before warnings). \
         Re-run via the MCP tool (not Bash) to verify: `{tool}`. \
         Always run fmt + clippy/lint before staging files for commit."
    )
}

/// Generate a hint for format check failures.
pub fn format_hint(fix_cmd: &str) -> String {
    format!(
        "Run `{fix_cmd}` to auto-fix formatting, then re-stage changed files before committing. \
         Alternatively, use the format_fix MCP tool. \
         Always format before staging — pre-commit hooks will reject unformatted code."
    )
}

/// Fallback hint when a command failed but no specific hint was generated.
/// Ensures the LLM always gets actionable guidance rather than silently stalling.
pub fn fallback_hint(tool: &str, exit_code: i32) -> String {
    format!(
        "Command `{tool}` failed (exit code {exit_code}). \
         Check the raw_output field for error details. \
         Fix the issue and re-run via the MCP tool."
    )
}

/// Ensure `hint` is populated on failure. If the caller left it `None` and the
/// command failed, fill in a generic fallback so the LLM always has guidance.
pub fn ensure_hint(hint: Option<String>, cmd: &str, exit_code: i32) -> Option<String> {
    if hint.is_some() {
        return hint;
    }
    if exit_code != 0 {
        Some(fallback_hint(cmd, exit_code))
    } else {
        None
    }
}

/// Ensure `raw_output` is populated on any non-zero exit code.
/// The LLM needs the actual error text to diagnose unknown failures.
pub fn ensure_raw_output(raw: Option<String>, output: &str, exit_code: i32) -> Option<String> {
    if raw.is_some() {
        return raw;
    }
    if exit_code != 0 {
        Some(truncate(output, MAX_RAW))
    } else {
        None
    }
}

/// Common base fields for every tool result. The `new()` constructor
/// guarantees that `hint` and `raw_output` are always populated on failure,
/// so individual tools never need to remember to call ensure_*.
#[derive(Debug, Serialize)]
pub struct ToolOutput {
    pub command: String,
    pub exit_code: i32,
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw_output: Option<String>,
    /// Size of the raw command output before structuring (not serialized).
    /// Used by the MCP analytics layer to compute token savings.
    #[serde(skip)]
    pub raw_input_bytes: usize,
}

impl ToolOutput {
    pub fn new(
        command: String,
        exit_code: i32,
        success: bool,
        hint: Option<String>,
        raw_output: &str,
    ) -> Self {
        Self {
            hint: ensure_hint(hint, &command, exit_code),
            raw_output: ensure_raw_output(None, raw_output, exit_code),
            raw_input_bytes: raw_output.len(),
            command,
            exit_code,
            success,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_short_string() {
        assert_eq!(truncate("hello", 100), "hello");
    }

    #[test]
    fn truncate_long_string() {
        let s = "a".repeat(100);
        let result = truncate(&s, 50);
        assert!(result.len() < 70); // 50 + marker
        assert!(result.ends_with("… (truncated)"));
    }

    #[test]
    fn cap_limits_items() {
        let v = vec![1, 2, 3, 4, 5];
        assert_eq!(cap(v, 3), vec![1, 2, 3]);
    }

    #[test]
    fn split_args_empty() {
        assert!(split_args("").is_empty());
    }

    #[test]
    fn split_args_multiple() {
        assert_eq!(
            split_args("--verbose --color"),
            vec!["--verbose", "--color"]
        );
    }

    #[test]
    fn run_cmd_local() {
        let r = run_cmd("echo", &["hello"], ".", None);
        assert_eq!(r.exit_code, 0);
        assert!(r.output.contains("hello"));
    }

    #[test]
    fn run_cmd_bad_binary() {
        let r = run_cmd("nonexistent_binary_xyz", &[], ".", None);
        assert_ne!(r.exit_code, 0);
        assert!(r.output.contains("failed to run"));
    }
}
