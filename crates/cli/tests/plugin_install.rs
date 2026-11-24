//! `frg plugin install` / `frg plugin migrate` and the B4 migration, against
//! fake `claude` and `codex` executables on PATH. HOME and CODEX_HOME point at
//! a temporary directory, so the real ~/.claude and ~/.codex are never read
//! or written.
#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::{json, Value};

const VERSION: &str = env!("CARGO_PKG_VERSION");
const FORGE_ENTRY: &str =
    r#"{"matcher":"Read","hooks":[{"type":"command","command":"frg hook 2>/dev/null || true"}]}"#;

/// Another tool's settings, serde-pretty so it can be compared byte for byte.
const FOREIGN: &str = r#"{
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
  }
}
"#;

/// fmem's marketplace manifests as it stages them (ferrosa-memory plugin/).
const FMEM_CLAUDE_MARKET: &str = r#"{
  "name": "ferrosa",
  "description": "The Ferrosa plugins installed on this machine, at the version of the binaries beside them.",
  "owner": {
    "name": "Ferrosa",
    "url": "https://www.ferrosa.ai"
  },
  "plugins": [
    {
      "name": "ferrosa-memory",
      "source": "./ferrosa-memory",
      "description": "Durable agent memory on Ferrosa: the ferrosa-memory MCP server, the turn-recording hooks and the memory skills.",
      "category": "productivity"
    }
  ]
}
"#;
const FMEM_CODEX_MARKET: &str = r#"{
  "name": "ferrosa",
  "interface": {
    "displayName": "Ferrosa (installed on this machine)"
  },
  "plugins": [
    {
      "name": "ferrosa-memory",
      "source": {
        "source": "local",
        "path": "./ferrosa-memory"
      },
      "policy": {
        "installation": "AVAILABLE",
        "authentication": "ON_INSTALL"
      },
      "category": "Productivity"
    }
  ]
}
"#;

const FAKE_CLAUDE: &str = r#"#!/bin/sh
set -eu
S="$FAKE_STATE"
echo "claude $*" >> "$S/calls"
case "$*" in
  "plugin marketplace list --json")
    if [ -f "$S/claude-mkt" ]; then
      p=$(cat "$S/claude-mkt")
      printf '[{"name":"ferrosa","source":"directory","path":"%s","installLocation":"%s"}]\n' "$p" "$p"
    else echo '[]'; fi ;;
  "plugin marketplace add "*) printf '%s' "$4" > "$S/claude-mkt" ;;
  "plugin list --json")
    if [ -f "$S/claude-version" ]; then
      printf '[{"id":"forge@ferrosa","version":"%s","scope":"user","enabled":%s,"installPath":"%s"}]\n' \
        "$(cat "$S/claude-version")" "${FAKE_ENABLED:-true}" "$HOME/.claude/plugins/cache/ferrosa/forge/$(cat "$S/claude-version")"
    else echo '[]'; fi ;;
  "plugin install forge@ferrosa --scope user --json"|"plugin update forge@ferrosa --scope user --json")
    d="$HOME/.claude/plugins/cache/ferrosa/forge/$FAKE_VERSION"
    mkdir -p "$d"; cp -R "$(cat "$S/claude-mkt")/forge/." "$d/"
    printf '%s' "$FAKE_VERSION" > "$S/claude-version"
    echo '{"outcome":"ok"}' ;;
  *) echo "fake claude: unexpected: $*" >&2; exit 2 ;;
esac
"#;

const FAKE_CODEX: &str = r#"#!/bin/sh
set -eu
S="$FAKE_STATE"
echo "codex $*" >> "$S/calls"
case "$*" in
  "plugin marketplace list --json")
    if [ -f "$S/codex-mkt" ]; then
      printf '{"marketplaces":[{"name":"ferrosa","root":"%s"}]}\n' "$(cat "$S/codex-mkt")"
    else echo '{"marketplaces":[]}'; fi ;;
  "plugin marketplace add "*) printf '%s' "$4" > "$S/codex-mkt"; echo '{}' ;;
  "plugin list --json")
    if [ -f "$S/codex-plugin" ]; then
      printf '{"installed":[{"pluginId":"forge@ferrosa","version":"%s","installed":true,"enabled":true}],"available":[]}\n' "$FAKE_VERSION"
    else echo '{"installed":[],"available":[]}'; fi ;;
  "plugin add forge@ferrosa --json")
    d="$CODEX_HOME/plugins/cache/ferrosa/forge/$FAKE_VERSION"
    mkdir -p "$d"; cp -R "$(cat "$S/codex-mkt")/forge/." "$d/"
    touch "$S/codex-plugin"; echo '{}' ;;
  *) echo "fake codex: unexpected: $*" >&2; exit 2 ;;
esac
"#;

struct Env {
    _tmp: tempfile::TempDir,
    home: PathBuf,
    state: PathBuf,
    bin: PathBuf,
}

fn repo_plugin_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugin")
}

fn frg_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_frg"))
        .parent()
        .unwrap()
        .to_path_buf()
}

/// A temp HOME whose `~/.ferrosa/plugins/current` holds fmem's manifests.
fn setup() -> Env {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let state = tmp.path().join("state");
    let bin = tmp.path().join("bin");
    for d in [&home, &state, &bin] {
        std::fs::create_dir_all(d).unwrap();
    }
    for (name, body) in [("claude", FAKE_CLAUDE), ("codex", FAKE_CODEX)] {
        let path = bin.join(name);
        std::fs::write(&path, body).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let env = Env {
        _tmp: tmp,
        home,
        state,
        bin,
    };
    env.write(
        ".ferrosa/plugins/current/.claude-plugin/marketplace.json",
        FMEM_CLAUDE_MARKET,
    );
    env.write(
        ".ferrosa/plugins/current/.agents/plugins/marketplace.json",
        FMEM_CODEX_MARKET,
    );
    std::fs::create_dir_all(env.home.join(".codex")).unwrap();
    env
}

impl Env {
    fn command(&self, args: &[&str], with_frg_on_path: bool) -> Command {
        let mut path = format!("{}", self.bin.display());
        if with_frg_on_path {
            path.push_str(&format!(":{}", frg_dir().display()));
        }
        path.push_str(":/usr/bin:/bin");
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_frg"));
        cmd.args(args)
            .env("HOME", &self.home)
            .env("CODEX_HOME", self.home.join(".codex"))
            .env("PATH", path)
            .env("FAKE_STATE", &self.state)
            .env("FAKE_VERSION", VERSION)
            .env_remove("FORGE_BIN")
            .env_remove("FERROSA_HOME")
            .current_dir(&self.home);
        cmd
    }

    fn frg(&self, args: &[&str]) -> Output {
        self.command(args, true).output().unwrap()
    }

    fn install(&self, harness: &str) -> Output {
        self.frg(&["plugin", "install", "--harness", harness, "--json"])
    }

    fn calls(&self) -> String {
        std::fs::read_to_string(self.state.join("calls")).unwrap_or_default()
    }

    fn write(&self, rel: &str, text: &str) {
        let path = self.home.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    fn read(&self, rel: &str) -> String {
        std::fs::read_to_string(self.home.join(rel)).unwrap()
    }

    fn backups(&self) -> Vec<PathBuf> {
        let Ok(rd) = std::fs::read_dir(self.home.join(".ferrosa/backups/harness")) else {
            return Vec::new();
        };
        let mut v: Vec<PathBuf> = rd.map(|e| e.unwrap().path()).collect();
        v.sort();
        v
    }

    fn coordinator_claude_json(&self) -> String {
        format!(
            "{{\n  \"numStartups\": 12,\n  \"mcpServers\": {{\n    \"github\": {{\n      \"command\": \"gh-mcp\"\n    }},\n    \"forge\": {{\n      \"type\": \"stdio\",\n      \"command\": \"{}/.ferrosa/bin/frg\",\n      \"args\": [\n        \"--mcp\"\n      ]\n    }}\n  }},\n  \"projects\": {{}}\n}}\n",
            self.home.display()
        )
    }

    fn coordinator_codex_toml(&self) -> String {
        format!(
            "# my settings\nmodel = \"o3\"\n\n# github server\n[mcp_servers.github]\ncommand = \"gh-mcp\"\n\n[mcp_servers.forge]\ncommand = \"{}/.ferrosa/bin/frg\"\nargs = [\"--mcp\"]\n\n# after forge\n[profiles.x]\nmodel = \"o4\"\n",
            self.home.display()
        )
    }
}

const CLAUDE_JSON_WITHOUT_FORGE: &str = "{\n  \"numStartups\": 12,\n  \"mcpServers\": {\n    \"github\": {\n      \"command\": \"gh-mcp\"\n    }\n  },\n  \"projects\": {}\n}\n";
const CODEX_TOML_WITHOUT_FORGE: &str = "# my settings\nmodel = \"o3\"\n\n# github server\n[mcp_servers.github]\ncommand = \"gh-mcp\"\n\n# after forge\n[profiles.x]\nmodel = \"o4\"\n";

/// The single object of an install `--json` array.
fn report(output: &Output) -> Value {
    let v: Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|e| {
        panic!(
            "not JSON ({e}): {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    });
    assert_eq!(v.as_array().map(Vec::len), Some(1), "{v}");
    v[0].clone()
}

fn ok(output: &Output) -> Value {
    assert!(
        output.status.success(),
        "frg failed: {}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    report(output)
}

/// FOREIGN with Forge's two entries in it, as `frg init --global` leaves it.
fn foreign_with_legacy_hooks() -> String {
    let mut v: Value = serde_json::from_str(FOREIGN).unwrap();
    let entry: Value = serde_json::from_str(FORGE_ENTRY).unwrap();
    v["hooks"]["PreToolUse"]
        .as_array_mut()
        .unwrap()
        .push(entry.clone());
    v["hooks"]["PostToolUse"] = Value::Array(vec![entry]);
    serde_json::to_string_pretty(&v).unwrap() + "\n"
}

fn signatures(report: &Value) -> Vec<String> {
    let mut v: Vec<String> = report["migration"]["removed"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["signature"].as_str().unwrap().to_string())
        .collect();
    v.sort();
    v
}

// ── The shared marketplace ──────────────────────────────────────────────────

#[test]
fn install_adds_forge_to_fmems_marketplace_and_keeps_its_entry() {
    let env = setup();

    let r = ok(&env.install("claude-code"));

    let root = ".ferrosa/plugins/current";
    for (rel, original) in [
        (".claude-plugin/marketplace.json", FMEM_CLAUDE_MARKET),
        (".agents/plugins/marketplace.json", FMEM_CODEX_MARKET),
    ] {
        let after: Value = serde_json::from_str(&env.read(&format!("{root}/{rel}"))).unwrap();
        let mut expected: Value = serde_json::from_str(original).unwrap();
        let plugins = after["plugins"].as_array().unwrap();
        assert_eq!(plugins.len(), 2, "{rel}: {after}");
        assert_eq!(
            plugins[0], expected["plugins"][0],
            "{rel}: fmem's entry changed"
        );
        assert_eq!(plugins[1]["name"], "forge");
        expected["plugins"]
            .as_array_mut()
            .unwrap()
            .push(plugins[1].clone());
        assert_eq!(
            after, expected,
            "{rel}: something besides forge's entry changed"
        );
    }
    let staged: Value =
        serde_json::from_str(&env.read(&format!("{root}/forge/.claude-plugin/plugin.json")))
            .unwrap();
    assert_eq!(staged["version"], VERSION);
    let calls = env.calls();
    assert!(calls.contains("claude plugin marketplace add "), "{calls}");
    assert!(
        calls.contains(&format!("{}/{root}", env.home.display())),
        "{calls}"
    );
    assert!(calls.contains("claude plugin install forge@ferrosa --scope user --json"));
    assert_eq!(r["status"], "changed");
    assert_eq!(r["marketplace"], "added");
    assert_eq!(r["plugin"], "installed");
    assert!(r["verified"].as_str().unwrap().contains(VERSION), "{r}");
}

#[test]
fn a_second_install_writes_nothing_and_changes_nothing() {
    let env = setup();
    ok(&env.install("claude-code"));
    let market = ".ferrosa/plugins/current/.claude-plugin/marketplace.json";
    let before = std::fs::metadata(env.home.join(market))
        .unwrap()
        .modified()
        .unwrap();
    let calls_before = env.calls().len();

    let r = ok(&env.install("claude-code"));

    assert_eq!(r["status"], "already_right", "{r}");
    assert_eq!(r["marketplace"], "already_registered");
    assert_eq!(r["plugin"], "already_installed");
    let after = std::fs::metadata(env.home.join(market))
        .unwrap()
        .modified()
        .unwrap();
    assert_eq!(before, after, "the shared manifest was rewritten");
    let new_calls = env.calls()[calls_before..].to_string();
    assert!(
        !new_calls.contains(" add ") && !new_calls.contains(" install "),
        "{new_calls}"
    );
}

#[test]
fn a_different_forge_entry_in_the_marketplace_is_refused_and_nothing_is_written() {
    let env = setup();
    let mut theirs: Value = serde_json::from_str(FMEM_CLAUDE_MARKET).unwrap();
    theirs["plugins"]
        .as_array_mut()
        .unwrap()
        .push(json!({"name": "forge", "source": "./someone-elses-forge"}));
    let text = serde_json::to_string_pretty(&theirs).unwrap() + "\n";
    let rel = ".ferrosa/plugins/current/.claude-plugin/marketplace.json";
    env.write(rel, &text);

    let output = env.install("claude-code");

    assert!(!output.status.success());
    let r = report(&output);
    assert_eq!(r["status"], "failed");
    assert!(
        r["error"].as_str().unwrap().contains("will not overwrite"),
        "{r}"
    );
    assert_eq!(env.read(rel), text);
    assert_eq!(
        env.read(".ferrosa/plugins/current/.agents/plugins/marketplace.json"),
        FMEM_CODEX_MARKET,
        "the other manifest was written before the refusal"
    );
    assert!(!env.home.join(".ferrosa/plugins/current/forge").exists());
    assert!(!env.calls().contains("marketplace add"));
}

#[test]
fn a_missing_default_root_fails_loudly() {
    let env = setup();
    std::fs::remove_dir_all(env.home.join(".ferrosa/plugins/current")).unwrap();

    let output = env.install("claude-code");

    assert!(!output.status.success());
    assert!(report(&output)["error"]
        .as_str()
        .unwrap()
        .contains("does not exist"));
    assert!(!env.home.join(".ferrosa/plugins/current").exists());
}

#[test]
fn an_explicit_plugin_root_is_created_and_used() {
    let env = setup();
    let root = env.home.join("elsewhere");

    ok(&env.frg(&[
        "plugin",
        "install",
        "--harness",
        "claude-code",
        "--plugin-root",
        root.to_str().unwrap(),
        "--json",
    ]));

    let market: Value =
        serde_json::from_str(&env.read("elsewhere/.claude-plugin/marketplace.json")).unwrap();
    assert_eq!(market["name"], "ferrosa");
    assert_eq!(market["plugins"][0]["name"], "forge");
}

#[test]
fn a_ferrosa_marketplace_registered_elsewhere_is_left_alone() {
    let env = setup();
    std::fs::write(env.state.join("claude-mkt"), "/somewhere/else").unwrap();

    let output = env.install("claude-code");

    assert!(!output.status.success());
    let err = report(&output)["error"].as_str().unwrap().to_string();
    assert!(
        err.contains("/somewhere/else") && err.contains("will not re-point"),
        "{err}"
    );
    let calls = env.calls();
    assert!(
        !calls.contains("marketplace add") && !calls.contains("plugin install"),
        "{calls}"
    );
}

#[test]
fn an_older_installed_version_is_updated() {
    let env = setup();
    ok(&env.install("claude-code"));
    std::fs::write(env.state.join("claude-version"), "0.0.1").unwrap();

    let r = ok(&env.install("claude-code"));

    assert_eq!(r["plugin"], "updated");
    assert!(env
        .calls()
        .contains("claude plugin update forge@ferrosa --scope user --json"));
}

// ── Migration ───────────────────────────────────────────────────────────────

#[test]
fn claude_migration_removes_init_hooks_and_the_coordinator_server_exactly() {
    let env = setup();
    env.write(".claude/settings.json", &foreign_with_legacy_hooks());
    env.write(".claude.json", &env.coordinator_claude_json());

    let r = ok(&env.install("claude-code"));

    assert_eq!(
        signatures(&r),
        [
            "claude-json-mcp-coordinator-forge",
            "frg-init-hook-PostToolUse-Read",
            "frg-init-hook-PreToolUse-Read"
        ]
    );
    assert_eq!(env.read(".claude/settings.json"), FOREIGN);
    assert_eq!(env.read(".claude.json"), CLAUDE_JSON_WITHOUT_FORGE);

    let m = &r["migration"];
    assert_eq!(m["tool"], "frg");
    assert_eq!(m["harnesses"], json!(["claude-code"]));
    assert_eq!(m["dry_run"], false);
    assert_eq!(m["errors"], json!([]));
    assert_eq!(m["files"].as_array().unwrap().len(), 2);
    assert!(m["undo"]
        .as_str()
        .unwrap()
        .starts_with("frg plugin migrate --undo "));
    let dir = PathBuf::from(m["backup_dir"].as_str().unwrap());
    assert_eq!(
        std::fs::read_to_string(dir.join("files/.claude.json")).unwrap(),
        env.coordinator_claude_json()
    );
    let written: Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join("report.json")).unwrap()).unwrap();
    assert_eq!(written["removed"], m["removed"]);
}

#[test]
fn an_edited_forge_server_is_left_and_reported() {
    let env = setup();
    let edited = env.coordinator_claude_json().replace(
        "\"type\": \"stdio\",",
        "\"type\": \"stdio\",\n      \"env\": {\"X\": \"1\"},",
    );
    env.write(".claude.json", &edited);

    let r = ok(&env.install("claude-code"));

    assert_eq!(env.read(".claude.json"), edited);
    let left = r["migration"]["left"].as_array().unwrap();
    assert_eq!(left.len(), 1, "{left:?}");
    assert_eq!(left[0]["status"], "edited");
    assert_eq!(left[0]["manual"], "claude mcp remove forge --scope user");
}

#[test]
fn codex_migration_removes_the_coordinator_table_and_keeps_comments() {
    let env = setup();
    env.write(".codex/config.toml", &env.coordinator_codex_toml());
    env.write(".claude/settings.json", &foreign_with_legacy_hooks());

    let r = ok(&env.install("codex"));

    assert_eq!(signatures(&r), ["codex-toml-mcp-coordinator-forge"]);
    assert_eq!(env.read(".codex/config.toml"), CODEX_TOML_WITHOUT_FORGE);
    assert_eq!(
        env.read(".claude/settings.json"),
        foreign_with_legacy_hooks()
    );
    let calls = env.calls();
    assert!(
        calls.contains("codex plugin add forge@ferrosa --json"),
        "{calls}"
    );
    assert!(!calls.contains("claude "), "{calls}");
}

#[test]
fn a_codex_table_with_a_comment_inside_is_left() {
    let env = setup();
    let commented = env
        .coordinator_codex_toml()
        .replace("args = [\"--mcp\"]", "# mine\nargs = [\"--mcp\"]");
    env.write(".codex/config.toml", &commented);

    let r = ok(&env.install("codex"));

    assert_eq!(env.read(".codex/config.toml"), commented);
    assert_eq!(r["migration"]["left"][0]["status"], "edited");
    assert_eq!(r["migration"]["removed"], json!([]));
}

#[test]
fn undo_restores_every_file_and_cannot_run_twice() {
    let env = setup();
    env.write(".claude/settings.json", &foreign_with_legacy_hooks());
    env.write(".claude.json", &env.coordinator_claude_json());
    let r = ok(&env.install("claude-code"));
    let stamp = r["migration"]["undo"]
        .as_str()
        .unwrap()
        .rsplit(' ')
        .next()
        .unwrap()
        .to_string();

    let out = env.frg(&["plugin", "migrate", "--undo", &stamp, "--json"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let undo: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(undo["action"], "undo");
    assert_eq!(undo["errors"], json!([]));
    assert_eq!(
        env.read(".claude/settings.json"),
        foreign_with_legacy_hooks()
    );
    assert_eq!(env.read(".claude.json"), env.coordinator_claude_json());

    let again = env.frg(&["plugin", "migrate", "--undo", &stamp, "--json"]);
    assert!(!again.status.success());
}

#[test]
fn undo_refuses_when_a_file_changed_since_and_restores_nothing() {
    let env = setup();
    env.write(".claude/settings.json", &foreign_with_legacy_hooks());
    env.write(".claude.json", &env.coordinator_claude_json());
    let r = ok(&env.install("claude-code"));
    let stamp = r["migration"]["undo"]
        .as_str()
        .unwrap()
        .rsplit(' ')
        .next()
        .unwrap()
        .to_string();
    env.write(".claude.json", "{\"numStartups\": 13}\n");

    let out = env.frg(&["plugin", "migrate", "--undo", &stamp, "--json"]);

    assert!(!out.status.success());
    assert_eq!(env.read(".claude.json"), "{\"numStartups\": 13}\n");
    assert_eq!(
        env.read(".claude/settings.json"),
        FOREIGN,
        "half an undo was applied"
    );
}

#[test]
fn a_dry_run_lists_what_would_go_and_writes_nothing() {
    let env = setup();
    ok(&env.frg(&[
        "plugin",
        "install",
        "--harness",
        "claude-code",
        "--no-migrate",
        "--json",
    ]));
    env.write(".claude/settings.json", &foreign_with_legacy_hooks());
    env.write(".claude.json", &env.coordinator_claude_json());

    let out = env.frg(&[
        "plugin",
        "migrate",
        "--harness",
        "claude-code",
        "--dry-run",
        "--json",
    ]);

    assert!(out.status.success());
    let m: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(m["dry_run"], true);
    assert_eq!(m["removed"].as_array().unwrap().len(), 3);
    assert_eq!(
        env.read(".claude/settings.json"),
        foreign_with_legacy_hooks()
    );
    assert_eq!(env.read(".claude.json"), env.coordinator_claude_json());
    assert!(env.backups().is_empty());
}

#[test]
fn migrate_without_a_verified_plugin_removes_nothing() {
    let env = setup();
    env.write(".claude/settings.json", &foreign_with_legacy_hooks());

    let out = env.frg(&["plugin", "migrate", "--harness", "claude-code", "--json"]);

    assert!(!out.status.success());
    let m: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(m["removed"], json!([]));
    assert_eq!(m["left"][0]["status"], "removable");
    assert!(!m["errors"].as_array().unwrap().is_empty());
    assert_eq!(
        env.read(".claude/settings.json"),
        foreign_with_legacy_hooks()
    );
}

#[test]
fn nothing_is_migrated_when_frg_is_missing_and_the_server_cannot_answer() {
    let env = setup();
    let before = foreign_with_legacy_hooks();
    env.write(".claude/settings.json", &before);

    let output = env
        .command(
            &["plugin", "install", "--harness", "claude-code", "--json"],
            false,
        )
        .output()
        .unwrap();

    assert!(!output.status.success());
    let r = report(&output);
    assert!(
        r["error"]
            .as_str()
            .unwrap()
            .contains("frg is not installed"),
        "{r}"
    );
    assert_eq!(r["migration"], Value::Null);
    assert_eq!(env.read(".claude/settings.json"), before);
    assert!(env.backups().is_empty());
}

#[test]
fn a_disabled_plugin_fails_verification_and_migrates_nothing() {
    let env = setup();
    let before = foreign_with_legacy_hooks();
    env.write(".claude/settings.json", &before);

    let output = env
        .command(
            &["plugin", "install", "--harness", "claude-code", "--json"],
            true,
        )
        .env("FAKE_ENABLED", "false")
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(report(&output)["error"]
        .as_str()
        .unwrap()
        .contains("disabled"));
    assert_eq!(env.read(".claude/settings.json"), before);
}

#[test]
fn without_json_the_output_is_text() {
    let env = setup();
    let out = env.frg(&["plugin", "install", "--harness", "claude-code"]);
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(serde_json::from_str::<Value>(&text).is_err(), "{text}");
    assert!(text.contains("claude-code: changed"), "{text}");
}

// ── The plugin tree itself ──────────────────────────────────────────────────

fn read_json(rel: &str) -> Value {
    let path = repo_plugin_root().join(rel);
    serde_json::from_str(&std::fs::read_to_string(&path).unwrap())
        .unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

#[test]
fn the_repo_marketplace_is_the_shared_ferrosa_one() {
    for rel in ["forge/.claude-plugin/plugin.json", "forge/plugin.json"] {
        assert_eq!(read_json(rel)["version"], VERSION, "{rel}");
    }
    for rel in [
        ".claude-plugin/marketplace.json",
        ".agents/plugins/marketplace.json",
    ] {
        let m = read_json(rel);
        assert_eq!(m["name"], "ferrosa", "{rel}");
        assert_eq!(m["plugins"][0]["name"], "forge", "{rel}");
    }
}

#[test]
fn plugin_hooks_are_exactly_what_frg_init_installs() {
    let hooks = read_json("forge/hooks/hooks.json");
    let entry: Value = serde_json::from_str(FORGE_ENTRY).unwrap();
    let events: Vec<&String> = hooks["hooks"].as_object().unwrap().keys().collect();
    assert_eq!(events, ["PreToolUse", "PostToolUse"]);
    assert_eq!(
        hooks["hooks"]["PreToolUse"],
        Value::Array(vec![entry.clone()])
    );
    assert_eq!(hooks["hooks"]["PostToolUse"], Value::Array(vec![entry]));
}

#[test]
fn codex_gets_no_hooks_and_the_same_server() {
    let plugin = read_json("forge/plugin.json");
    assert_eq!(
        plugin["extensions"]["com.openai"]["hooks"],
        Value::Array(vec![])
    );
    let codex = read_json("forge/mcp.json");
    let claude = read_json("forge/.mcp.json");
    assert_eq!(codex["mcpServers"]["forge"]["command"], "./scripts/frg-mcp");
    assert_eq!(
        claude["mcpServers"]["forge"]["command"],
        "${CLAUDE_PLUGIN_ROOT}/scripts/frg-mcp"
    );
}

#[test]
fn the_mcp_launcher_fails_loudly_without_frg() {
    let tmp = tempfile::tempdir().unwrap();
    let output = Command::new(repo_plugin_root().join("forge/scripts/frg-mcp"))
        .env("HOME", tmp.path())
        .env("PATH", "/usr/bin:/bin")
        .env_remove("FORGE_BIN")
        .env_remove("FERROSA_HOME")
        .output()
        .unwrap();
    // A machine with frg in Homebrew's bin or /usr/local/bin would find it;
    // this test is about the failure, so only assert when none is there.
    let system_frg = [
        "/opt/homebrew/bin/frg",
        "/usr/local/bin/frg",
        "/home/linuxbrew/.linuxbrew/bin/frg",
    ]
    .iter()
    .any(|p| Path::new(p).exists());
    if !system_frg {
        assert_eq!(output.status.code(), Some(127));
        assert!(String::from_utf8_lossy(&output.stderr).contains("frg is not installed"));
    }
}

#[test]
fn the_mcp_launcher_runs_the_named_frg_with_mcp() {
    let tmp = tempfile::tempdir().unwrap();
    let fake = tmp.path().join("frg");
    std::fs::write(&fake, "#!/bin/sh\necho \"args: $*\"\n").unwrap();
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();

    let output = Command::new(repo_plugin_root().join("forge/scripts/frg-mcp"))
        .env("FORGE_BIN", &fake)
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "args: --mcp"
    );

    let missing = Command::new(repo_plugin_root().join("forge/scripts/frg-mcp"))
        .env("FORGE_BIN", tmp.path().join("nope"))
        .output()
        .unwrap();
    assert_eq!(
        missing.status.code(),
        Some(127),
        "a set FORGE_BIN never falls through"
    );
}
