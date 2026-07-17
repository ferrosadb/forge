//! Binary-level contracts for `frg sheet` subcommands.
//!
//! `resolve_alias` is required to run FIRST in every `sheet` dispatch arm
//! (CLI and MCP), before any OAuth/CQL/network call — see
//! `forge_sheet_sync::resolve_alias`'s doc. This test exercises that
//! contract at the binary boundary: a nonexistent alias must fail loud
//! (non-zero exit, no panic, error names the alias) without needing a live
//! OAuth client or CQL cluster.

use std::path::Path;
use std::process::{Command, Output};

fn frg(root: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_frg"))
        .current_dir(root)
        .args(args)
        .output()
        .unwrap()
}

#[test]
fn sheet_pull_nonexistent_alias_fails_loud() {
    let temp = tempfile::TempDir::new().unwrap();
    let output = frg(temp.path(), &["sheet", "pull", "__no_such_alias__"]);

    assert!(
        !output.status.success(),
        "expected non-zero exit for a missing alias, got success. stdout: {}, stderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        combined.contains("__no_such_alias__"),
        "expected the error to name the missing alias, got: {combined}"
    );
    assert!(
        combined.to_lowercase().contains("not found"),
        "expected the error to say the mapping was not found, got: {combined}"
    );
}

#[test]
fn sheet_push_nonexistent_alias_fails_loud() {
    let temp = tempfile::TempDir::new().unwrap();
    let output = frg(
        temp.path(),
        &["sheet", "push", "__no_such_alias__", "ROW-1"],
    );

    assert!(
        !output.status.success(),
        "expected non-zero exit for a missing alias, got success. stdout: {}, stderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        combined.contains("__no_such_alias__"),
        "expected the error to name the missing alias, got: {combined}"
    );
}

#[test]
fn sheet_auth_nonexistent_alias_fails_loud() {
    let temp = tempfile::TempDir::new().unwrap();
    let output = frg(temp.path(), &["sheet", "auth", "__no_such_alias__"]);

    assert!(
        !output.status.success(),
        "expected non-zero exit for a missing alias, got success. stdout: {}, stderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        combined.contains("__no_such_alias__"),
        "expected the error to name the missing alias, got: {combined}"
    );
}

#[test]
fn sheet_help_renders() {
    let temp = tempfile::TempDir::new().unwrap();
    for args in [
        vec!["sheet", "--help"],
        vec!["sheet", "pull", "--help"],
        vec!["sheet", "push", "--help"],
        vec!["sheet", "auth", "--help"],
    ] {
        let output = frg(temp.path(), &args);
        assert!(
            output.status.success(),
            "expected `{}` to succeed, stderr: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
