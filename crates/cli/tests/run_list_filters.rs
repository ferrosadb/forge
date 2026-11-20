//! `frg run --list-filters` lists the active filter rules and exits, so it
//! needs no command to run.

use std::process::Command;

#[test]
fn list_filters_needs_no_command() {
    let output = Command::new(env!("CARGO_BIN_EXE_frg"))
        .args(["run", "--list-filters"])
        .output()
        .expect("run frg");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("--list-filters prints JSON");
    assert!(report.get("filters").is_some(), "stdout: {report}");
}
