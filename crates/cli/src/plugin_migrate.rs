//! Migrating legacy forge entries away once the plugin carries them (B4).
//!
//! An entry is removed only when it is structurally equal to a row of the
//! signature table below, the plugin's MCP server answered in this run, and
//! the whole file was backed up first. Anything that merely looks like ours is
//! left alone and reported with the manual change. The report has the shape
//! `fmem harness migrate --json` uses (ferrosa-memory docs/harness-plugins.md,
//! "JSON contract"), with `tool: "frg"`, and lands in the backup directory as
//! `report.json`.
//!
//! | signature | file, place | shape | writer |
//! |---|---|---|---|
//! | `frg-init-hook-PreToolUse-Read` | `~/.claude/settings.json` `hooks.PreToolUse[i]` | `{"matcher":"Read","hooks":[{"type":"command","command":"frg hook 2>/dev/null \|\| true"}]}` | `frg init --global` |
//! | `frg-init-hook-PostToolUse-Read` | same, `hooks.PostToolUse[i]` | same | `frg init --global` |
//! | `frg-init-hook-PostToolUse-Bash` | same | `"matcher":"Bash"` | `frg init --global` before 269c97c |
//! | `claude-json-mcp-coordinator-forge` | `~/.claude.json` `mcpServers.forge` | `{"type":"stdio","command":"{HOME}/.ferrosa/bin/frg","args":["--mcp"]}` | ferrosa-coordinator harness_plan |
//! | `codex-toml-mcp-coordinator-forge` | `~/.codex/config.toml` `[mcp_servers.forge]` | `command = "{HOME}/.ferrosa/bin/frg"`, `args = ["--mcp"]` | ferrosa-coordinator harness_plan |

use std::path::{Path, PathBuf};

use anyhow::{bail, Context};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::claude_hooks;
use crate::harness_files::{self, BackupSet};
use crate::plugin_cmd::Harness;

const REPORT: &str = "report.json";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    SettingsHook,
    ClaudeJsonMcp,
    CodexTomlMcp,
}

/// One legacy entry found by a scan.
#[derive(Clone, Debug)]
pub struct Finding {
    harness: Harness,
    kind: Kind,
    file: PathBuf,
    location: String,
    signature: Option<&'static str>,
    entry: Value,
    /// `removable`, `edited` or `unparseable`.
    status: &'static str,
    reason: String,
    manual: String,
}

pub fn codex_home(home: &Path) -> PathBuf {
    std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".codex"))
}

fn frg_path(home: &Path) -> String {
    home.join(".ferrosa/bin/frg").display().to_string()
}

fn coordinator_claude_entry(home: &Path) -> Value {
    json!({"type": "stdio", "command": frg_path(home), "args": ["--mcp"]})
}

fn coordinator_codex_entry(home: &Path) -> toml::Value {
    let mut t = toml::Table::new();
    t.insert("command".into(), toml::Value::String(frg_path(home)));
    t.insert(
        "args".into(),
        toml::Value::Array(vec![toml::Value::String("--mcp".into())]),
    );
    toml::Value::Table(t)
}

fn hook_signature(phase: &str, entry: &Value) -> Option<&'static str> {
    if !claude_hooks::is_forge_entry(phase, entry) {
        return None;
    }
    Some(match (phase, entry["matcher"].as_str()) {
        ("PreToolUse", _) => "frg-init-hook-PreToolUse-Read",
        (_, Some("Bash")) => "frg-init-hook-PostToolUse-Bash",
        _ => "frg-init-hook-PostToolUse-Read",
    })
}

/// True when a hook entry runs `frg hook` in some form, so a non-exact one is
/// worth reporting.
fn mentions_frg_hook(entry: &Value) -> bool {
    entry["hooks"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|h| h["command"].as_str())
        .any(|c| c.trim_start().starts_with("frg hook") || c.contains("/frg hook"))
}

pub fn tilde(home: &Path, path: &Path) -> String {
    match path.strip_prefix(home) {
        Ok(rel) => format!("~/{}", rel.display()),
        Err(_) => path.display().to_string(),
    }
}

fn scan_settings(home: &Path, harness: Harness, out: &mut Vec<Finding>) {
    let file = home.join(".claude/settings.json");
    let base = Finding {
        harness,
        kind: Kind::SettingsHook,
        file: file.clone(),
        location: "hooks".into(),
        signature: None,
        entry: Value::Null,
        status: "removable",
        reason: String::new(),
        manual: String::new(),
    };
    let settings = match claude_hooks::load(&file) {
        Ok(None) => return,
        Ok(Some((_, v))) => v,
        Err(e) => {
            out.push(Finding {
                status: "unparseable",
                reason: format!("{e:#}"),
                manual: "fix the file, then run this again".into(),
                ..base
            });
            return;
        }
    };
    for phase in claude_hooks::PHASES {
        for (i, entry) in settings["hooks"][phase]
            .as_array()
            .into_iter()
            .flatten()
            .enumerate()
        {
            let location = format!("hooks.{phase}[{i}]");
            if let Some(signature) = hook_signature(phase, entry) {
                out.push(Finding {
                    location,
                    signature: Some(signature),
                    entry: entry.clone(),
                    ..base.clone()
                });
            } else if mentions_frg_hook(entry) {
                out.push(Finding {
                    location: location.clone(),
                    entry: entry.clone(),
                    status: "edited",
                    reason: "runs frg hook but is not an entry frg init wrote".into(),
                    manual: format!(
                        "~/.claude/settings.json: delete the frg hook from {location} if the forge plugin's hook replaces it"
                    ),
                    ..base.clone()
                });
            }
        }
    }
}

fn scan_claude_mcp(home: &Path, harness: Harness, out: &mut Vec<Finding>) {
    for (file, signed) in [
        (home.join(".claude.json"), true),
        (home.join(".mcp.json"), false),
    ] {
        let Ok(text) = std::fs::read_to_string(&file) else {
            continue;
        };
        let shown = tilde(home, &file);
        let mut f = Finding {
            harness,
            kind: Kind::ClaudeJsonMcp,
            file: file.clone(),
            location: "mcpServers.forge".into(),
            signature: None,
            entry: Value::Null,
            status: "edited",
            reason: String::new(),
            manual: if signed {
                "claude mcp remove forge --scope user".into()
            } else {
                format!("{shown}: delete mcpServers.forge")
            },
        };
        match serde_json::from_str::<Value>(&text) {
            Err(e) => {
                f.status = "unparseable";
                f.location = "mcpServers".into();
                f.reason = format!("{shown} is not valid JSON: {e}");
                out.push(f);
            }
            Ok(v) => {
                let Some(entry) = v["mcpServers"].get("forge") else {
                    continue;
                };
                f.entry = entry.clone();
                if signed && *entry == coordinator_claude_entry(home) {
                    f.status = "removable";
                    f.signature = Some("claude-json-mcp-coordinator-forge");
                } else {
                    f.reason =
                        "a forge MCP server that is not exactly one a Ferrosa installer wrote; \
                                the plugin now provides the server"
                            .into();
                }
                out.push(f);
            }
        }
    }
}

fn scan_codex(home: &Path, harness: Harness, out: &mut Vec<Finding>) {
    let file = codex_home(home).join("config.toml");
    let Ok(text) = std::fs::read_to_string(&file) else {
        return;
    };
    let mut f = Finding {
        harness,
        kind: Kind::CodexTomlMcp,
        file,
        location: "mcp_servers.forge".into(),
        signature: None,
        entry: Value::Null,
        status: "edited",
        reason: String::new(),
        manual: "~/.codex/config.toml: delete [mcp_servers.forge] (while it is there it hides the plugin's server)".into(),
    };
    match text.parse::<toml::Table>() {
        Err(e) => {
            f.status = "unparseable";
            f.location = "mcp_servers".into();
            f.reason = format!("config.toml is not valid TOML: {e}");
            out.push(f);
        }
        Ok(t) => {
            let Some(entry) = t.get("mcp_servers").and_then(|m| m.get("forge")) else {
                return;
            };
            f.entry = serde_json::to_value(entry).unwrap_or(Value::Null);
            if *entry == coordinator_codex_entry(home) {
                f.status = "removable";
                f.signature = Some("codex-toml-mcp-coordinator-forge");
            } else {
                f.reason =
                    "a forge MCP server that is not exactly one a Ferrosa installer wrote".into();
            }
            out.push(f);
        }
    }
}

/// Every legacy forge entry for `harness`.
pub fn scan(home: &Path, harness: Harness) -> Vec<Finding> {
    let mut out = Vec::new();
    match harness {
        Harness::ClaudeCode => {
            scan_settings(home, harness, &mut out);
            scan_claude_mcp(home, harness, &mut out);
        }
        Harness::Codex => scan_codex(home, harness, &mut out),
    }
    out
}

fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

// ── Byte-range removal ──────────────────────────────────────────────────────

fn skip_ws(b: &[u8], mut i: usize) -> usize {
    while i < b.len() && b[i].is_ascii_whitespace() {
        i += 1;
    }
    i
}

fn string_end(b: &[u8], start: usize) -> Option<usize> {
    let mut i = start + 1;
    while i < b.len() {
        match b[i] {
            b'\\' => i += 2,
            b'"' => return Some(i + 1),
            _ => i += 1,
        }
    }
    None
}

fn value_end(b: &[u8], start: usize) -> Option<usize> {
    match *b.get(start)? {
        b'"' => string_end(b, start),
        b'{' | b'[' => {
            let mut depth = 0usize;
            let mut i = start;
            while i < b.len() {
                match b[i] {
                    b'"' => {
                        i = string_end(b, i)?;
                        continue;
                    }
                    b'{' | b'[' => depth += 1,
                    b'}' | b']' => {
                        depth -= 1;
                        if depth == 0 {
                            return Some(i + 1);
                        }
                    }
                    _ => {}
                }
                i += 1;
            }
            None
        }
        _ => {
            let mut i = start;
            while i < b.len() && !matches!(b[i], b',' | b'}' | b']') && !b[i].is_ascii_whitespace()
            {
                i += 1;
            }
            Some(i)
        }
    }
}

/// Members of the object starting at `open`: (raw key, key start, value
/// start, value end).
fn members(b: &[u8], open: usize) -> Option<Vec<(String, usize, usize, usize)>> {
    if b.get(open) != Some(&b'{') {
        return None;
    }
    let mut out = Vec::new();
    let mut i = skip_ws(b, open + 1);
    if b.get(i) == Some(&b'}') {
        return Some(out);
    }
    loop {
        if b.get(i) != Some(&b'"') {
            return None;
        }
        let key_end = string_end(b, i)?;
        let key = String::from_utf8_lossy(&b[i + 1..key_end - 1]).into_owned();
        let colon = skip_ws(b, key_end);
        if b.get(colon) != Some(&b':') {
            return None;
        }
        let vstart = skip_ws(b, colon + 1);
        let vend = value_end(b, vstart)?;
        out.push((key, i, vstart, vend));
        let next = skip_ws(b, vend);
        match b.get(next)? {
            b',' => i = skip_ws(b, next + 1),
            b'}' => return Some(out),
            _ => return None,
        }
    }
}

/// Cut `outer.inner` out of a JSON document's text, leaving every other byte
/// alone. `None` when it cannot be located with certainty.
pub fn cut_json_member(text: &str, outer: &str, inner: &str) -> Option<String> {
    let b = text.as_bytes();
    let top = members(b, skip_ws(b, 0))?;
    let (_, _, ostart, _) = top.into_iter().find(|m| m.0 == outer)?;
    let list = members(b, ostart)?;
    let pos = list.iter().position(|m| m.0 == inner)?;
    let (_, kstart, _, vend) = list[pos];
    let (from, to) = if pos + 1 < list.len() {
        (kstart, list[pos + 1].1)
    } else if pos > 0 {
        (list[pos - 1].3, vend)
    } else {
        (ostart + 1, vend)
    };
    let mut out = String::with_capacity(text.len());
    out.push_str(&text[..from]);
    out.push_str(&text[to..]);
    Some(out)
}

fn cut_claude_json(text: &str) -> anyhow::Result<String> {
    let new = cut_json_member(text, "mcpServers", "forge")
        .context("could not locate mcpServers.forge in the text")?;
    let mut expected: Value = serde_json::from_str(text)?;
    if let Some(servers) = expected["mcpServers"].as_object_mut() {
        servers.shift_remove("forge");
    }
    let got: Value = serde_json::from_str(&new).context("the cut did not leave valid JSON")?;
    if got != expected {
        bail!("the cut changed more than mcpServers.forge");
    }
    Ok(new)
}

fn comment_lines(text: &str) -> Vec<&str> {
    let mut v: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|l| l.starts_with('#'))
        .collect();
    v.sort_unstable();
    v
}

fn cut_codex_toml(text: &str) -> anyhow::Result<String> {
    let mut doc: toml_edit::DocumentMut = text.parse()?;
    let servers = doc
        .get_mut("mcp_servers")
        .and_then(|i| i.as_table_like_mut())
        .context("no mcp_servers table")?;
    servers.remove("forge").context("no mcp_servers.forge")?;
    let new = doc.to_string();
    if comment_lines(&new) != comment_lines(text) {
        bail!("removing [mcp_servers.forge] would also remove a comment in or above it");
    }
    let mut expected: toml::Table = text.parse()?;
    if let Some(toml::Value::Table(m)) = expected.get_mut("mcp_servers") {
        m.remove("forge");
        if m.is_empty() {
            expected.remove("mcp_servers");
        }
    }
    let mut got: toml::Table = new.parse().context("the edit did not leave valid TOML")?;
    if matches!(got.get("mcp_servers"), Some(toml::Value::Table(m)) if m.is_empty()) {
        got.remove("mcp_servers");
    }
    if got != expected {
        bail!("the edit changed more than [mcp_servers.forge]");
    }
    Ok(new)
}

/// The file's new text with every removable finding in it removed.
fn rewrite(kind: Kind, text: &str) -> anyhow::Result<String> {
    match kind {
        Kind::SettingsHook => {
            let mut v: Value = serde_json::from_str(text)?;
            claude_hooks::uninstall(&mut v)?;
            harness_files::json_text(&v, Some(text))
        }
        Kind::ClaudeJsonMcp => cut_claude_json(text),
        Kind::CodexTomlMcp => cut_codex_toml(text),
    }
}

// ── Migrate ─────────────────────────────────────────────────────────────────

fn left_json(home: &Path, f: &Finding) -> Value {
    json!({
        "harness": f.harness.name(),
        "file": tilde(home, &f.file),
        "location": f.location,
        "status": f.status,
        "reason": f.reason,
        "manual": f.manual,
    })
}

fn removed_json(home: &Path, f: &Finding) -> Value {
    json!({
        "harness": f.harness.name(),
        "file": tilde(home, &f.file),
        "location": f.location,
        "signature": f.signature,
        "entry": f.entry,
    })
}

/// Remove the removable findings. `verified` is the plugin verification for
/// this run: without it nothing is written and the findings are left as
/// `removable` with an error.
pub fn migrate(
    home: &Path,
    harness: Harness,
    verified: Result<(), String>,
    dry_run: bool,
) -> anyhow::Result<Value> {
    let findings = scan(home, harness);
    let mut removed = Vec::new();
    let mut left = Vec::new();
    let mut errors = Vec::new();
    let mut files = Vec::new();
    let mut backups = BackupSet::new(home);

    if let Err(why) = &verified {
        errors.push(json!({"harness": harness.name(), "message": format!("the {} plugin is not verified ({why}); nothing was removed", harness.name())}));
    }
    let (removable, others): (Vec<Finding>, Vec<Finding>) =
        findings.into_iter().partition(|f| f.status == "removable");
    left.extend(others.iter().map(|f| left_json(home, f)));

    let mut by_file: Vec<(PathBuf, Vec<Finding>)> = Vec::new();
    for f in removable {
        match by_file.iter_mut().find(|(p, _)| *p == f.file) {
            Some((_, v)) => v.push(f),
            None => by_file.push((f.file.clone(), vec![f])),
        }
    }
    for (file, group) in by_file {
        if verified.is_err() {
            left.extend(group.iter().map(|f| left_json(home, f)));
            continue;
        }
        let text = std::fs::read_to_string(&file)
            .with_context(|| format!("cannot read {}", file.display()))?;
        let new = match rewrite(group[0].kind, &text) {
            Ok(new) => new,
            Err(e) => {
                for f in &group {
                    let mut kept = f.clone();
                    kept.status = "edited";
                    kept.reason = format!("{e:#}");
                    kept.manual =
                        format!("{}: delete {} by hand", tilde(home, &f.file), f.location);
                    left.push(left_json(home, &kept));
                }
                continue;
            }
        };
        removed.extend(group.iter().map(|f| removed_json(home, f)));
        if dry_run {
            continue;
        }
        if std::fs::read_to_string(&file).ok().as_deref() != Some(text.as_str()) {
            errors.push(json!({"harness": harness.name(), "message": format!("{} changed while migrating; it was not written", tilde(home, &file))}));
            removed.retain(|r| r["file"] != tilde(home, &file));
            continue;
        }
        let saved = backups.save(&file)?;
        harness_files::write_atomic(&file, &new)?;
        let dir = backups.ensure_dir()?;
        files.push(json!({
            "path": file.display().to_string(),
            "backup": saved.strip_prefix(&dir).unwrap_or(&saved).display().to_string(),
            "sha256_before": sha256(text.as_bytes()),
            "sha256_after": sha256(new.as_bytes()),
            "written": true,
        }));
    }

    let mut report = json!({
        "tool": "frg",
        "tool_version": env!("CARGO_PKG_VERSION"),
        "action": "migrate",
        "harness": harness.name(),
        "harnesses": [harness.name()],
        "created_at": jiff::Timestamp::now().to_string(),
        "dry_run": dry_run,
        "removed": removed,
        "left": left,
        "errors": errors,
        "backup_dir": Value::Null,
        "undo": Value::Null,
        "files": files,
        "undone_at": Value::Null,
    });
    if !files.is_empty() {
        let dir = backups.ensure_dir()?;
        let stamp = dir
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        report["backup_dir"] = json!(dir.display().to_string());
        report["undo"] = json!(format!("frg plugin migrate --undo {stamp}"));
        write_report(&dir, &report)?;
    }
    Ok(report)
}

fn write_report(dir: &Path, report: &Value) -> anyhow::Result<()> {
    harness_files::write_atomic(
        &dir.join(REPORT),
        &(serde_json::to_string_pretty(report)? + "\n"),
    )
}

// ── Undo ────────────────────────────────────────────────────────────────────

enum Step {
    Restore {
        path: PathBuf,
        backup: PathBuf,
    },
    ReAdd {
        path: PathBuf,
        entries: Vec<(String, Value)>,
    },
    Unchanged(PathBuf),
}

/// `frg plugin migrate --undo <ts>`. A file still exactly as the migration
/// left it is restored whole from the backup. A `settings.json` edited since
/// gets the removed entries re-added instead; any other file edited since
/// stops the undo before anything is written.
pub fn undo(home: &Path, stamp: &str) -> anyhow::Result<Value> {
    if stamp.is_empty() || stamp.contains('/') || stamp.contains("..") {
        bail!("{stamp} is not a backup timestamp");
    }
    let dir = harness_files::backups_root(home).join(stamp);
    let text = std::fs::read_to_string(dir.join(REPORT))
        .with_context(|| format!("no migration report in {}", dir.display()))?;
    let mut report: Value = serde_json::from_str(&text)?;
    if report["tool"] != "frg" {
        bail!(
            "{} was not written by frg; undo it with the tool that wrote it",
            dir.display()
        );
    }
    let mut out = json!({
        "tool": "frg", "tool_version": env!("CARGO_PKG_VERSION"), "action": "undo",
        "undone": stamp, "restored": [], "unchanged": [], "errors": [],
    });
    if !report["undone_at"].is_null() {
        out["errors"] = json!([format!(
            "{stamp} was already undone at {}",
            report["undone_at"]
        )]);
        return Ok(out);
    }
    let mut steps = Vec::new();
    let mut errors = Vec::new();
    for f in report["files"].as_array().cloned().unwrap_or_default() {
        let path = PathBuf::from(f["path"].as_str().context("files[] without a path")?);
        let backup = dir.join(f["backup"].as_str().context("files[] without a backup")?);
        let current = std::fs::read(&path).map(|b| sha256(&b)).unwrap_or_default();
        if current == f["sha256_after"] {
            steps.push(Step::Restore { path, backup });
        } else if current == f["sha256_before"] {
            steps.push(Step::Unchanged(path));
        } else if path.ends_with(".claude/settings.json") {
            let shown = tilde(home, &path);
            let entries = report["removed"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|r| r["file"] == shown)
                .filter_map(|r| {
                    let loc = r["location"].as_str()?.strip_prefix("hooks.")?;
                    let phase = loc.split('[').next()?.to_string();
                    Some((phase, r["entry"].clone()))
                })
                .collect();
            steps.push(Step::ReAdd { path, entries });
        } else {
            errors.push(format!(
                "{} changed since the migration; compare it with {} by hand",
                tilde(home, &path),
                backup.display()
            ));
        }
    }
    if !errors.is_empty() {
        out["errors"] = json!(errors);
        return Ok(out);
    }
    let mut restored = Vec::new();
    let mut unchanged = Vec::new();
    for step in steps {
        match step {
            Step::Restore { path, backup } => {
                let body = std::fs::read_to_string(&backup)
                    .with_context(|| format!("cannot read backup {}", backup.display()))?;
                harness_files::write_atomic(&path, &body)?;
                restored.push(tilde(home, &path));
            }
            Step::ReAdd { path, entries } => {
                let (text, mut settings) = claude_hooks::load(&path)?
                    .map(|(t, v)| (Some(t), v))
                    .unwrap_or((None, json!({})));
                for (phase, entry) in &entries {
                    claude_hooks::restore_entry(&mut settings, phase, entry)?;
                }
                let new = harness_files::json_text(&settings, text.as_deref())?;
                harness_files::write_atomic(&path, &new)?;
                restored.push(tilde(home, &path));
            }
            Step::Unchanged(path) => unchanged.push(tilde(home, &path)),
        }
    }
    report["undone_at"] = json!(jiff::Timestamp::now().to_string());
    write_report(&dir, &report)?;
    out["restored"] = json!(restored);
    out["unchanged"] = json!(unchanged);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cut_keeps_every_other_byte() {
        let text = "{\n  \"a\": 1,\n  \"mcpServers\": {\n    \"x\": {\"command\": \"x\"},\n    \"forge\": {\"command\": \"f\", \"args\": [\"--mcp\"]},\n    \"y\": 2\n  },\n  \"z\": \"}\"\n}\n";
        let want = "{\n  \"a\": 1,\n  \"mcpServers\": {\n    \"x\": {\"command\": \"x\"},\n    \"y\": 2\n  },\n  \"z\": \"}\"\n}\n";
        assert_eq!(cut_json_member(text, "mcpServers", "forge").unwrap(), want);
        assert!(cut_claude_json(text).is_ok());
    }

    #[test]
    fn cut_handles_the_last_and_the_only_member() {
        let last = "{\"mcpServers\": {\"x\": 1, \"forge\": {}}}";
        assert_eq!(
            cut_json_member(last, "mcpServers", "forge").unwrap(),
            "{\"mcpServers\": {\"x\": 1}}"
        );
        let only = "{\"mcpServers\": {\"forge\": {}}}";
        assert_eq!(
            cut_json_member(only, "mcpServers", "forge").unwrap(),
            "{\"mcpServers\": {}}"
        );
        assert!(cut_json_member(only, "mcpServers", "nope").is_none());
    }

    #[test]
    fn toml_cut_refuses_to_lose_a_comment() {
        let ok = "# top\n[mcp_servers.github]\ncommand = \"gh\"\n\n[mcp_servers.forge]\ncommand = \"f\"\nargs = [\"--mcp\"]\n\n# after\n[profiles.x]\nmodel = \"o4\"\n";
        let new = cut_codex_toml(ok).unwrap();
        assert!(new.contains("# top") && new.contains("# after") && !new.contains("forge"));
        let commented = "[mcp_servers.forge]\n# mine\ncommand = \"f\"\nargs = [\"--mcp\"]\n";
        assert!(cut_codex_toml(commented).is_err());
        let above = "model = \"o3\"\n\n# forge server\n[mcp_servers.forge]\ncommand = \"f\"\nargs = [\"--mcp\"]\n";
        assert!(cut_codex_toml(above).is_err());
    }
}
