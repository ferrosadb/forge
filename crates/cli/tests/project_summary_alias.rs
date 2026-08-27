//! Integration tests for project summary command aliases.

use serde_json::Value;
use std::fs;
use std::process::Command;
use tempfile::TempDir;

fn fixture_project() -> TempDir {
    let temp_dir = TempDir::new().expect("failed to create temp project");
    fs::write(
        temp_dir.path().join("Cargo.toml"),
        r#"[package]
name = "fixture"
version = "0.1.0"
edition = "2021"
"#,
    )
    .expect("failed to write Cargo.toml");

    let src_dir = temp_dir.path().join("src");
    fs::create_dir(&src_dir).expect("failed to create src directory");
    fs::write(src_dir.join("lib.rs"), "pub fn answer() -> u8 { 42 }\n")
        .expect("failed to write lib.rs");

    temp_dir
}

fn run_json(args: &[&str]) -> Value {
    let output = Command::new(env!("CARGO_BIN_EXE_frg"))
        .args(args)
        .output()
        .expect("failed to execute frg");

    assert!(
        output.status.success(),
        "frg failed with stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    serde_json::from_slice(&output.stdout).expect("frg output was not valid JSON")
}

#[test]
fn project_summary_alias_matches_project_detect_summary() {
    let temp_dir = fixture_project();
    let path = temp_dir.path().to_str().expect("temp path is not UTF-8");

    let canonical = run_json(&["project-detect", "--summary", path]);
    let alias = run_json(&["project_summary", path]);

    assert_eq!(alias, canonical);
}

#[test]
fn project_summary_hyphen_alias_matches_project_detect_summary() {
    let temp_dir = fixture_project();
    let path = temp_dir.path().to_str().expect("temp path is not UTF-8");

    let canonical = run_json(&["project-detect", "--summary", path]);
    let alias = run_json(&["project-summary", path]);

    assert_eq!(alias, canonical);
}
