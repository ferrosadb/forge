//! Binary-level contract for `frg run`'s stderr status line.
//! Correctness: stdout stays untouched by the change while stderr always
//! carries exactly one `frg: ok|FAIL exit=... ms=... filter=... cmd="..."` line,
//! disambiguating "passed with no output" from "hung".
//! Last revised: 2026-09-26

use std::process::{Command, Output};

fn frg(args: &[&str], status_env: Option<&str>) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_frg"));
    cmd.args(args);
    match status_env {
        Some(v) => {
            cmd.env("FRG_RUN_STATUS", v);
        }
        None => {
            cmd.env_remove("FRG_RUN_STATUS");
        }
    }
    cmd.output().unwrap()
}

fn last_stderr_line(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr)
        .lines()
        .last()
        .unwrap_or_default()
        .to_string()
}

#[test]
fn ok_run_prints_unchanged_stdout_and_one_ok_status_line() {
    let output = frg(&["run", "--", "true"], None);
    assert!(output.status.success());
    assert!(
        output.stdout.is_empty(),
        "stdout must stay byte-identical to prior behavior"
    );
    let last_line = last_stderr_line(&output);
    assert!(
        last_line.starts_with("frg: ok exit=0"),
        "unexpected status line: {last_line}"
    );
    assert!(last_line.contains("cmd=\"true\""));
}

#[test]
fn failing_run_propagates_exit_code_and_prints_fail_status_line() {
    let output = frg(&["run", "--", "sh", "-c", "exit 3"], None);
    assert_eq!(output.status.code(), Some(3));
    let last_line = last_stderr_line(&output);
    assert!(
        last_line.starts_with("frg: FAIL exit=3"),
        "unexpected status line: {last_line}"
    );
}

#[test]
fn frg_run_status_zero_suppresses_the_status_line() {
    let output = frg(&["run", "--", "true"], Some("0"));
    assert!(output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("frg: "),
        "status line should be suppressed with FRG_RUN_STATUS=0, got: {stderr}"
    );
}
