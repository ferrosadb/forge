//! t_92c2dcc0: `frg init` must never drop another tool's hooks.
//!
//! Install merges Forge's two Read hook entries in by exact identity, and
//! uninstall removes only entries that are exactly what Forge installs. Every
//! test points HOME at a temporary directory, so the real
//! ~/.claude/settings.json is never read or written.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;

const FORGE_READ_ENTRY: &str = r#"{"matcher": "Read", "hooks": [{"type": "command", "command": "frg hook 2>/dev/null || true"}]}"#;

/// Another tool's settings, written the way serde pretty-prints so a round
/// trip can be compared byte for byte. Includes a hook whose path contains
/// "forge" but is not Forge's, and an entry that mixes `frg hook` with the
/// user's own hook (Forge never writes a mixed entry, so it is not Forge's).
const FOREIGN_SETTINGS: &str = r#"{
  "model": "opus",
  "permissions": {
    "allow": [
      "Bash(git status)"
    ]
  },
  "hooks": {
    "PreToolUse": [
      {
        "matcher": "Bash",
        "hooks": [
          {
            "type": "command",
            "command": "python3 ~/.claude/hooks/require-forge.py"
          }
        ]
      },
      {
        "matcher": "Bash",
        "hooks": [
          {
            "type": "command",
            "command": "python3 ~/.claude/hooks/ask-before-oom-waiver.py",
            "timeout": 5
          }
        ]
      },
      {
        "matcher": "Bash",
        "hooks": [
          {
            "type": "command",
            "command": "python3 ~/.claude/hooks/no-gnu-timeout.py"
          }
        ]
      },
      {
        "matcher": "Bash",
        "hooks": [
          {
            "type": "command",
            "command": "/Users/someone/src/research/hooks/git-destructive-guard.sh"
          }
        ]
      }
    ],
    "PostToolUse": [
      {
        "matcher": "Read",
        "hooks": [
          {
            "type": "command",
            "command": "~/.claude/hooks/forge-read-audit.sh"
          }
        ]
      },
      {
        "matcher": "Read",
        "hooks": [
          {
            "type": "command",
            "command": "frg hook 2>/dev/null || true"
          },
          {
            "type": "command",
            "command": "~/bin/audit-read.sh"
          }
        ]
      }
    ],
    "PostToolUseFailure": [
      {
        "matcher": "Bash",
        "hooks": [
          {
            "type": "command",
            "command": "bash ~/.claude/hooks/forge-failure-hint.sh"
          }
        ]
      }
    ]
  },
  "statusLine": {
    "type": "command",
    "command": "~/.claude/statusline.sh"
  }
}
"#;

fn frg(home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_frg"))
        .args(args)
        .env("HOME", home)
        .current_dir(home)
        .output()
        .unwrap()
}

fn settings_path(home: &Path) -> PathBuf {
    home.join(".claude").join("settings.json")
}

fn write_settings(home: &Path, text: &str) {
    std::fs::create_dir_all(home.join(".claude")).unwrap();
    std::fs::write(settings_path(home), text).unwrap();
}

fn read(home: &Path) -> String {
    std::fs::read_to_string(settings_path(home)).unwrap()
}

fn forge_entry() -> Value {
    serde_json::from_str(FORGE_READ_ENTRY).unwrap()
}

fn ok(output: &Output) {
    assert!(
        output.status.success(),
        "frg failed: {}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// The settings with every exact Forge entry taken out, re-serialized.
fn without_forge_entries(text: &str) -> String {
    let mut settings: Value = serde_json::from_str(text).unwrap();
    for phase in ["PreToolUse", "PostToolUse"] {
        if let Some(entries) = settings["hooks"][phase].as_array_mut() {
            entries.retain(|entry| *entry != forge_entry());
        }
    }
    serde_json::to_string_pretty(&settings).unwrap() + "\n"
}

fn count_forge_entries(text: &str, phase: &str) -> usize {
    let settings: Value = serde_json::from_str(text).unwrap();
    settings["hooks"][phase].as_array().map_or(0, |entries| {
        entries.iter().filter(|e| **e == forge_entry()).count()
    })
}

fn backup_dirs(home: &Path) -> Vec<PathBuf> {
    let root = home.join(".ferrosa/backups/harness");
    let Ok(entries) = std::fs::read_dir(&root) else {
        return Vec::new();
    };
    let mut dirs: Vec<PathBuf> = entries.map(|e| e.unwrap().path()).collect();
    dirs.sort();
    dirs
}

#[test]
fn install_keeps_every_other_tools_hook_exactly() {
    let home = tempfile::tempdir().unwrap();
    write_settings(home.path(), FOREIGN_SETTINGS);

    ok(&frg(home.path(), &["init", "--global"]));

    let after = read(home.path());
    assert_eq!(count_forge_entries(&after, "PreToolUse"), 1, "{after}");
    assert_eq!(count_forge_entries(&after, "PostToolUse"), 1, "{after}");
    assert_eq!(
        without_forge_entries(&after),
        FOREIGN_SETTINGS,
        "install changed something that is not Forge's"
    );
}

#[test]
fn install_then_uninstall_restores_the_file_byte_for_byte() {
    let home = tempfile::tempdir().unwrap();
    write_settings(home.path(), FOREIGN_SETTINGS);

    ok(&frg(home.path(), &["init", "--global"]));
    ok(&frg(home.path(), &["init", "--global", "--uninstall"]));

    assert_eq!(read(home.path()), FOREIGN_SETTINGS);
}

#[test]
fn uninstall_leaves_a_file_without_forge_entries_untouched() {
    let home = tempfile::tempdir().unwrap();
    write_settings(home.path(), FOREIGN_SETTINGS);

    ok(&frg(home.path(), &["init", "--global", "--uninstall"]));

    assert_eq!(read(home.path()), FOREIGN_SETTINGS);
    assert!(
        backup_dirs(home.path()).is_empty(),
        "nothing changed, so nothing to back up"
    );
}

#[test]
fn a_second_install_writes_nothing() {
    let home = tempfile::tempdir().unwrap();
    write_settings(home.path(), FOREIGN_SETTINGS);
    ok(&frg(home.path(), &["init", "--global"]));
    let first = read(home.path());
    let backups = backup_dirs(home.path()).len();

    ok(&frg(home.path(), &["init", "--global"]));

    assert_eq!(read(home.path()), first);
    assert_eq!(backup_dirs(home.path()).len(), backups);
}

#[test]
fn install_backs_up_the_original_first() {
    let home = tempfile::tempdir().unwrap();
    write_settings(home.path(), FOREIGN_SETTINGS);

    ok(&frg(home.path(), &["init", "--global"]));

    let dirs = backup_dirs(home.path());
    assert_eq!(dirs.len(), 1, "{dirs:?}");
    let saved = dirs[0].join("files/.claude/settings.json");
    assert_eq!(std::fs::read_to_string(&saved).unwrap(), FOREIGN_SETTINGS);
}

#[test]
fn install_and_uninstall_refuse_a_file_that_is_not_json() {
    let home = tempfile::tempdir().unwrap();
    let broken = "{\n  \"hooks\": {\"PreToolUse\": [\n";
    write_settings(home.path(), broken);

    for args in [
        &["init", "--global"][..],
        &["init", "--global", "--uninstall"][..],
    ] {
        let output = frg(home.path(), args);
        assert!(!output.status.success(), "{args:?} accepted invalid JSON");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("settings.json"), "{args:?}: {stderr}");
        assert_eq!(read(home.path()), broken, "{args:?} wrote over the file");
    }
    assert!(backup_dirs(home.path()).is_empty());
}

#[test]
fn install_refuses_hooks_that_are_not_an_object() {
    let home = tempfile::tempdir().unwrap();
    let odd = "{\"hooks\": [\"not\", \"an\", \"object\"]}\n";
    write_settings(home.path(), odd);

    let output = frg(home.path(), &["init", "--global"]);

    assert!(!output.status.success());
    assert_eq!(read(home.path()), odd);
}

#[test]
fn install_upgrades_forges_old_bash_entry_in_place_and_keeps_the_rest() {
    let home = tempfile::tempdir().unwrap();
    let old = r#"{
  "hooks": {
    "PostToolUse": [
      {
        "matcher": "Bash",
        "hooks": [
          {
            "type": "command",
            "command": "frg hook 2>/dev/null || true"
          }
        ]
      },
      {
        "matcher": "Bash",
        "hooks": [
          {
            "type": "command",
            "command": "~/bin/after-bash.sh"
          }
        ]
      }
    ]
  }
}
"#;
    write_settings(home.path(), old);

    ok(&frg(home.path(), &["init", "--global"]));

    let after: Value = serde_json::from_str(&read(home.path())).unwrap();
    let post = after["hooks"]["PostToolUse"].as_array().unwrap();
    assert_eq!(post.len(), 2, "{post:?}");
    assert_eq!(
        post[0],
        forge_entry(),
        "the old entry is replaced where it was"
    );
    assert_eq!(post[1]["hooks"][0]["command"], "~/bin/after-bash.sh");
    assert_eq!(
        after["hooks"]["PreToolUse"],
        Value::Array(vec![forge_entry()])
    );
}

#[cfg(unix)]
#[test]
fn install_writes_through_a_symlinked_settings_file() {
    let home = tempfile::tempdir().unwrap();
    let dotfiles = home.path().join("dotfiles");
    std::fs::create_dir_all(&dotfiles).unwrap();
    std::fs::create_dir_all(home.path().join(".claude")).unwrap();
    std::fs::write(dotfiles.join("settings.json"), FOREIGN_SETTINGS).unwrap();
    std::os::unix::fs::symlink(dotfiles.join("settings.json"), settings_path(home.path())).unwrap();

    ok(&frg(home.path(), &["init", "--global"]));

    let link = std::fs::symlink_metadata(settings_path(home.path())).unwrap();
    assert!(
        link.file_type().is_symlink(),
        "the symlink was replaced by a file"
    );
    let target = std::fs::read_to_string(dotfiles.join("settings.json")).unwrap();
    assert_eq!(count_forge_entries(&target, "PreToolUse"), 1);
}

#[test]
fn install_into_a_missing_file_creates_only_forges_entries() {
    let home = tempfile::tempdir().unwrap();

    ok(&frg(home.path(), &["init", "--global"]));

    let after: Value = serde_json::from_str(&read(home.path())).unwrap();
    assert_eq!(
        after["hooks"]["PreToolUse"],
        Value::Array(vec![forge_entry()])
    );
    assert_eq!(
        after["hooks"]["PostToolUse"],
        Value::Array(vec![forge_entry()])
    );
}
