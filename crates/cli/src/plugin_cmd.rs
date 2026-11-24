//! `frg plugin install` and `frg plugin migrate` (harness-plugins B1–B5).
//!
//! The forge plugin lives in the shared local marketplace `ferrosa` (default
//! root `~/.ferrosa/plugins/current/`, beside fmem's `ferrosa-memory`) and is
//! installed through the harness's own CLI (`claude plugin ...`, `codex
//! plugin ...`). Forge never writes the harness's plugin state itself. Only
//! after the plugin is installed and its MCP server has answered does the B4
//! migration remove legacy entries (see `plugin_migrate`).
//!
//! `--json` prints the shapes `fmem harness plugin install --json` and `fmem
//! harness migrate --json` use (ferrosa-memory docs/harness-plugins.md).

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use anyhow::{bail, Context};
use serde_json::{json, Value};

use crate::plugin_migrate;
use crate::plugin_stage::{self, MARKETPLACE, PLUGIN};

const PLUGIN_ID: &str = "forge@ferrosa";
const VERSION: &str = env!("CARGO_PKG_VERSION");
const MCP_TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum Harness {
    #[value(name = "claude-code")]
    ClaudeCode,
    Codex,
}

impl Harness {
    pub fn name(self) -> &'static str {
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
}

/// The shared marketplace root the Ferrosa installer maintains (B1).
pub fn default_plugin_root(home: &Path) -> PathBuf {
    home.join(".ferrosa").join("plugins").join("current")
}

fn read_json(path: &Path) -> anyhow::Result<Value> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("cannot read {}", path.display()))?;
    serde_json::from_str(&text).with_context(|| format!("{} is not valid JSON", path.display()))
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

/// Claude Code can exit 0 from a failed install; its `--json` line carries
/// the real outcome.
fn run_claude_change(args: &[&str]) -> anyhow::Result<()> {
    let out = run(Harness::ClaudeCode, args)?;
    let last = out
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("");
    match serde_json::from_str::<Value>(last) {
        Ok(v) if v.get("outcome").is_some() && v["outcome"] != "ok" => {
            bail!(
                "`claude {}` did not succeed: {}",
                args.join(" "),
                out.trim()
            )
        }
        _ => Ok(()),
    }
}

fn same_dir(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

/// The directory a registered `ferrosa` marketplace points at, if any.
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
            "{} already has a marketplace named {MARKETPLACE} at {}, not {}. frg will not re-point it, \
             since other Ferrosa plugins may come from it; run `fmem harness plugin install` or \
             pass --plugin-root {}",
            harness.name(),
            path.display(),
            root.display(),
            path.display()
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
    let action = match installed_plugin(harness)? {
        Some(p) if p["version"] == VERSION => return Ok("already_installed"),
        Some(_) => "updated",
        None => "installed",
    };
    match (harness, action) {
        (Harness::ClaudeCode, "installed") => {
            run_claude_change(&["plugin", "install", PLUGIN_ID, "--scope", "user", "--json"])?
        }
        (Harness::ClaudeCode, _) => {
            run_claude_change(&["plugin", "update", PLUGIN_ID, "--scope", "user", "--json"])?
        }
        // `codex plugin add` copies the marketplace's current version into
        // its cache, so it both installs and updates.
        (Harness::Codex, _) => {
            run(harness, &["plugin", "add", PLUGIN_ID, "--json"])?;
        }
    }
    Ok(action)
}

/// The installed plugin's directory, as the harness copied it.
fn install_dir(harness: Harness, home: &Path, plugin: &Value) -> PathBuf {
    match harness {
        Harness::ClaudeCode => PathBuf::from(plugin["installPath"].as_str().unwrap_or_default()),
        // Codex's documented cache layout: plugins/cache/<market>/<plugin>/<version>.
        Harness::Codex => plugin_migrate::codex_home(home)
            .join("plugins/cache")
            .join(MARKETPLACE)
            .join(PLUGIN)
            .join(VERSION),
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
        "clientInfo": {"name": "frg-plugin-verify", "version": VERSION}
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

/// Confirm the plugin is installed, enabled, this frg's version, and that its
/// MCP server answers. Returns how it was verified.
fn verify(harness: Harness, home: &Path) -> anyhow::Result<String> {
    let plugin = installed_plugin(harness)?
        .with_context(|| format!("{PLUGIN_ID} is not installed in {}", harness.name()))?;
    if plugin["enabled"] != true {
        bail!(
            "{PLUGIN_ID} is installed in {} but disabled; enable it with `{} plugin enable {PLUGIN_ID}`",
            harness.name(),
            harness.cli()
        );
    }
    if plugin["version"] != VERSION {
        bail!(
            "{} has {PLUGIN_ID} {} installed, but this frg is {VERSION}",
            harness.name(),
            plugin["version"]
        );
    }
    let dir = install_dir(harness, home, &plugin);
    let command = mcp_command(harness, &dir)?;
    let server = probe_mcp(&command, &dir)?;
    Ok(format!(
        "forge {server} answered initialize from {}",
        command.display()
    ))
}

/// The install steps up to verification. Fills `report` as it goes, so a
/// failure still reports what was done.
fn install_steps(
    home: &Path,
    harness: Harness,
    root: &Path,
    report: &mut Value,
) -> anyhow::Result<bool> {
    let staged = plugin_stage::stage(root)?;
    let mut changed = !staged.steps.is_empty();
    let mut steps = staged.steps;
    let marketplace = ensure_marketplace(harness, root)?;
    report["marketplace"] = json!(marketplace);
    if marketplace == "added" {
        changed = true;
        steps.push(format!(
            "registered marketplace {MARKETPLACE} at {}",
            root.display()
        ));
    }
    let plugin = ensure_plugin(harness)?;
    report["plugin"] = json!(plugin);
    if plugin != "already_installed" {
        changed = true;
        steps.push(format!("{plugin} {PLUGIN_ID} {VERSION}"));
    }
    report["steps"] = json!(steps);
    report["verified"] = json!(verify(harness, home)?);
    Ok(changed)
}

/// `frg plugin install --harness <h> [--plugin-root DIR] [--no-migrate]`.
/// Returns the fmem-shaped report object and whether it succeeded.
pub fn install(
    home: &Path,
    harness: Harness,
    plugin_root: Option<PathBuf>,
    migrate_legacy: bool,
) -> (Value, bool) {
    let explicit = plugin_root.is_some();
    let root = plugin_root.unwrap_or_else(|| default_plugin_root(home));
    let mut report = json!({
        "harness": harness.name(),
        "status": "failed",
        "plugin_root": root.display().to_string(),
        "plugin_version": VERSION,
        "marketplace": Value::Null,
        "plugin": Value::Null,
        "verified": Value::Null,
        "error": Value::Null,
        "steps": [],
        "migration": Value::Null,
    });
    if !explicit && !root.is_dir() {
        report["error"] = json!(format!(
            "{} does not exist. Install Ferrosa, which creates it, or pass --plugin-root DIR",
            root.display()
        ));
        return (report, false);
    }
    let changed = match install_steps(home, harness, &root, &mut report) {
        Ok(changed) => changed,
        Err(e) => {
            report["error"] = json!(format!("{e:#}"));
            return (report, false);
        }
    };
    let mut ok = true;
    let mut removed_any = false;
    if migrate_legacy {
        match plugin_migrate::migrate(home, harness, Ok(()), false) {
            Ok(m) => {
                ok = m["errors"].as_array().is_none_or(Vec::is_empty);
                removed_any = m["removed"].as_array().is_some_and(|r| !r.is_empty());
                report["migration"] = m;
            }
            Err(e) => {
                report["error"] = json!(format!("migration: {e:#}"));
                ok = false;
            }
        }
    }
    report["status"] = json!(if changed || removed_any {
        "changed"
    } else {
        "already_right"
    });
    (report, ok)
}

/// `frg plugin migrate --harness <h> [--dry-run]`: verify the plugin first,
/// then migrate (or report what would be removed).
pub fn migrate(home: &Path, harness: Harness, dry_run: bool) -> anyhow::Result<Value> {
    let verified = verify(harness, home)
        .map(|_| ())
        .map_err(|e| format!("{e:#}"));
    plugin_migrate::migrate(home, harness, verified, dry_run)
}

/// Human-readable lines for an install report.
pub fn describe_install(report: &Value) -> String {
    let mut out = String::new();
    for step in report["steps"].as_array().into_iter().flatten() {
        out.push_str(&format!("{}\n", step.as_str().unwrap_or_default()));
    }
    if let Some(v) = report["verified"].as_str() {
        out.push_str(&format!("verified: {v}\n"));
    }
    out.push_str(&describe_migration(&report["migration"]));
    out.push_str(&format!(
        "{}: {}\n",
        report["harness"].as_str().unwrap_or_default(),
        report["status"].as_str().unwrap_or_default()
    ));
    out
}

/// Human-readable lines for a migrate or undo report.
pub fn describe_migration(m: &Value) -> String {
    let mut out = String::new();
    let list = |key: &str| m[key].as_array().cloned().unwrap_or_default();
    for r in list("removed") {
        out.push_str(&format!(
            "removed {} {} ({})\n",
            r["file"].as_str().unwrap_or_default(),
            r["location"].as_str().unwrap_or_default(),
            r["signature"].as_str().unwrap_or_default()
        ));
    }
    for l in list("left") {
        out.push_str(&format!(
            "left {} {}: {}\n",
            l["file"].as_str().unwrap_or_default(),
            l["location"].as_str().unwrap_or_default(),
            l["manual"].as_str().unwrap_or_default()
        ));
    }
    for r in list("restored") {
        out.push_str(&format!("restored {}\n", r.as_str().unwrap_or_default()));
    }
    for e in list("errors") {
        let msg = e["message"]
            .as_str()
            .map(str::to_string)
            .unwrap_or_else(|| e.to_string());
        out.push_str(&format!("error: {msg}\n"));
    }
    if let Some(undo) = m["undo"].as_str() {
        out.push_str(&format!("undo with: {undo}\n"));
    }
    out
}
