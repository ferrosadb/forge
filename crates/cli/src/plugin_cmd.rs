//! `frg plugin install` and `frg plugin migrate --undo` (harness-plugins B1–B5).
//!
//! The forge plugin is installed through the harness's own CLI (`claude
//! plugin ...`, `codex plugin ...`) from a local marketplace directory. Forge
//! never writes the harness's plugin state itself. After the plugin is
//! installed and its MCP server has answered, the hook entries `frg init
//! --global` wrote are migrated away under B4: only exact matches, only after
//! that verification, with a backup first, and undoable.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use anyhow::{bail, Context};
use serde_json::{json, Value};

use crate::claude_hooks;
use crate::harness_files::{self, BackupSet};

/// Name of the plugin and of the local marketplace that carries it.
pub const PLUGIN: &str = "forge";
pub const MARKETPLACE: &str = "forge";
const PLUGIN_ID: &str = "forge@forge";
const MCP_TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum Harness {
    #[value(name = "claude-code")]
    ClaudeCode,
    Codex,
}

impl Harness {
    fn name(self) -> &'static str {
        match self {
            Harness::ClaudeCode => "claude-code",
            Harness::Codex => "codex",
        }
    }

    fn cli(self) -> &'static str {
        match self {
            Harness::ClaudeCode => "claude",
            Harness::Codex => "codex",
        }
    }

    /// The marketplace manifest this harness reads, relative to the root.
    fn marketplace_manifest(self) -> &'static str {
        match self {
            Harness::ClaudeCode => ".claude-plugin/marketplace.json",
            Harness::Codex => ".agents/plugins/marketplace.json",
        }
    }

    /// The plugin manifest this harness reads, relative to the plugin dir.
    fn plugin_manifest(self) -> &'static str {
        match self {
            Harness::ClaudeCode => ".claude-plugin/plugin.json",
            Harness::Codex => "plugin.json",
        }
    }
}

/// Where the installer puts the marketplace for this frg (B1).
pub fn default_plugin_root(home: &Path) -> PathBuf {
    home.join(".ferrosa")
        .join("plugins")
        .join(env!("CARGO_PKG_VERSION"))
        .join(PLUGIN)
}

fn read_json(path: &Path) -> anyhow::Result<Value> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("cannot read {}", path.display()))?;
    serde_json::from_str(&text).with_context(|| format!("{} is not valid JSON", path.display()))
}

/// Check the marketplace directory and return the plugin version it carries.
fn plugin_version(root: &Path, harness: Harness) -> anyhow::Result<String> {
    let market = read_json(&root.join(harness.marketplace_manifest()))?;
    if market["name"] != MARKETPLACE {
        bail!(
            "{} names marketplace {}, not {MARKETPLACE}",
            root.join(harness.marketplace_manifest()).display(),
            market["name"]
        );
    }
    let manifest = root.join(PLUGIN).join(harness.plugin_manifest());
    let plugin = read_json(&manifest)?;
    if plugin["name"] != PLUGIN {
        bail!("{} is not the forge plugin", manifest.display());
    }
    plugin["version"]
        .as_str()
        .map(str::to_string)
        .with_context(|| format!("{} has no version", manifest.display()))
}

/// Run the harness CLI and return stdout, failing loudly with its stderr.
fn run(harness: Harness, args: &[&str]) -> anyhow::Result<String> {
    let output = Command::new(harness.cli())
        .args(args)
        .stdin(Stdio::null())
        .output()
        .with_context(|| {
            format!(
                "cannot run `{}`: is it installed and on PATH?",
                harness.cli()
            )
        })?;
    if !output.status.success() {
        bail!(
            "`{} {}` failed ({}): {}",
            harness.cli(),
            args.join(" "),
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn run_json(harness: Harness, args: &[&str]) -> anyhow::Result<Value> {
    let out = run(harness, args)?;
    serde_json::from_str(&out).with_context(|| {
        format!(
            "`{} {}` did not print JSON: {}",
            harness.cli(),
            args.join(" "),
            out.trim()
        )
    })
}

fn same_dir(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

/// The directory a registered `forge` marketplace points at, if there is one.
fn registered_marketplace(harness: Harness) -> anyhow::Result<Option<PathBuf>> {
    let listed = run_json(harness, &["plugin", "marketplace", "list", "--json"])?;
    let entries = match harness {
        Harness::ClaudeCode => listed.as_array().cloned(),
        Harness::Codex => listed["marketplaces"].as_array().cloned(),
    }
    .with_context(|| {
        format!(
            "unexpected `{} plugin marketplace list` output",
            harness.cli()
        )
    })?;
    let Some(found) = entries.iter().find(|m| m["name"] == MARKETPLACE) else {
        return Ok(None);
    };
    let path = match harness {
        Harness::ClaudeCode => found["path"].as_str(),
        Harness::Codex => found["root"].as_str(),
    };
    Ok(Some(PathBuf::from(path.unwrap_or_default())))
}

fn ensure_marketplace(harness: Harness, root: &Path) -> anyhow::Result<&'static str> {
    match registered_marketplace(harness)? {
        Some(path) if same_dir(&path, root) => Ok("already_registered"),
        Some(path) => bail!(
            "{} already has a marketplace named {MARKETPLACE} at {}. Forge will not replace it. \
             If it is an older forge install, remove it with `{} plugin marketplace remove {MARKETPLACE}` \
             and run this again.",
            harness.name(),
            path.display(),
            harness.cli()
        ),
        None => {
            let root = root.to_str().context("plugin root is not valid UTF-8")?;
            run(harness, &["plugin", "marketplace", "add", root])?;
            Ok("added")
        }
    }
}

/// The installed forge plugin as the harness lists it.
fn installed_plugin(harness: Harness) -> anyhow::Result<Option<Value>> {
    let listed = run_json(harness, &["plugin", "list", "--json"])?;
    let (entries, id_key) = match harness {
        Harness::ClaudeCode => (listed.as_array().cloned(), "id"),
        Harness::Codex => (listed["installed"].as_array().cloned(), "pluginId"),
    };
    let entries = entries
        .with_context(|| format!("unexpected `{} plugin list --json` output", harness.cli()))?;
    Ok(entries.into_iter().find(|p| p[id_key] == PLUGIN_ID))
}

fn ensure_plugin(harness: Harness) -> anyhow::Result<&'static str> {
    if installed_plugin(harness)?.is_some() {
        return Ok("already_installed");
    }
    match harness {
        Harness::ClaudeCode => run(
            harness,
            &["plugin", "install", PLUGIN_ID, "--scope", "user"],
        )?,
        Harness::Codex => run(harness, &["plugin", "add", PLUGIN_ID, "--json"])?,
    };
    Ok("installed")
}

/// The installed plugin's directory, as the harness copied it.
fn install_dir(harness: Harness, home: &Path, plugin: &Value, version: &str) -> PathBuf {
    match harness {
        Harness::ClaudeCode => PathBuf::from(plugin["installPath"].as_str().unwrap_or_default()),
        Harness::Codex => {
            // Codex's documented cache layout: plugins/cache/<market>/<plugin>/<version>.
            let codex_home = std::env::var_os("CODEX_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join(".codex"));
            codex_home
                .join("plugins/cache")
                .join(MARKETPLACE)
                .join(PLUGIN)
                .join(version)
        }
    }
}

/// The command the harness will run for the plugin's `forge` MCP server.
fn mcp_command(harness: Harness, dir: &Path) -> anyhow::Result<PathBuf> {
    let (file, root_var) = match harness {
        Harness::ClaudeCode => (".mcp.json", "${CLAUDE_PLUGIN_ROOT}"),
        Harness::Codex => ("mcp.json", "."),
    };
    let config = read_json(&dir.join(file))?;
    let command = config["mcpServers"][PLUGIN]["command"]
        .as_str()
        .with_context(|| format!("{} has no forge server", dir.join(file).display()))?;
    let relative = command
        .strip_prefix(root_var)
        .with_context(|| format!("unexpected forge server command {command}"))?;
    Ok(dir.join(relative.trim_start_matches('/')))
}

/// Start the server, send `initialize`, and require a forge `serverInfo`.
fn probe_mcp(command: &Path, cwd: &Path) -> anyhow::Result<String> {
    let mut child = Command::new(command)
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("cannot start {}", command.display()))?;
    let init = json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
        "protocolVersion": "2024-11-05", "capabilities": {},
        "clientInfo": {"name": "frg-plugin-verify", "version": env!("CARGO_PKG_VERSION")}
    }});
    let mut stdin = child.stdin.take().context("no stdin")?;
    writeln!(stdin, "{init}")?;
    let stdout = child.stdout.take().context("no stdout")?;
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().take(1000) {
            let Ok(line) = line else { break };
            let Ok(msg) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            if msg["id"] == 1 {
                // A send error means the probe already timed out and stopped
                // listening; the timeout is what gets reported.
                let _ = tx.send(msg);
                break;
            }
        }
    });
    let reply = rx.recv_timeout(MCP_TIMEOUT);
    drop(stdin);
    // The server is only being probed. kill fails only when it has already
    // exited, and its stderr is reported below either way.
    let _ = child.kill();
    let output = child.wait_with_output()?;
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let reply = reply.map_err(|_| {
        anyhow::anyhow!(
            "the forge MCP server ({}) did not answer initialize: {stderr}",
            command.display()
        )
    })?;
    match reply["result"]["serverInfo"]["name"].as_str() {
        Some(PLUGIN) => Ok(reply["result"]["serverInfo"]["version"]
            .as_str()
            .unwrap_or("")
            .to_string()),
        _ => bail!("the forge MCP server answered initialize with {reply}"),
    }
}

/// Confirm the plugin is installed, enabled, the expected version, and that
/// its MCP server answers.
fn verify(harness: Harness, home: &Path, version: &str) -> anyhow::Result<Value> {
    let plugin = installed_plugin(harness)?.with_context(|| {
        format!(
            "{PLUGIN_ID} is not installed in {} after install",
            harness.name()
        )
    })?;
    if plugin["enabled"] != true {
        bail!(
            "{PLUGIN_ID} is installed in {} but disabled; enable it with `{} plugin enable {PLUGIN_ID}`",
            harness.name(),
            harness.cli()
        );
    }
    if plugin["version"] != version {
        bail!(
            "{} has {PLUGIN_ID} {} installed, but the marketplace carries {version}",
            harness.name(),
            plugin["version"]
        );
    }
    let dir = install_dir(harness, home, &plugin, version);
    let command = mcp_command(harness, &dir)?;
    let server_version = probe_mcp(&command, &dir)?;
    Ok(json!({
        "plugin_version": version,
        "install_dir": dir.display().to_string(),
        "mcp_server_version": server_version,
    }))
}

/// `frg plugin install --harness <h> [--plugin-root DIR] [--no-migrate]`.
pub fn install(
    home: &Path,
    harness: Harness,
    plugin_root: Option<PathBuf>,
    migrate_legacy: bool,
) -> anyhow::Result<Value> {
    let root = plugin_root.unwrap_or_else(|| default_plugin_root(home));
    let root = std::fs::canonicalize(&root)
        .with_context(|| format!("plugin root {} does not exist", root.display()))?;
    let version = plugin_version(&root, harness)?;
    let marketplace = ensure_marketplace(harness, &root)?;
    let plugin = ensure_plugin(harness)?;
    let verified = verify(harness, home, &version)?;
    let migration = if migrate_legacy {
        migrate(home, harness)?
    } else {
        json!({"skipped": true})
    };
    Ok(json!({
        "harness": harness.name(),
        "plugin_root": root.display().to_string(),
        "marketplace": marketplace,
        "plugin": plugin,
        "verified": verified,
        "migration": migration,
    }))
}

// ── Migration (B4) ──────────────────────────────────────────────────────────

const REPORT: &str = "report.json";

/// Legacy forge MCP registrations Forge did not write and so never removes.
/// Each is reported with the exact manual change.
fn legacy_mcp_left(home: &Path, harness: Harness) -> Vec<Value> {
    let mut left = Vec::new();
    let mut note = |file: PathBuf, location: &str, manual: &str| {
        left.push(json!({
            "file": file.display().to_string(),
            "location": location,
            "reason": "a forge MCP server registered by another installer; the plugin now provides it",
            "manual": manual,
        }));
    };
    match harness {
        Harness::ClaudeCode => {
            for (file, manual) in [
                (
                    home.join(".claude.json"),
                    "claude mcp remove forge --scope user",
                ),
                (
                    home.join(".mcp.json"),
                    "delete mcpServers.forge from ~/.mcp.json",
                ),
            ] {
                if !file.exists() {
                    continue;
                }
                match read_json(&file) {
                    Ok(v) if v["mcpServers"].get(PLUGIN).is_some() => {
                        note(file, "mcpServers.forge", manual)
                    }
                    Ok(_) => {}
                    Err(e) => note(
                        file,
                        "mcpServers",
                        &format!("could not check ({e:#}); look for a forge server by hand"),
                    ),
                }
            }
        }
        Harness::Codex => {
            let codex_home = std::env::var_os("CODEX_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join(".codex"));
            let file = codex_home.join("config.toml");
            if file.exists() {
                let parsed = std::fs::read_to_string(&file)
                    .map_err(anyhow::Error::from)
                    .and_then(|t| t.parse::<toml::Table>().map_err(anyhow::Error::from));
                match parsed {
                    Ok(t) if t.get("mcp_servers").and_then(|m| m.get(PLUGIN)).is_some() => {
                        note(file, "mcp_servers.forge", "codex mcp remove forge")
                    }
                    Ok(_) => {}
                    Err(e) => note(
                        file,
                        "mcp_servers",
                        &format!("could not check ({e:#}); look for a forge server by hand"),
                    ),
                }
            }
        }
    }
    left
}

/// Remove the hook entries `frg init --global` wrote, now that the plugin
/// carries them. Called only after `verify` succeeded.
pub fn migrate(home: &Path, harness: Harness) -> anyhow::Result<Value> {
    let mut removed = Vec::new();
    let mut left = legacy_mcp_left(home, harness);
    let mut backups = BackupSet::new(home);
    let settings_path = home.join(".claude").join("settings.json");

    if harness == Harness::ClaudeCode {
        match claude_hooks::load(&settings_path) {
            Err(e) => left.push(json!({
                "file": settings_path.display().to_string(),
                "location": "hooks",
                "reason": format!("{e:#}"),
                "manual": "remove the two Read entries running `frg hook 2>/dev/null || true` by hand",
            })),
            Ok(None) => {}
            Ok(Some((text, mut settings))) => {
                for phase in claude_hooks::PHASES {
                    for entry in settings["hooks"][phase].as_array().into_iter().flatten() {
                        if claude_hooks::is_forge_entry(phase, entry) {
                            removed.push(json!({
                                "file": settings_path.display().to_string(),
                                "location": format!("hooks.{phase}"),
                                "entry": entry,
                            }));
                        }
                    }
                }
                if claude_hooks::uninstall(&mut settings)? > 0 {
                    backups.save(&settings_path)?;
                    let new_text = harness_files::json_text(&settings, Some(&text))?;
                    harness_files::write_atomic(&settings_path, &new_text)?;
                }
            }
        }
    }

    let mut report = json!({
        "tool": "frg",
        "tool_version": env!("CARGO_PKG_VERSION"),
        "action": "migrate",
        "harness": harness.name(),
        "created_at": jiff::Timestamp::now().to_string(),
        "removed": removed,
        "left": left,
    });
    if !removed.is_empty() {
        let dir = backups.ensure_dir()?;
        report["backup_dir"] = json!(dir.display().to_string());
        report["undo"] = json!(format!(
            "frg plugin migrate --undo {}",
            dir.file_name().unwrap_or_default().to_string_lossy()
        ));
        std::fs::write(
            dir.join(REPORT),
            serde_json::to_string_pretty(&report)? + "\n",
        )?;
    }
    Ok(report)
}

/// `frg plugin migrate --undo <ts>`: put back the entries a migration
/// removed. Entries are re-added one by one, never by restoring the whole
/// backup over changes made since; the current file is backed up first.
pub fn undo(home: &Path, stamp: &str) -> anyhow::Result<Value> {
    if stamp.contains('/') || stamp.contains("..") {
        bail!("{stamp} is not a backup timestamp");
    }
    let dir = harness_files::backups_root(home).join(stamp);
    let report = read_json(&dir.join(REPORT))?;
    if report["tool"] != "frg" {
        bail!(
            "{} was not written by frg; undo it with the tool that wrote it",
            dir.display()
        );
    }
    let mut restored = Vec::new();
    let mut backups = BackupSet::new(home);
    let removed = report["removed"].as_array().cloned().unwrap_or_default();
    let mut by_file: std::collections::BTreeMap<String, Vec<Value>> = Default::default();
    for item in removed {
        let file = item["file"]
            .as_str()
            .context("report entry without a file")?;
        by_file.entry(file.to_string()).or_default().push(item);
    }
    for (file, items) in by_file {
        let path = PathBuf::from(&file);
        let (text, mut settings) = match claude_hooks::load(&path)? {
            Some((text, value)) => (Some(text), value),
            None => (None, json!({})),
        };
        let mut changed = false;
        for item in &items {
            let phase = item["location"]
                .as_str()
                .and_then(|l| l.strip_prefix("hooks."))
                .context("report entry without a hooks location")?;
            changed |= claude_hooks::restore_entry(&mut settings, phase, &item["entry"])?;
            restored.push(item.clone());
        }
        if changed {
            if text.is_some() {
                backups.save(&path)?;
            }
            let new_text = harness_files::json_text(&settings, text.as_deref())?;
            harness_files::write_atomic(&path, &new_text)?;
        }
    }
    Ok(json!({
        "tool": "frg",
        "action": "undo",
        "undone": stamp,
        "restored": restored,
    }))
}
