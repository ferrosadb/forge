//! Forge's hook entries in a Claude Code `settings.json`, by exact identity.
//!
//! `frg init` writes two entries, a `Read` matcher under `PreToolUse` and
//! under `PostToolUse`, each running `frg hook`. An entry is Forge's only when
//! it is exactly one Forge has written (now or in an earlier release). An entry
//! that merely mentions "forge" or `frg`, or that mixes Forge's command with
//! the user's own, belongs to someone else and is never touched (t_92c2dcc0,
//! harness-plugins B5).

use std::path::Path;

use anyhow::{bail, Context};
use serde_json::{json, Map, Value};

/// The hook command `frg init` installs.
pub const CANONICAL_HOOK_COMMAND: &str = "frg hook 2>/dev/null || true";

/// Hook events Forge writes to.
pub const PHASES: [&str; 2] = ["PreToolUse", "PostToolUse"];

fn entry(matcher: &str) -> Value {
    json!({
        "matcher": matcher,
        "hooks": [{"type": "command", "command": CANONICAL_HOOK_COMMAND}]
    })
}

/// The entries the current release installs, per phase.
pub fn current_entries(phase: &str) -> Vec<Value> {
    match phase {
        "PreToolUse" | "PostToolUse" => vec![entry("Read")],
        _ => Vec::new(),
    }
}

/// Entries an earlier release installed that the current one does not.
/// Until September 2026 (269c97c) `frg init` also wrote a `Bash` matcher
/// under `PostToolUse`.
pub fn retired_entries(phase: &str) -> Vec<Value> {
    match phase {
        "PostToolUse" => vec![entry("Bash")],
        _ => Vec::new(),
    }
}

fn is_current(phase: &str, candidate: &Value) -> bool {
    current_entries(phase).iter().any(|e| e == candidate)
}

fn is_retired(phase: &str, candidate: &Value) -> bool {
    retired_entries(phase).iter().any(|e| e == candidate)
}

/// True when `candidate` is exactly an entry some Forge release wrote.
pub fn is_forge_entry(phase: &str, candidate: &Value) -> bool {
    is_current(phase, candidate) || is_retired(phase, candidate)
}

/// The `hooks` object of a settings value, or an error naming what is wrong
/// with it. `None` when the settings have no `hooks` key.
fn hooks_object(settings: &Value) -> anyhow::Result<Option<&Map<String, Value>>> {
    let Some(root) = settings.as_object() else {
        bail!("the top level is not a JSON object");
    };
    match root.get("hooks") {
        None => Ok(None),
        Some(Value::Object(hooks)) => {
            for phase in PHASES {
                if let Some(v) = hooks.get(phase) {
                    if !v.is_array() {
                        bail!("hooks.{phase} is not an array");
                    }
                }
            }
            Ok(Some(hooks))
        }
        Some(_) => bail!("\"hooks\" is not a JSON object"),
    }
}

/// Check that Forge can edit these settings without guessing.
pub fn validate(settings: &Value) -> anyhow::Result<()> {
    hooks_object(settings).map(|_| ())
}

/// `current` when every current entry is present and no retired one is,
/// `outdated` when some Forge entry is present but not that, `not_installed`
/// when there is no Forge entry at all.
pub fn status(settings: &Value) -> &'static str {
    let Ok(Some(hooks)) = hooks_object(settings) else {
        return "not_installed";
    };
    let entries = |phase: &str| -> Vec<Value> {
        hooks
            .get(phase)
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
    };
    let mut any = false;
    let mut current = true;
    for phase in PHASES {
        let have = entries(phase);
        any |= have.iter().any(|e| is_forge_entry(phase, e));
        current &= current_entries(phase).iter().all(|e| have.contains(e));
        current &= !have.iter().any(|e| is_retired(phase, e));
    }
    match (any, current) {
        (false, _) => "not_installed",
        (true, true) => "current",
        (true, false) => "outdated",
    }
}

/// Merge Forge's entries in. A retired Forge entry is replaced in place by
/// the current one for its phase, current entries already present are left
/// alone, missing ones are appended, and nothing else changes. Returns
/// whether the settings changed.
pub fn install(settings: &mut Value) -> anyhow::Result<bool> {
    validate(settings)?;
    let root = settings
        .as_object_mut()
        .context("settings is not an object")?;
    let hooks = root
        .entry("hooks")
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()
        .context("hooks is not an object")?;
    let mut changed = false;
    for phase in PHASES {
        let wanted = current_entries(phase);
        let list = hooks
            .entry(phase)
            .or_insert_with(|| Value::Array(Vec::new()))
            .as_array_mut()
            .context("phase is not an array")?;
        let mut missing: Vec<Value> = wanted
            .iter()
            .filter(|w| !list.contains(w))
            .cloned()
            .collect();
        // Replace retired entries where they stand, while a missing current
        // entry is left to take the slot; drop the rest.
        let mut i = 0;
        while i < list.len() {
            if is_retired(phase, &list[i]) {
                changed = true;
                if missing.is_empty() {
                    list.remove(i);
                    continue;
                }
                list[i] = missing.remove(0);
            }
            i += 1;
        }
        changed |= !missing.is_empty();
        list.extend(missing);
    }
    Ok(changed)
}

/// Remove every entry that is exactly one Forge wrote. A phase list that
/// becomes empty because of it is removed, and so is a `hooks` object that
/// becomes empty, so install then uninstall leaves the file as it was.
/// Returns how many entries were removed.
pub fn uninstall(settings: &mut Value) -> anyhow::Result<usize> {
    validate(settings)?;
    let Some(root) = settings.as_object_mut() else {
        return Ok(0);
    };
    let Some(hooks) = root.get_mut("hooks").and_then(Value::as_object_mut) else {
        return Ok(0);
    };
    let mut removed = 0;
    for phase in PHASES {
        let Some(list) = hooks.get_mut(phase).and_then(Value::as_array_mut) else {
            continue;
        };
        let before = list.len();
        list.retain(|e| !is_forge_entry(phase, e));
        let dropped = before - list.len();
        removed += dropped;
        if dropped > 0 && list.is_empty() {
            hooks.shift_remove(phase);
        }
    }
    if removed > 0 && hooks.is_empty() {
        root.shift_remove("hooks");
    }
    Ok(removed)
}

/// Put one entry back under `phase` (the undo of a migration), unless an
/// identical entry is already there. Returns whether the settings changed.
pub fn restore_entry(settings: &mut Value, phase: &str, entry: &Value) -> anyhow::Result<bool> {
    validate(settings)?;
    let root = settings
        .as_object_mut()
        .context("settings is not an object")?;
    let list = root
        .entry("hooks")
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()
        .context("hooks is not an object")?
        .entry(phase)
        .or_insert_with(|| Value::Array(Vec::new()))
        .as_array_mut()
        .with_context(|| format!("hooks.{phase} is not an array"))?;
    if list.contains(entry) {
        return Ok(false);
    }
    list.push(entry.clone());
    Ok(true)
}

/// The settings `frg init` writes to a file that does not exist yet.
#[cfg(test)]
pub fn fresh_settings() -> Value {
    let mut settings = json!({});
    install(&mut settings).expect("an empty object always accepts Forge's hooks");
    settings
}

/// Hook schema version reported by `frg init --show`; bump when the hook
/// format changes.
pub const HOOK_SCHEMA_VERSION: &str = "1";

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum InitMode {
    Install,
    Uninstall,
    Show,
}

/// Read and parse a settings file. `Ok(None)` when it does not exist; an
/// error, naming the file, when it cannot be read or is not JSON Forge can
/// edit. Nothing is ever written after an error here.
pub fn load(path: &Path) -> anyhow::Result<Option<(String, Value)>> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).with_context(|| format!("cannot read {}", path.display())),
    };
    let value: Value = serde_json::from_str(&text).with_context(|| {
        format!(
            "{} is not valid JSON; fix it by hand, nothing was written",
            path.display()
        )
    })?;
    validate(&value).with_context(|| {
        format!(
            "{} has a shape Forge will not edit; nothing was written",
            path.display()
        )
    })?;
    Ok(Some((text, value)))
}

/// `frg init [--global] [--uninstall | --show]`.
pub fn run_init(home: &Path, path: &Path, global: bool, mode: InitMode) -> anyhow::Result<()> {
    if mode == InitMode::Show {
        return show(path);
    }
    let loaded = load(path)?;
    let original = loaded.as_ref().map(|(text, _)| text.as_str());
    let mut settings = loaded
        .as_ref()
        .map_or_else(|| json!({}), |(_, value)| value.clone());

    let changed = match mode {
        InitMode::Uninstall => {
            if loaded.is_none() {
                println!("No settings file found at {}", path.display());
                return Ok(());
            }
            let removed = uninstall(&mut settings)?;
            println!(
                "Removed {removed} forge hook entries from {}",
                path.display()
            );
            removed > 0
        }
        _ => {
            let changed = install(&mut settings)?;
            let scope = if global { "global" } else { "project" };
            if changed {
                println!("Installed forge hooks ({scope}) at {}", path.display());
                println!("Restart Claude Code to activate hooks.");
            } else {
                println!(
                    "Forge hooks already current ({scope}) at {}",
                    path.display()
                );
            }
            changed
        }
    };
    if !changed {
        return Ok(());
    }
    if loaded.is_some() {
        let mut backups = crate::harness_files::BackupSet::new(home);
        let saved = backups.save(path)?;
        println!("Backed up the previous file to {}", saved.display());
    }
    let text = crate::harness_files::json_text(&settings, original)?;
    crate::harness_files::write_atomic(path, &text)
}

fn show(path: &Path) -> anyhow::Result<()> {
    let mut result = json!({
        "settings_path": path.display().to_string(),
        "forge_hooks_installed": false,
        "hook_status": "not_installed",
        "hook_schema_version": HOOK_SCHEMA_VERSION,
    });
    match load(path) {
        Ok(None) => result["note"] = json!("Settings file does not exist"),
        Err(e) => result["note"] = json!(format!("{e:#}")),
        Ok(Some((_, settings))) => {
            let hook_status = status(&settings);
            result["hook_status"] = json!(hook_status);
            result["forge_hooks_installed"] = json!(hook_status != "not_installed");
            if hook_status == "outdated" {
                result["message"] =
                    json!("Installed hook uses old format. Run `frg init --global` to upgrade.");
            }
        }
    }
    println!("{}", serde_json::to_string_pretty(&result)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn forge() -> Value {
        entry("Read")
    }

    #[test]
    fn fresh_settings_report_current() {
        assert_eq!(status(&fresh_settings()), "current");
    }

    #[test]
    fn an_mcp_server_entry_alone_is_not_an_installed_hook() {
        let settings = json!({"mcpServers": {"forge": {"command": "frg", "args": ["--mcp"]}}});
        assert_eq!(status(&settings), "not_installed");
    }

    #[test]
    fn a_retired_entry_reports_outdated() {
        let settings = json!({"hooks": {"PostToolUse": [entry("Bash")]}});
        assert_eq!(status(&settings), "outdated");
    }

    #[test]
    fn a_hand_written_frg_hook_is_not_forges() {
        let settings = json!({"hooks": {"PostToolUse": [
            {"matcher": "Read", "hooks": [{"type": "command", "command": "/opt/bin/frg hook"}]}
        ]}});
        assert_eq!(status(&settings), "not_installed");
    }

    #[test]
    fn key_order_does_not_change_identity() {
        let reordered: Value = serde_json::from_str(
            r#"{"hooks":[{"command":"frg hook 2>/dev/null || true","type":"command"}],"matcher":"Read"}"#,
        )
        .unwrap();
        assert!(is_forge_entry("PreToolUse", &reordered));
    }

    #[test]
    fn uninstall_leaves_a_mixed_entry_whole() {
        let mixed = json!({"matcher": "Read", "hooks": [
            {"type": "command", "command": CANONICAL_HOOK_COMMAND},
            {"type": "command", "command": "~/bin/audit-read.sh"}
        ]});
        let mut settings = json!({"hooks": {"PostToolUse": [mixed.clone(), forge()]}});
        assert_eq!(uninstall(&mut settings).unwrap(), 1);
        assert_eq!(settings["hooks"]["PostToolUse"], json!([mixed]));
    }

    #[test]
    fn install_keeps_entries_of_other_phases_and_tools() {
        let theirs = json!({"matcher": "Bash", "hooks": [
            {"type": "command", "command": "python3 ~/.claude/hooks/require-forge.py"}
        ]});
        let mut settings = json!({"hooks": {
            "PreToolUse": [theirs.clone()],
            "Stop": [theirs.clone()]
        }});
        assert!(install(&mut settings).unwrap());
        assert_eq!(
            settings["hooks"]["PreToolUse"],
            json!([theirs.clone(), forge()])
        );
        assert_eq!(settings["hooks"]["Stop"], json!([theirs]));
        assert!(
            !install(&mut settings).unwrap(),
            "a second install changes nothing"
        );
    }

    #[test]
    fn both_writers_refuse_settings_they_cannot_read() {
        for bad in [
            json!([]),
            json!({"hooks": 3}),
            json!({"hooks": {"PreToolUse": {}}}),
        ] {
            assert!(install(&mut bad.clone()).is_err(), "{bad}");
            assert!(uninstall(&mut bad.clone()).is_err(), "{bad}");
        }
    }
}
