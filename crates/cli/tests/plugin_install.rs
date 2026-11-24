//! `frg plugin install` and the B4 migration, against fake `claude` and
//! `codex` executables on PATH. HOME and CODEX_HOME point at a temporary
//! directory, so the real ~/.claude and ~/.codex are never read or written.
#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;

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

const FAKE_CLAUDE: &str = r#"#!/bin/sh
set -eu
S="$FAKE_STATE"
echo "claude $*" >> "$S/calls"
case "$*" in
  "plugin marketplace list --json")
    if [ -f "$S/claude-mkt" ]; then
      p=$(cat "$S/claude-mkt")
      printf '[{"name":"forge","source":"directory","path":"%s","installLocation":"%s"}]\n' "$p" "$p"
    else echo '[]'; fi ;;
  "plugin marketplace add "*) printf '%s' "$4" > "$S/claude-mkt" ;;
  "plugin list --json")
    if [ -f "$S/claude-plugin" ]; then
      printf '[{"id":"forge@forge","version":"%s","scope":"user","enabled":%s,"installPath":"%s"}]\n' \
        "$FAKE_VERSION" "${FAKE_ENABLED:-true}" "$(cat "$S/claude-plugin")"
    else echo '[]'; fi ;;
  "plugin install forge@forge --scope user")
    d="$HOME/.claude/plugins/cache/forge/forge/$FAKE_VERSION"
    mkdir -p "$d"; cp -R "$(cat "$S/claude-mkt")/forge/." "$d/"
    printf '%s' "$d" > "$S/claude-plugin" ;;
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
      printf '{"marketplaces":[{"name":"forge","root":"%s"}]}\n' "$(cat "$S/codex-mkt")"
    else echo '{"marketplaces":[]}'; fi ;;
  "plugin marketplace add "*) printf '%s' "$4" > "$S/codex-mkt"; echo '{}' ;;
  "plugin list --json")
    if [ -f "$S/codex-plugin" ]; then
      printf '{"installed":[{"pluginId":"forge@forge","version":"%s","installed":true,"enabled":true}],"available":[]}\n' "$FAKE_VERSION"
    else echo '{"installed":[],"available":[]}'; fi ;;
  "plugin add forge@forge --json")
    d="$CODEX_HOME/plugins/cache/forge/forge/$FAKE_VERSION"
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
    Env {
        _tmp: tmp,
        home,
        state,
        bin,
    }
}

impl Env {
    fn frg(&self, args: &[&str], with_frg_on_path: bool) -> Output {
        let frg = PathBuf::from(env!("CARGO_BIN_EXE_frg"));
        let mut path = format!("{}", self.bin.display());
        if with_frg_on_path {
            path.push_str(&format!(":{}", frg.parent().unwrap().display()));
        }
        path.push_str(":/usr/bin:/bin");
        Command::new(&frg)
            .args(args)
            .env("HOME", &self.home)
            .env("CODEX_HOME", self.home.join(".codex"))
            .env("PATH", path)
            .env("FAKE_STATE", &self.state)
            .env("FAKE_VERSION", VERSION)
            .env_remove("FORGE_BIN")
            .current_dir(&self.home)
            .output()
            .unwrap()
    }

    fn install(&self, harness: &str) -> Output {
        let root = repo_plugin_root();
        self.frg(
            &[
                "plugin",
                "install",
                "--harness",
                harness,
                "--plugin-root",
                root.to_str().unwrap(),
            ],
            true,
        )
    }

    fn calls(&self) -> String {
        std::fs::read_to_string(self.state.join("calls")).unwrap_or_default()
    }

    fn settings_path(&self) -> PathBuf {
        self.home.join(".claude/settings.json")
    }

    fn write_settings(&self, text: &str) {
        std::fs::create_dir_all(self.home.join(".claude")).unwrap();
        std::fs::write(self.settings_path(), text).unwrap();
    }

    fn settings(&self) -> String {
        std::fs::read_to_string(self.settings_path()).unwrap()
    }

    fn backups(&self) -> Vec<PathBuf> {
        let Ok(rd) = std::fs::read_dir(self.home.join(".ferrosa/backups/harness")) else {
            return Vec::new();
        };
        let mut v: Vec<PathBuf> = rd.map(|e| e.unwrap().path()).collect();
        v.sort();
        v
    }
}

fn ok(output: &Output) -> Value {
    assert!(
        output.status.success(),
        "frg failed: {}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
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

#[test]
fn claude_install_registers_installs_verifies_then_migrates_only_forges_entries() {
    let env = setup();
    let before = foreign_with_legacy_hooks();
    env.write_settings(&before);

    let report = ok(&env.install("claude-code"));

    let calls = env.calls();
    assert!(calls.contains("claude plugin marketplace add "), "{calls}");
    assert!(
        calls.contains("claude plugin install forge@forge --scope user"),
        "{calls}"
    );
    assert_eq!(report["verified"]["mcp_server_version"], VERSION);
    assert_eq!(report["migration"]["removed"].as_array().unwrap().len(), 2);
    assert_eq!(env.settings(), FOREIGN, "only Forge's two entries may go");

    let backups = env.backups();
    assert_eq!(backups.len(), 1);
    let saved = std::fs::read_to_string(backups[0].join("files/.claude/settings.json")).unwrap();
    assert_eq!(saved, before);
    let written: Value =
        serde_json::from_str(&std::fs::read_to_string(backups[0].join("report.json")).unwrap())
            .unwrap();
    assert_eq!(written["removed"], report["migration"]["removed"]);
}

#[test]
fn a_second_install_changes_nothing() {
    let env = setup();
    env.write_settings(&foreign_with_legacy_hooks());
    ok(&env.install("claude-code"));
    let calls_before = env.calls();
    let backups_before = env.backups().len();

    let report = ok(&env.install("claude-code"));

    let new_calls = env.calls()[calls_before.len()..].to_string();
    assert!(!new_calls.contains(" add "), "{new_calls}");
    assert!(!new_calls.contains(" install "), "{new_calls}");
    assert_eq!(report["marketplace"], "already_registered");
    assert_eq!(report["plugin"], "already_installed");
    assert_eq!(env.settings(), FOREIGN);
    assert_eq!(env.backups().len(), backups_before);
}

#[test]
fn undo_puts_the_removed_entries_back_and_keeps_the_rest() {
    let env = setup();
    env.write_settings(&foreign_with_legacy_hooks());
    let report = ok(&env.install("claude-code"));
    let stamp = Path::new(report["migration"]["backup_dir"].as_str().unwrap())
        .file_name()
        .unwrap()
        .to_string_lossy()
        .to_string();

    ok(&env.frg(&["plugin", "migrate", "--undo", &stamp], true));

    assert_eq!(env.settings(), foreign_with_legacy_hooks());
}

#[test]
fn nothing_is_migrated_when_frg_is_missing_and_the_server_cannot_answer() {
    let env = setup();
    let before = foreign_with_legacy_hooks();
    env.write_settings(&before);
    let root = repo_plugin_root();

    let output = env.frg(
        &[
            "plugin",
            "install",
            "--harness",
            "claude-code",
            "--plugin-root",
            root.to_str().unwrap(),
        ],
        false,
    );

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("frg is not installed"), "{stderr}");
    assert_eq!(env.settings(), before);
    assert!(env.backups().is_empty());
}

#[test]
fn a_disabled_plugin_fails_verification_and_migrates_nothing() {
    let env = setup();
    let before = foreign_with_legacy_hooks();
    env.write_settings(&before);
    let root = repo_plugin_root();
    let frg_dir = PathBuf::from(env!("CARGO_BIN_EXE_frg"))
        .parent()
        .unwrap()
        .to_path_buf();
    let output = Command::new(env!("CARGO_BIN_EXE_frg"))
        .args([
            "plugin",
            "install",
            "--harness",
            "claude-code",
            "--plugin-root",
        ])
        .arg(&root)
        .env("HOME", &env.home)
        .env(
            "PATH",
            format!("{}:{}:/usr/bin:/bin", env.bin.display(), frg_dir.display()),
        )
        .env("FAKE_STATE", &env.state)
        .env("FAKE_VERSION", VERSION)
        .env("FAKE_ENABLED", "false")
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("disabled"));
    assert_eq!(env.settings(), before);
}

#[test]
fn a_marketplace_named_forge_elsewhere_is_left_alone() {
    let env = setup();
    std::fs::write(env.state.join("claude-mkt"), "/somewhere/else").unwrap();
    env.write_settings(&foreign_with_legacy_hooks());

    let output = env.install("claude-code");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("/somewhere/else"), "{stderr}");
    assert!(stderr.contains("will not replace it"), "{stderr}");
    let calls = env.calls();
    assert!(!calls.contains("marketplace add"), "{calls}");
    assert!(!calls.contains("plugin install"), "{calls}");
    assert_eq!(env.settings(), foreign_with_legacy_hooks());
}

#[test]
fn codex_install_uses_codex_and_leaves_claude_and_codex_config_alone() {
    let env = setup();
    env.write_settings(&foreign_with_legacy_hooks());
    let config =
        "model = \"o3\"\n\n[mcp_servers.forge]\ncommand = \"/x/frg\"\nargs = [\"--mcp\"]\n";
    std::fs::create_dir_all(env.home.join(".codex")).unwrap();
    std::fs::write(env.home.join(".codex/config.toml"), config).unwrap();

    let report = ok(&env.install("codex"));

    let calls = env.calls();
    assert!(calls.contains("codex plugin marketplace add "), "{calls}");
    assert!(
        calls.contains("codex plugin add forge@forge --json"),
        "{calls}"
    );
    assert!(!calls.contains("claude "), "{calls}");
    assert_eq!(report["verified"]["mcp_server_version"], VERSION);
    let left = report["migration"]["left"].as_array().unwrap();
    assert_eq!(left.len(), 1, "{left:?}");
    assert_eq!(left[0]["manual"], "codex mcp remove forge");
    assert_eq!(
        std::fs::read_to_string(env.home.join(".codex/config.toml")).unwrap(),
        config
    );
    assert_eq!(env.settings(), foreign_with_legacy_hooks());
}

// ── The plugin tree itself ──────────────────────────────────────────────────

fn read_json(rel: &str) -> Value {
    let path = repo_plugin_root().join(rel);
    serde_json::from_str(&std::fs::read_to_string(&path).unwrap())
        .unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

#[test]
fn every_manifest_carries_the_frg_version() {
    for rel in ["forge/.claude-plugin/plugin.json", "forge/plugin.json"] {
        assert_eq!(read_json(rel)["version"], VERSION, "{rel}");
    }
    let market = read_json(".claude-plugin/marketplace.json");
    assert_eq!(market["plugins"][0]["version"], VERSION);
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
