//! Resolve the CQL contact point for the task store.
//!
//! Precedence (highest first):
//! 1. explicit argument (the tool's `cql_host` / CLI `--cql-host`)
//! 2. `FORGE_CQL_HOST` environment variable
//! 3. `cql_host` in the nearest `.forge/config.toml` (walking up from the cwd)
//! 4. `cql_host` in the global `~/.config/forge.toml`
//! 5. the built-in [`DEFAULT_CQL_HOST`]
//!
//! The global layer exists because every other layer is tied to WHERE forge is
//! run from. An installer can configure a machine once -- which is what the
//! Ferrosa workbench needs: it provisions a database on a non-default port, and
//! before this the only ways to tell forge about it were an environment variable
//! the user had to export or a per-project file in every repo they own. `forge
//! task list` in a fresh directory failed with "connect to CQL ... Connection
//! refused" against a database that was running the whole time.
//!
//! The path matches the rest of the system: forge already reads its memory
//! client config from `~/.config/ferrosa-memory.toml`, so its own settings sit
//! beside it as `~/.config/forge.toml`, with the same schema as the project
//! file. Project still beats global, so a repo can pin its own board.
//!
//! Blank/whitespace values at any layer are ignored and fall through.

use std::path::Path;

use serde::Deserialize;

/// Built-in fallback when nothing else is configured.
pub const DEFAULT_CQL_HOST: &str = "127.0.0.1:9042";

#[derive(Debug, Default, Deserialize)]
struct ForgeConfig {
    cql_host: Option<String>,
    #[serde(default)]
    debug_stop: Option<bool>,
}

/// Trim a candidate, dropping it if blank.
fn non_blank(s: String) -> Option<String> {
    let t = s.trim();
    if t.is_empty() {
        None
    } else {
        Some(t.to_string())
    }
}

/// Pure precedence rule, separated from I/O for testing.
fn pick(
    explicit: Option<&str>,
    env: Option<String>,
    file: Option<String>,
    global: Option<String>,
) -> String {
    explicit
        .map(str::to_string)
        .and_then(non_blank)
        .or_else(|| env.and_then(non_blank))
        .or_else(|| file.and_then(non_blank))
        .or_else(|| global.and_then(non_blank))
        .unwrap_or_else(|| DEFAULT_CQL_HOST.to_string())
}

/// Parse `cql_host` out of a `.forge/config.toml` body. Pure; testable.
fn parse_cql_host_toml(body: &str) -> Option<String> {
    toml::from_str::<ForgeConfig>(body)
        .ok()
        .and_then(|c| c.cql_host)
}

/// Walk up from `start` looking for `.forge/config.toml`; return its `cql_host`.
fn read_config_cql_host(start: &Path) -> Option<String> {
    for dir in start.ancestors() {
        let candidate = dir.join(".forge").join("config.toml");
        if let Ok(body) = std::fs::read_to_string(&candidate) {
            return parse_cql_host_toml(&body);
        }
    }
    None
}

/// The machine-wide config file: `~/.config/forge.toml`.
///
/// Beside `~/.config/ferrosa-memory.toml`, which forge already reads, so a user
/// looking for "where do I configure this" finds both in one place.
pub fn global_config_path() -> Option<std::path::PathBuf> {
    dirs::home_dir().map(|home| home.join(".config").join("forge.toml"))
}

/// Read the global config body, if there is one.
fn read_global_config() -> Option<String> {
    let path = global_config_path()?;
    std::fs::read_to_string(path).ok()
}

/// Resolve the effective CQL `host:port` for the task store.
pub fn resolve_cql_host(explicit: Option<&str>) -> String {
    let env = std::env::var("FORGE_CQL_HOST").ok();
    let file = std::env::current_dir()
        .ok()
        .and_then(|cwd| read_config_cql_host(&cwd));
    let global = read_global_config().and_then(|body| parse_cql_host_toml(&body));
    pick(explicit, env, file, global)
}

/// Split a (possibly comma-separated) contact-point string into individual
/// `host:port` entries, dropping blanks. A single host yields a one-element vec.
fn split_hosts(s: &str) -> Vec<String> {
    s.split(',')
        .filter_map(|h| non_blank(h.to_string()))
        .collect()
}

/// Resolve the effective CQL contact points for the task store.
///
/// Any layer may supply a comma-separated list (e.g.
/// `cql_host = "n1:19042,n2:19042,n3:19042"`). Passing every node lets the driver
/// bootstrap from whichever is up and fail over for queries, so the board
/// survives a single node loss instead of dying with one fixed contact point.
/// Always returns at least one entry (the resolved value, or [`DEFAULT_CQL_HOST`]).
pub fn resolve_cql_hosts(explicit: Option<&str>) -> Vec<String> {
    let hosts = split_hosts(&resolve_cql_host(explicit));
    if hosts.is_empty() {
        vec![DEFAULT_CQL_HOST.to_string()]
    } else {
        hosts
    }
}

/// Parse `debug_stop` from a `.forge/config.toml` body. Pure; testable.
fn parse_debug_stop_toml(body: &str) -> Option<bool> {
    toml::from_str::<ForgeConfig>(body)
        .ok()
        .and_then(|c| c.debug_stop)
}

/// Walk up from `start` for `.forge/config.toml`; return its `debug_stop`.
fn read_config_debug_stop(start: &Path) -> Option<bool> {
    for dir in start.ancestors() {
        let candidate = dir.join(".forge").join("config.toml");
        if let Ok(body) = std::fs::read_to_string(&candidate) {
            return parse_debug_stop_toml(&body);
        }
    }
    None
}

/// Resolve whether `debug_stop` board alerting is on. Precedence: explicit tool
/// arg → `FORGE_DEBUG_STOP` (1/true/yes) → `.forge/config.toml` `debug_stop` →
/// `~/.config/forge.toml` `debug_stop` → false. The explicit arg lets the LLM flip it on per call when it suspects the
/// board is degraded.
pub fn resolve_debug_stop(explicit: Option<bool>) -> bool {
    if let Some(b) = explicit {
        return b;
    }
    if let Ok(raw) = std::env::var("FORGE_DEBUG_STOP") {
        match raw.trim().to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => return true,
            "0" | "false" | "no" | "off" => return false,
            _ => {}
        }
    }
    if let Some(project) = std::env::current_dir()
        .ok()
        .and_then(|cwd| read_config_debug_stop(&cwd))
    {
        return project;
    }
    // Same file, same schema, same precedence as cql_host -- a setting that
    // honoured the global config for one key and ignored it for the other would
    // be the more surprising design.
    read_global_config()
        .and_then(|body| parse_debug_stop_toml(&body))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_wins_over_everything() {
        assert_eq!(
            pick(Some("h:1"), Some("e:2".into()), Some("f:3".into()), Some("g:4".into())),
            "h:1"
        );
    }

    #[test]
    fn env_wins_when_no_explicit() {
        assert_eq!(
            pick(None, Some("e:2".into()), Some("f:3".into()), Some("g:4".into())),
            "e:2"
        );
    }

    #[test]
    fn file_used_when_no_explicit_or_env() {
        assert_eq!(
            pick(None, None, Some("f:3".into()), Some("g:4".into())),
            "f:3"
        );
    }

    #[test]
    fn default_when_nothing_set() {
        assert_eq!(pick(None, None, None, None), DEFAULT_CQL_HOST);
    }

    #[test]
    fn blank_values_are_ignored() {
        assert_eq!(
            pick(Some("   "), Some(String::new()), Some("f:3".into()), None),
            "f:3"
        );
        assert_eq!(
            pick(Some("  "), Some("  ".into()), None, None),
            DEFAULT_CQL_HOST
        );
        // A blank global falls through to the default rather than being taken
        // as a configured empty host.
        assert_eq!(pick(None, None, None, Some("   ".into())), DEFAULT_CQL_HOST);
    }

    #[test]
    fn global_config_is_used_when_nothing_else_is_set() {
        // The case that motivated this layer: an installer configured the
        // machine, and the user runs `forge task list` in a directory with no
        // .forge/config.toml and no exported variable. That previously reached
        // the built-in default and failed against a database that was running
        // the whole time.
        assert_eq!(
            pick(None, None, None, Some("127.0.0.1:47017".into())),
            "127.0.0.1:47017"
        );
    }

    #[test]
    fn a_project_config_still_beats_the_global_one() {
        // A repo pinning its own board must win over a machine-wide default, or
        // checking out a project would silently talk to the wrong database.
        assert_eq!(
            pick(None, None, Some("project:1".into()), Some("global:2".into())),
            "project:1"
        );
    }

    #[test]
    fn env_and_explicit_still_beat_the_global_one() {
        assert_eq!(
            pick(None, Some("env:1".into()), None, Some("global:2".into())),
            "env:1"
        );
        assert_eq!(
            pick(Some("explicit:1"), None, None, Some("global:2".into())),
            "explicit:1"
        );
    }

    #[test]
    fn the_global_path_sits_beside_the_memory_client_config() {
        // forge already reads ~/.config/ferrosa-memory.toml. Its own settings
        // belong next to that, not in a third place a user has to discover.
        let path = global_config_path().expect("home dir");
        assert!(
            path.ends_with(".config/forge.toml"),
            "expected ~/.config/forge.toml, got {}",
            path.display()
        );
    }

    #[test]
    fn the_global_file_uses_the_same_schema_as_the_project_file() {
        // One shape, two locations. A separate schema for the global file would
        // mean two formats to document and keep in step.
        let body = "cql_host = \"127.0.0.1:47017\"\ndebug_stop = true\n";
        assert_eq!(parse_cql_host_toml(body).as_deref(), Some("127.0.0.1:47017"));
        assert_eq!(parse_debug_stop_toml(body), Some(true));
    }

    #[test]
    fn split_hosts_single_and_list_and_blanks() {
        assert_eq!(split_hosts("h:1"), vec!["h:1"]);
        assert_eq!(
            split_hosts("n1:19042, n2:19042 ,n3:19042"),
            vec!["n1:19042", "n2:19042", "n3:19042"]
        );
        assert_eq!(split_hosts("a:1,,  ,b:2"), vec!["a:1", "b:2"]);
        assert!(split_hosts("   ").is_empty());
    }

    #[test]
    fn resolve_hosts_always_nonempty() {
        // A single explicit host yields one contact point; the list form yields many.
        assert_eq!(resolve_cql_hosts(Some("h:1")), vec!["h:1"]);
        assert_eq!(
            resolve_cql_hosts(Some("n1:19042,n2:19042,n3:19042")),
            vec!["n1:19042", "n2:19042", "n3:19042"]
        );
    }

    #[test]
    fn parses_debug_stop_and_explicit_wins() {
        assert_eq!(parse_debug_stop_toml("debug_stop = true\n"), Some(true));
        assert_eq!(parse_debug_stop_toml("debug_stop = false\n"), Some(false));
        assert_eq!(parse_debug_stop_toml("cql_host = \"h:1\"\n"), None);
        // explicit arg short-circuits env/file
        assert!(resolve_debug_stop(Some(true)));
        assert!(!resolve_debug_stop(Some(false)));
    }

    #[test]
    fn parses_cql_host_from_toml() {
        assert_eq!(
            parse_cql_host_toml("cql_host = \"127.0.0.1:19042\"\n").as_deref(),
            Some("127.0.0.1:19042")
        );
    }

    #[test]
    fn toml_without_cql_host_is_none() {
        assert_eq!(parse_cql_host_toml("other = 1\n"), None);
    }

    #[test]
    fn reads_config_walking_up_from_subdir() {
        let base = std::env::temp_dir().join(format!("forge_cfg_test_{}", std::process::id()));
        let sub = base.join("a").join("b");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::create_dir_all(base.join(".forge")).unwrap();
        std::fs::write(
            base.join(".forge").join("config.toml"),
            "cql_host = \"10.0.0.1:9999\"\n",
        )
        .unwrap();
        let got = read_config_cql_host(&sub);
        std::fs::remove_dir_all(&base).ok();
        assert_eq!(got.as_deref(), Some("10.0.0.1:9999"));
    }
}
