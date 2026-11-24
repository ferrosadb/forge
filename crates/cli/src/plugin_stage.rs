//! Stage the forge plugin into the shared `ferrosa` marketplace (B1).
//!
//! The marketplace root (default `~/.ferrosa/plugins/current/`) is shared with
//! fmem, which stages `ferrosa-memory/` beside our `forge/`. The two manifests
//! at the root are therefore shared files: forge's entry is merged in by exact
//! identity (added when absent, left alone when identical, refused when an
//! entry named `forge` differs), and every other entry and key stays as it
//! was. `forge/` itself is forge's own directory and is brought up to date
//! with the tree embedded in this frg, so the plugin's version is always this
//! frg's version.

use std::path::Path;

use anyhow::{bail, Context};
use serde_json::{json, Value};

use crate::harness_files;

pub const MARKETPLACE: &str = "ferrosa";
pub const PLUGIN: &str = "forge";

/// The plugin tree, embedded from `plugin/forge/` in this repository.
const TREE: [(&str, &str, bool); 6] = [
    (
        ".claude-plugin/plugin.json",
        include_str!("../../../plugin/forge/.claude-plugin/plugin.json"),
        false,
    ),
    (
        ".mcp.json",
        include_str!("../../../plugin/forge/.mcp.json"),
        false,
    ),
    (
        "hooks/hooks.json",
        include_str!("../../../plugin/forge/hooks/hooks.json"),
        false,
    ),
    (
        "plugin.json",
        include_str!("../../../plugin/forge/plugin.json"),
        false,
    ),
    (
        "mcp.json",
        include_str!("../../../plugin/forge/mcp.json"),
        false,
    ),
    (
        "scripts/frg-mcp",
        include_str!("../../../plugin/forge/scripts/frg-mcp"),
        true,
    ),
];

const DESCRIPTION: &str =
    "frg as an MCP server: structured, token-bounded test, build, lint and log summaries, code analysis, and the Forge task board.";

/// A marketplace manifest the root carries, and forge's entry in it.
pub struct Manifest {
    pub rel: &'static str,
    skeleton: fn() -> Value,
    entry: fn() -> Value,
}

/// Claude Code's manifest, then Codex's. The skeletons match the ones fmem
/// stages, so whichever tool creates the file first, the other merges in.
pub const MANIFESTS: [Manifest; 2] = [
    Manifest {
        rel: ".claude-plugin/marketplace.json",
        skeleton: || {
            json!({
                "name": MARKETPLACE,
                "description": "The Ferrosa plugins installed on this machine, at the version of the binaries beside them.",
                "owner": {"name": "Ferrosa", "url": "https://www.ferrosa.ai"},
                "plugins": []
            })
        },
        entry: || {
            json!({
                "name": PLUGIN,
                "source": "./forge",
                "description": DESCRIPTION,
                "category": "development"
            })
        },
    },
    Manifest {
        rel: ".agents/plugins/marketplace.json",
        skeleton: || {
            json!({
                "name": MARKETPLACE,
                "interface": {"displayName": "Ferrosa (installed on this machine)"},
                "plugins": []
            })
        },
        entry: || {
            json!({
                "name": PLUGIN,
                "source": {"source": "local", "path": "./forge"},
                "policy": {"installation": "AVAILABLE", "authentication": "ON_INSTALL"},
                "category": "Developer Tools"
            })
        },
    },
];

/// What staging did, for the report's `steps`.
pub struct Staged {
    pub steps: Vec<String>,
}

/// Merge forge's entry into one manifest value. Returns whether it changed.
pub fn merge_entry(manifest: &mut Value, entry: &Value, file: &str) -> anyhow::Result<bool> {
    if manifest["name"] != MARKETPLACE {
        bail!(
            "{file} is the marketplace {}, not {MARKETPLACE}; forge will not add itself to it",
            manifest["name"]
        );
    }
    let plugins = manifest
        .get_mut("plugins")
        .and_then(Value::as_array_mut)
        .with_context(|| format!("{file} has no plugins list"))?;
    let named: Vec<&Value> = plugins.iter().filter(|p| p["name"] == PLUGIN).collect();
    match named.as_slice() {
        [] => {
            plugins.push(entry.clone());
            Ok(true)
        }
        [only] if *only == entry => Ok(false),
        _ => bail!(
            "{file} already lists a plugin named {PLUGIN} that differs from this frg's entry; \
             forge will not overwrite it. Compare it with {entry} and fix it by hand."
        ),
    }
}

fn stage_manifest(root: &Path, manifest: &Manifest) -> anyhow::Result<Option<String>> {
    let path = root.join(manifest.rel);
    let file = path.display().to_string();
    let existing = match std::fs::read_to_string(&path) {
        Ok(text) => Some(text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(e).with_context(|| format!("cannot read {file}")),
    };
    let mut value = match &existing {
        Some(text) => serde_json::from_str(text)
            .with_context(|| format!("{file} is not valid JSON; nothing was written"))?,
        None => (manifest.skeleton)(),
    };
    let changed = merge_entry(&mut value, &(manifest.entry)(), &file)?;
    if !changed && existing.is_some() {
        return Ok(None);
    }
    let text = harness_files::json_text(&value, existing.as_deref())?;
    harness_files::write_atomic(&path, &text)?;
    Ok(Some(format!("added {PLUGIN} to {file}")))
}

fn stage_tree(root: &Path) -> anyhow::Result<Vec<String>> {
    let mut steps = Vec::new();
    for (rel, body, executable) in TREE {
        let path = root.join(PLUGIN).join(rel);
        if std::fs::read_to_string(&path).is_ok_and(|have| have == body) {
            continue;
        }
        harness_files::write_atomic(&path, body)?;
        if executable {
            set_executable(&path)?;
        }
        steps.push(format!("wrote {}", path.display()));
    }
    Ok(steps)
}

#[cfg(unix)]
fn set_executable(path: &Path) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
        .with_context(|| format!("cannot make {} executable", path.display()))
}

#[cfg(not(unix))]
fn set_executable(_path: &Path) -> anyhow::Result<()> {
    Ok(())
}

/// Bring `root` up to date: forge's tree, and forge's entry in both
/// manifests. Every manifest is checked before anything is written, so a
/// refusal leaves the root as it was.
pub fn stage(root: &Path) -> anyhow::Result<Staged> {
    for manifest in &MANIFESTS {
        let path = root.join(manifest.rel);
        if let Ok(text) = std::fs::read_to_string(&path) {
            let mut value: Value = serde_json::from_str(&text).with_context(|| {
                format!("{} is not valid JSON; nothing was written", path.display())
            })?;
            merge_entry(&mut value, &(manifest.entry)(), &path.display().to_string())?;
        }
    }
    let mut steps = stage_tree(root)?;
    for manifest in &MANIFESTS {
        steps.extend(stage_manifest(root, manifest)?);
    }
    Ok(Staged { steps })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_embedded_plugin_carries_this_frgs_version() {
        for (rel, body, _) in TREE {
            if rel.ends_with("plugin.json") {
                let v: Value = serde_json::from_str(body).unwrap();
                assert_eq!(v["version"], env!("CARGO_PKG_VERSION"), "{rel}");
            }
        }
    }

    #[test]
    fn merge_adds_once_keeps_others_and_refuses_a_different_forge() {
        let memory = json!({"name": "ferrosa-memory", "source": "./ferrosa-memory"});
        let entry = (MANIFESTS[0].entry)();
        let mut m = json!({"name": "ferrosa", "plugins": [memory.clone()]});
        assert!(merge_entry(&mut m, &entry, "m").unwrap());
        assert!(!merge_entry(&mut m, &entry, "m").unwrap());
        assert_eq!(m["plugins"], json!([memory, entry.clone()]));

        let mut other =
            json!({"name": "ferrosa", "plugins": [{"name": "forge", "source": "./elsewhere"}]});
        let before = other.clone();
        assert!(merge_entry(&mut other, &entry, "m").is_err());
        assert_eq!(other, before);

        let mut foreign = json!({"name": "someone-else", "plugins": []});
        assert!(merge_entry(&mut foreign, &entry, "m").is_err());
    }
}
