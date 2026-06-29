//! Resolve the CQL contact point for the task store.
//!
//! Precedence (highest first):
//! 1. explicit argument (the tool's `cql_host` / CLI `--cql-host`)
//! 2. `FORGE_CQL_HOST` environment variable
//! 3. `cql_host` in the nearest `.forge/config.toml` (walking up from the cwd)
//! 4. the built-in [`DEFAULT_CQL_HOST`]
//!
//! Blank/whitespace values at any layer are ignored and fall through.

use std::path::Path;

use serde::Deserialize;

/// Built-in fallback when nothing else is configured.
pub const DEFAULT_CQL_HOST: &str = "127.0.0.1:9042";

#[derive(Debug, Default, Deserialize)]
struct ForgeConfig {
    cql_host: Option<String>,
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
fn pick(explicit: Option<&str>, env: Option<String>, file: Option<String>) -> String {
    explicit
        .map(str::to_string)
        .and_then(non_blank)
        .or_else(|| env.and_then(non_blank))
        .or_else(|| file.and_then(non_blank))
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

/// Resolve the effective CQL `host:port` for the task store.
pub fn resolve_cql_host(explicit: Option<&str>) -> String {
    let env = std::env::var("FORGE_CQL_HOST").ok();
    let file = std::env::current_dir()
        .ok()
        .and_then(|cwd| read_config_cql_host(&cwd));
    pick(explicit, env, file)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_wins_over_everything() {
        assert_eq!(
            pick(Some("h:1"), Some("e:2".into()), Some("f:3".into())),
            "h:1"
        );
    }

    #[test]
    fn env_wins_when_no_explicit() {
        assert_eq!(pick(None, Some("e:2".into()), Some("f:3".into())), "e:2");
    }

    #[test]
    fn file_used_when_no_explicit_or_env() {
        assert_eq!(pick(None, None, Some("f:3".into())), "f:3");
    }

    #[test]
    fn default_when_nothing_set() {
        assert_eq!(pick(None, None, None), DEFAULT_CQL_HOST);
    }

    #[test]
    fn blank_values_are_ignored() {
        assert_eq!(
            pick(Some("   "), Some(String::new()), Some("f:3".into())),
            "f:3"
        );
        assert_eq!(pick(Some("  "), Some("  ".into()), None), DEFAULT_CQL_HOST);
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
