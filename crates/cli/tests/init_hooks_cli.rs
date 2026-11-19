//! Binary-level regression tests for #20: `frg init --show` and
//! `frg init --uninstall` find Forge's hook by its `frg hook` command.
//! Each test points HOME at a temporary directory, so the real
//! ~/.claude/settings.json is never read or written.

use std::path::Path;
use std::process::{Command, Output};

fn frg(home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_frg"))
        .args(args)
        .env("HOME", home)
        .output()
        .unwrap()
}

fn settings_path(home: &Path) -> std::path::PathBuf {
    home.join(".claude").join("settings.json")
}

#[test]
fn show_reports_current_right_after_global_init() {
    let home = tempfile::tempdir().unwrap();
    assert!(frg(home.path(), &["init", "--global"]).status.success());

    let output = frg(home.path(), &["init", "--global", "--show"]);
    let status: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(status["forge_hooks_installed"], true);
    assert_eq!(status["hook_status"], "current");
}

#[test]
fn show_does_not_count_the_mcp_server_entry_as_a_hook() {
    let home = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(home.path().join(".claude")).unwrap();
    std::fs::write(
        settings_path(home.path()),
        r#"{"mcpServers":{"forge":{"command":"frg","args":["--mcp"]}}}"#,
    )
    .unwrap();

    let output = frg(home.path(), &["init", "--global", "--show"]);
    let status: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(status["forge_hooks_installed"], false);
    assert_eq!(status["hook_status"], "not_installed");
}

#[test]
fn global_uninstall_removes_forge_hooks_and_keeps_the_users() {
    let home = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(home.path().join(".claude")).unwrap();
    std::fs::write(
        settings_path(home.path()),
        r#"{
  "hooks": {
    "PreToolUse": [
      {"matcher": "Read", "hooks": [{"type": "command", "command": "frg hook 2>/dev/null || true"}]}
    ],
    "PostToolUse": [
      {"matcher": "Read", "hooks": [{"type": "command", "command": "frg hook 2>/dev/null || true"}]},
      {"matcher": "Write", "hooks": [{"type": "command", "command": "~/bin/format.sh"}]}
    ]
  },
  "mcpServers": {"forge": {"command": "frg", "args": ["--mcp"]}}
}"#,
    )
    .unwrap();

    assert!(frg(home.path(), &["init", "--global", "--uninstall"])
        .status
        .success());

    let text = std::fs::read_to_string(settings_path(home.path())).unwrap();
    let settings: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert!(
        !text.contains("frg hook"),
        "Forge hooks left behind: {text}"
    );
    assert_eq!(
        settings["hooks"]["PostToolUse"][0]["hooks"][0]["command"],
        "~/bin/format.sh"
    );
    assert_eq!(settings["mcpServers"]["forge"]["command"], "frg");
}
