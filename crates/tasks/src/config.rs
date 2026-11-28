//! Resolve the CQL contact point and login for the task store.
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
//!
//! Every layer fails loud. A config file that exists but cannot be read or
//! parsed is an ERROR, not a silent fall-through to the next layer: falling
//! through sends the board query to `127.0.0.1:9042` -- a different database, or
//! none at all -- while the user is looking at a file that says otherwise. The
//! symptom is an empty or wrong board with no indication that the config they
//! wrote was never applied. A file that is simply absent is not an error; that
//! is what "not configured at this layer" means.
//!
//! # The board credential
//!
//! The CQL login resolves through the SAME layers, from the same files:
//!
//! 1. `FORGE_CQL_USERNAME` + `FORGE_CQL_PASSWORD_FILE` in the environment
//! 2. `cql_username` + `cql_password_file` in the nearest `.forge/config.toml`
//! 3. `cql_username` + `cql_password_file` in `~/.config/forge.toml`
//! 4. the built-in development credential, [`DEFAULT_CQL_USERNAME`] with the
//!    password Ferrosa seeds for it in `FERROSA_MODE=development`
//!
//! ```toml
//! cql_host = "127.0.0.1:9042"
//! cql_username = "ferrosa_admin"
//! cql_password_file = "/Users/me/.ferrosa/config/cql-admin.secret"
//! ```
//!
//! `cql_password_file` is a PATH to a file holding only the password (a trailing
//! newline is trimmed); a relative path is resolved against the directory of the
//! config file that names it (against the cwd for the environment variable). The
//! file should be mode 0600: a file readable by group or other is used but warned
//! about. A file that is missing, unreadable or empty is an ERROR naming the file
//! -- falling back to the development login would send a credential the operator
//! did not configure. Username and password file come as a pair: a layer that
//! sets one without the other is an error.
//!
//! There is deliberately no key for the password itself. A config file is read by
//! every tool in the repo and is easily committed; `cql_password` is refused,
//! loudly, rather than ignored.
//!
//! Before this, the board login was hardcoded to `ferrosa_admin`/`ferrosa_admin`
//! (t_4e5fdc3c). On a node installed with a generated admin password every
//! `frg task ...` failed with `Authentication failed - bad credentials` and there
//! was no setting to fix it (t_84fde1ee). The built-in fallback is kept so a
//! development install keeps working unchanged, and every failure says when it
//! was the fallback that was used -- see [`BoardTarget::describe`].

use std::fmt;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;

/// Built-in fallback when nothing else is configured.
pub const DEFAULT_CQL_HOST: &str = "127.0.0.1:9042";

/// The development role used when no board credential is configured.
pub const DEFAULT_CQL_USERNAME: &str = "ferrosa_admin";

/// The password Ferrosa seeds for [`DEFAULT_CQL_USERNAME`] in development mode.
/// It is published (Ferrosa's own docs and logs print it), so it is no secret;
/// it exists here only so an unconfigured development install keeps working.
const DEFAULT_CQL_PASSWORD: &str = "ferrosa_admin"; // pragma: allowlist secret

/// The environment variables that supply the board credential, as a pair.
const ENV_USERNAME: &str = "FORGE_CQL_USERNAME";
const ENV_PASSWORD_FILE: &str = "FORGE_CQL_PASSWORD_FILE";

#[derive(Debug, Default, Deserialize)]
struct ForgeConfig {
    cql_host: Option<String>,
    #[serde(default)]
    debug_stop: Option<bool>,
    #[serde(default)]
    cql_username: Option<String>,
    #[serde(default)]
    cql_password_file: Option<String>,
    /// Never accepted; present only so a config that tries is refused by name
    /// instead of being silently ignored. Its value is never read or printed.
    #[serde(default)]
    cql_password: Option<toml::Value>,
}

/// Where a board setting came from. Every connect or login failure names it,
/// so the operator can tell which of the layers above was actually applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SettingSource {
    /// Passed by the caller: `--cql-host`, a tool's `cql_host` argument, or
    /// contact points handed straight to `TaskStore::connect`.
    Explicit,
    /// An environment variable.
    Env(&'static str),
    /// The nearest `.forge/config.toml`.
    ProjectFile(PathBuf),
    /// `~/.config/forge.toml`.
    GlobalFile(PathBuf),
    /// Nothing was configured; the built-in default was used.
    BuiltIn,
}

impl fmt::Display for SettingSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Explicit => f.write_str("the --cql-host flag / cql_host argument"),
            Self::Env(name) => write!(f, "env {name}"),
            Self::ProjectFile(path) | Self::GlobalFile(path) => write!(f, "{}", path.display()),
            Self::BuiltIn => f.write_str("built-in default"),
        }
    }
}

/// The login the task board connects with.
#[derive(Clone)]
pub struct BoardCredential {
    username: String,
    password: String,
    source: SettingSource,
}

impl fmt::Debug for BoardCredential {
    // The password is never formatted, not even in a debug dump.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BoardCredential")
            .field("username", &self.username)
            .field("password", &"<redacted>")
            .field("source", &self.source)
            .finish()
    }
}

impl BoardCredential {
    pub fn username(&self) -> &str {
        &self.username
    }

    pub fn source(&self) -> &SettingSource {
        &self.source
    }

    pub(crate) fn password(&self) -> &str {
        &self.password
    }

    fn built_in() -> Self {
        Self {
            username: DEFAULT_CQL_USERNAME.to_string(),
            password: DEFAULT_CQL_PASSWORD.to_string(),
            source: SettingSource::BuiltIn,
        }
    }

    /// The credential's provenance, for error messages.
    fn describe_source(&self) -> String {
        match &self.source {
            SettingSource::BuiltIn => format!(
                "the built-in development credential {DEFAULT_CQL_USERNAME} (no \
                 {ENV_USERNAME}/{ENV_PASSWORD_FILE} or cql_username/cql_password_file configured)"
            ),
            SettingSource::Env(_) => format!("env {ENV_USERNAME} + {ENV_PASSWORD_FILE}"),
            other => other.to_string(),
        }
    }
}

/// Everything the task board connects with, and where each part came from.
#[derive(Debug, Clone)]
pub struct BoardTarget {
    pub hosts: Vec<String>,
    pub host_source: SettingSource,
    pub credential: BoardCredential,
}

impl BoardTarget {
    /// Which configuration supplied the host and the credential, and the role.
    /// Appended to every connect and login failure. Never contains a password.
    pub fn describe(&self) -> String {
        if self.host_source == SettingSource::BuiltIn
            && self.credential.source == SettingSource::BuiltIn
        {
            return format!(
                "no config found, used built-in defaults: host {}, built-in development \
                 credential {DEFAULT_CQL_USERNAME}",
                self.hosts.join(", ")
            );
        }
        format!(
            "host {} from {}; role {} from {}",
            self.hosts.join(", "),
            self.host_source,
            self.credential.username,
            self.credential.describe_source()
        )
    }
}

/// One layer's credential declaration, before its password file is read.
#[derive(Debug, Clone, PartialEq, Eq)]
struct CredentialDecl {
    username: String,
    password_file: PathBuf,
    source: SettingSource,
}

/// A layer's `(username, password_file)` as a declaration: `None` when the
/// layer sets neither, an error when it sets only one. Blank counts as unset.
fn credential_decl(
    username: Option<String>,
    password_file: Option<String>,
    source: SettingSource,
    base_dir: Option<&Path>,
) -> Result<Option<CredentialDecl>> {
    let (user_key, file_key) = match source {
        SettingSource::Env(_) => (ENV_USERNAME, ENV_PASSWORD_FILE),
        _ => ("cql_username", "cql_password_file"),
    };
    match (
        username.and_then(non_blank),
        password_file.and_then(non_blank),
    ) {
        (None, None) => Ok(None),
        (Some(_), None) => anyhow::bail!(
            "{source}: {user_key} is set without {file_key}; set both, or neither to use the \
             built-in development credential"
        ),
        (None, Some(_)) => anyhow::bail!(
            "{source}: {file_key} is set without {user_key}; set both, or neither to use the \
             built-in development credential"
        ),
        (Some(username), Some(file)) => {
            let file = PathBuf::from(file);
            let password_file = match base_dir {
                Some(dir) if file.is_relative() => dir.join(file),
                _ => file,
            };
            Ok(Some(CredentialDecl {
                username,
                password_file,
                source,
            }))
        }
    }
}

/// Read a password file: the whole file minus its trailing newline. Missing,
/// unreadable or empty is an error naming the file and the setting that named it.
fn read_password_file(decl: &CredentialDecl) -> Result<String> {
    let path = &decl.password_file;
    let body = std::fs::read_to_string(path).with_context(|| {
        format!(
            "cannot read the CQL password file {} (named by {})",
            path.display(),
            decl.source
        )
    })?;
    let password = body.trim_end_matches(['\n', '\r']);
    anyhow::ensure!(
        !password.trim().is_empty(),
        "the CQL password file {} (named by {}) is empty",
        path.display(),
        decl.source
    );
    warn_if_readable_by_others(path);
    Ok(password.to_string())
}

/// Warn, by name, when a password file is readable by group or other. A warning
/// rather than an error: the operator's credential still works, and refusing it
/// would break a running install over a permission bit.
#[cfg(unix)]
fn warn_if_readable_by_others(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    match std::fs::metadata(path) {
        Ok(meta) if meta.permissions().mode() & 0o077 != 0 => eprintln!(
            "forge: warning: CQL password file {} is readable by group or other (mode {:o}); \
             run `chmod 600 {}`",
            path.display(),
            meta.permissions().mode() & 0o777,
            path.display()
        ),
        Ok(_) => {}
        Err(e) => eprintln!(
            "forge: warning: could not check the permissions of CQL password file {}: {e}",
            path.display()
        ),
    }
}

#[cfg(not(unix))]
fn warn_if_readable_by_others(_path: &Path) {}

/// A parsed config file and where it lives.
type Layer = Option<(PathBuf, ForgeConfig)>;

/// Parse a config layer, refusing an inline password by name. The refusal
/// never echoes the value.
fn parse_layer(path: PathBuf, body: &str) -> Result<(PathBuf, ForgeConfig)> {
    let config = parse_config_toml(body).with_context(|| format!("in {}", path.display()))?;
    anyhow::ensure!(
        config.cql_password.is_none(),
        "{}: `cql_password` is not accepted -- a password must not live in a config file. \
         Put it in a file only you can read (chmod 600) and set `cql_password_file` to its path",
        path.display()
    );
    Ok((path, config))
}

/// The project and global config layers, parsed.
fn load_layers() -> Result<(Layer, Layer)> {
    let cwd = std::env::current_dir()
        .context("resolve the project forge config: read current directory")?;
    let project = match find_project_config(&cwd)? {
        Some((path, body)) => Some(parse_layer(path, &body)?),
        None => None,
    };
    let global = match read_global_config()? {
        Some((path, body)) => Some(parse_layer(path, &body)?),
        None => None,
    };
    Ok((project, global))
}

/// Pure credential precedence: env pair > project > global > `None` (built-in).
fn pick_credential(
    env_username: Option<String>,
    env_password_file: Option<String>,
    project: &Layer,
    global: &Layer,
) -> Result<Option<CredentialDecl>> {
    if let Some(decl) = credential_decl(
        env_username,
        env_password_file,
        SettingSource::Env(ENV_PASSWORD_FILE),
        None,
    )? {
        return Ok(Some(decl));
    }
    for (layer, wrap) in [
        (
            project,
            SettingSource::ProjectFile as fn(PathBuf) -> SettingSource,
        ),
        (
            global,
            SettingSource::GlobalFile as fn(PathBuf) -> SettingSource,
        ),
    ] {
        let Some((path, config)) = layer else {
            continue;
        };
        if let Some(decl) = credential_decl(
            config.cql_username.clone(),
            config.cql_password_file.clone(),
            wrap(path.clone()),
            path.parent(),
        )? {
            return Ok(Some(decl));
        }
    }
    Ok(None)
}

/// Resolve the board credential from the environment and config layers.
fn resolve_credential(project: &Layer, global: &Layer) -> Result<BoardCredential> {
    let decl = pick_credential(
        env_var(ENV_USERNAME)?,
        env_var(ENV_PASSWORD_FILE)?,
        project,
        global,
    )?;
    match decl {
        None => Ok(BoardCredential::built_in()),
        Some(decl) => Ok(BoardCredential {
            password: read_password_file(&decl)?,
            username: decl.username,
            source: decl.source,
        }),
    }
}

/// Resolve everything the task board connects with: hosts and credential, each
/// with its source. `explicit_host` is the flag / tool argument, if any.
pub fn resolve_board_target(explicit_host: Option<&str>) -> Result<BoardTarget> {
    let (project, global) = load_layers()?;
    let (host, host_source) = pick_sourced(
        explicit_host,
        env_var("FORGE_CQL_HOST")?,
        layer_value(&project, |c| c.cql_host.clone()),
        layer_value(&global, |c| c.cql_host.clone()),
    );
    let mut hosts = split_hosts(&host);
    if hosts.is_empty() {
        hosts = vec![DEFAULT_CQL_HOST.to_string()];
    }
    Ok(BoardTarget {
        hosts,
        host_source: match host_source {
            HostLayer::Explicit => SettingSource::Explicit,
            HostLayer::Env => SettingSource::Env("FORGE_CQL_HOST"),
            HostLayer::Project => project_path(&project),
            HostLayer::Global => global_path(&global),
            HostLayer::BuiltIn => SettingSource::BuiltIn,
        },
        credential: resolve_credential(&project, &global)?,
    })
}

/// A board target for contact points the caller already chose: the hosts are
/// taken as given, the credential resolves through the usual layers.
pub fn resolve_board_target_for_hosts(hosts: &[String]) -> Result<BoardTarget> {
    let (project, global) = load_layers()?;
    Ok(BoardTarget {
        hosts: hosts.to_vec(),
        host_source: SettingSource::Explicit,
        credential: resolve_credential(&project, &global)?,
    })
}

fn project_path(layer: &Layer) -> SettingSource {
    match layer {
        Some((path, _)) => SettingSource::ProjectFile(path.clone()),
        None => SettingSource::BuiltIn,
    }
}

fn global_path(layer: &Layer) -> SettingSource {
    match layer {
        Some((path, _)) => SettingSource::GlobalFile(path.clone()),
        None => SettingSource::BuiltIn,
    }
}

fn layer_value(layer: &Layer, get: impl Fn(&ForgeConfig) -> Option<String>) -> Option<String> {
    layer.as_ref().and_then(|(_, config)| get(config))
}

/// Which host layer won.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HostLayer {
    Explicit,
    Env,
    Project,
    Global,
    BuiltIn,
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

/// Pure precedence rule, separated from I/O for testing: the host and the
/// layer that supplied it.
fn pick_sourced(
    explicit: Option<&str>,
    env: Option<String>,
    file: Option<String>,
    global: Option<String>,
) -> (String, HostLayer) {
    explicit
        .map(str::to_string)
        .and_then(non_blank)
        .map(|h| (h, HostLayer::Explicit))
        .or_else(|| env.and_then(non_blank).map(|h| (h, HostLayer::Env)))
        .or_else(|| file.and_then(non_blank).map(|h| (h, HostLayer::Project)))
        .or_else(|| global.and_then(non_blank).map(|h| (h, HostLayer::Global)))
        .unwrap_or_else(|| (DEFAULT_CQL_HOST.to_string(), HostLayer::BuiltIn))
}

/// Parse a config body. A syntax error is reported, never swallowed.
fn parse_config_toml(body: &str) -> Result<ForgeConfig> {
    toml::from_str::<ForgeConfig>(body).context("invalid TOML")
}

/// Parse `cql_host` out of a `.forge/config.toml` body. Pure; testable.
#[cfg(test)]
fn parse_cql_host_toml(body: &str) -> Result<Option<String>> {
    Ok(parse_config_toml(body)?.cql_host)
}

/// Read a config file. `Ok(None)` when it does not exist; `Err` when it exists
/// and cannot be read (permissions, a directory in its place, an I/O fault) --
/// those mean the user's settings were not applied, which they need to know.
fn read_config_file(path: &Path) -> Result<Option<String>> {
    match std::fs::read_to_string(path) {
        Ok(body) => Ok(Some(body)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("read forge config {}", path.display())),
    }
}

/// Walk up from `start` looking for `.forge/config.toml`; return its `cql_host`.
#[cfg(test)]
fn read_config_cql_host(start: &Path) -> Result<Option<String>> {
    match find_project_config(start)? {
        Some((path, body)) => {
            parse_cql_host_toml(&body).with_context(|| format!("in {}", path.display()))
        }
        None => Ok(None),
    }
}

/// The nearest `.forge/config.toml` walking up from `start`, with its body.
fn find_project_config(start: &Path) -> Result<Option<(std::path::PathBuf, String)>> {
    for dir in start.ancestors() {
        let candidate = dir.join(".forge").join("config.toml");
        if let Some(body) = read_config_file(&candidate)? {
            return Ok(Some((candidate, body)));
        }
    }
    Ok(None)
}

/// The machine-wide config file: `~/.config/forge.toml`.
///
/// Beside `~/.config/ferrosa-memory.toml`, which forge already reads, so a user
/// looking for "where do I configure this" finds both in one place.
pub fn global_config_path() -> Option<std::path::PathBuf> {
    dirs::home_dir().map(|home| home.join(".config").join("forge.toml"))
}

/// Read the global config body, if there is one.
fn read_global_config() -> Result<Option<(std::path::PathBuf, String)>> {
    let Some(path) = global_config_path() else {
        return Ok(None);
    };
    Ok(read_config_file(&path)?.map(|body| (path, body)))
}

/// Read an environment variable. Absent is `Ok(None)`; present but not UTF-8
/// is an error, because the user set it and it is not being honoured.
fn env_var(name: &str) -> Result<Option<String>> {
    match std::env::var(name) {
        Ok(v) => Ok(Some(v)),
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(e) => Err(e).with_context(|| format!("read {name}")),
    }
}

/// Resolve the effective CQL `host:port` for the task store.
///
/// A cwd that cannot be read is an error, not "no project config": it is a
/// question we failed to ask, and the answer decides which database is queried.
pub fn resolve_cql_host(explicit: Option<&str>) -> Result<String> {
    let (project, global) = load_layers()?;
    let (host, _layer) = pick_sourced(
        explicit,
        env_var("FORGE_CQL_HOST")?,
        layer_value(&project, |c| c.cql_host.clone()),
        layer_value(&global, |c| c.cql_host.clone()),
    );
    Ok(host)
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
pub fn resolve_cql_hosts(explicit: Option<&str>) -> Result<Vec<String>> {
    let hosts = split_hosts(&resolve_cql_host(explicit)?);
    Ok(if hosts.is_empty() {
        vec![DEFAULT_CQL_HOST.to_string()]
    } else {
        hosts
    })
}

/// Parse the `FORGE_DEBUG_STOP` value. Pure; testable without touching the
/// process environment.
///
/// An unrecognised value is an error. Falling through to `false` left the
/// degraded-board alerting the operator had just switched on silently OFF --
/// the state they were trying to leave.
fn parse_debug_stop_env(raw: &str) -> Result<bool> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Ok(true),
        "0" | "false" | "no" | "off" => Ok(false),
        other => anyhow::bail!(
            "FORGE_DEBUG_STOP={other:?} is not a boolean (use 1/true/yes/on or 0/false/no/off)"
        ),
    }
}

/// Parse `debug_stop` from a `.forge/config.toml` body. Pure; testable.
fn parse_debug_stop_toml(body: &str) -> Result<Option<bool>> {
    Ok(parse_config_toml(body)?.debug_stop)
}

/// Walk up from `start` for `.forge/config.toml`; return its `debug_stop`.
fn read_config_debug_stop(start: &Path) -> Result<Option<bool>> {
    match find_project_config(start)? {
        Some((path, body)) => {
            parse_debug_stop_toml(&body).with_context(|| format!("in {}", path.display()))
        }
        None => Ok(None),
    }
}

/// Resolve whether `debug_stop` board alerting is on. Precedence: explicit tool
/// arg → `FORGE_DEBUG_STOP` (1/true/yes) → `.forge/config.toml` `debug_stop` →
/// `~/.config/forge.toml` `debug_stop` → false. The explicit arg lets the LLM flip it on per call when it suspects the
/// board is degraded.
pub fn resolve_debug_stop(explicit: Option<bool>) -> Result<bool> {
    if let Some(b) = explicit {
        return Ok(b);
    }
    if let Some(raw) = env_var("FORGE_DEBUG_STOP")? {
        return parse_debug_stop_env(&raw);
    }
    let cwd = std::env::current_dir()
        .context("resolve the project forge config: read current directory")?;
    if let Some(project) = read_config_debug_stop(&cwd)? {
        return Ok(project);
    }
    // Same file, same schema, same precedence as cql_host -- a setting that
    // honoured the global config for one key and ignored it for the other would
    // be the more surprising design.
    match read_global_config()? {
        Some((path, body)) => Ok(parse_debug_stop_toml(&body)
            .with_context(|| format!("in {}", path.display()))?
            .unwrap_or(false)),
        None => Ok(false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The host alone, for the precedence cases written before sources existed.
    fn pick(
        explicit: Option<&str>,
        env: Option<String>,
        file: Option<String>,
        global: Option<String>,
    ) -> String {
        pick_sourced(explicit, env, file, global).0
    }

    fn layer(path: &str, body: &str) -> Layer {
        Some(parse_layer(PathBuf::from(path), body).expect("valid layer"))
    }

    #[test]
    fn each_host_layer_is_reported_as_its_source() {
        let s = |e: Option<&str>, v: Option<&str>, f: Option<&str>, g: Option<&str>| {
            pick_sourced(e, v.map(Into::into), f.map(Into::into), g.map(Into::into)).1
        };
        assert_eq!(s(Some("h:1"), Some("e:2"), None, None), HostLayer::Explicit);
        assert_eq!(s(None, Some("e:2"), Some("f:3"), None), HostLayer::Env);
        assert_eq!(s(None, None, Some("f:3"), Some("g:4")), HostLayer::Project);
        assert_eq!(s(None, None, None, Some("g:4")), HostLayer::Global);
        assert_eq!(s(None, Some("  "), None, None), HostLayer::BuiltIn);
    }

    #[test]
    fn credentials_resolve_env_then_project_then_global_then_built_in() {
        let project = layer(
            "/repo/.forge/config.toml",
            "cql_username = \"project_role\"\ncql_password_file = \"/p.secret\"\n",
        );
        let global = layer(
            "/home/u/.config/forge.toml",
            "cql_username = \"global_role\"\ncql_password_file = \"/g.secret\"\n",
        );
        let env = pick_credential(
            Some("env_role".into()),
            Some("/e.secret".into()),
            &project,
            &global,
        )
        .unwrap()
        .unwrap();
        assert_eq!(env.username, "env_role");
        assert_eq!(env.source, SettingSource::Env(ENV_PASSWORD_FILE));

        let from_project = pick_credential(None, None, &project, &global)
            .unwrap()
            .unwrap();
        assert_eq!(from_project.username, "project_role");
        assert_eq!(
            from_project.source,
            SettingSource::ProjectFile("/repo/.forge/config.toml".into())
        );

        let from_global = pick_credential(None, None, &None, &global)
            .unwrap()
            .unwrap();
        assert_eq!(from_global.username, "global_role");
        assert_eq!(from_global.password_file, PathBuf::from("/g.secret"));

        assert_eq!(pick_credential(None, None, &None, &None).unwrap(), None);
    }

    #[test]
    fn a_layer_with_only_half_the_credential_is_an_error_naming_it() {
        let half = layer("/home/u/.config/forge.toml", "cql_username = \"r\"\n");
        let err = pick_credential(None, None, &None, &half).expect_err("half a pair");
        let text = format!("{err:#}");
        assert!(
            text.contains("/home/u/.config/forge.toml") && text.contains("cql_password_file"),
            "{text}"
        );
        let err = pick_credential(None, Some("/e.secret".into()), &None, &None)
            .expect_err("env half a pair");
        assert!(format!("{err:#}").contains(ENV_USERNAME), "{err:#}");
    }

    #[test]
    fn a_relative_password_file_resolves_against_its_config_directory() {
        let global = layer(
            "/home/u/.config/forge.toml",
            "cql_username = \"r\"\ncql_password_file = \"board.secret\"\n",
        );
        let decl = pick_credential(None, None, &None, &global)
            .unwrap()
            .unwrap();
        assert_eq!(
            decl.password_file,
            PathBuf::from("/home/u/.config/board.secret")
        );
    }

    #[test]
    fn an_inline_password_is_refused_by_name_without_echoing_it() {
        let err = parse_layer(
            PathBuf::from("/home/u/.config/forge.toml"),
            "cql_password = \"fixture-inline\"\n",
        )
        .expect_err("inline password");
        let text = format!("{err:#}");
        assert!(text.contains("cql_password_file"), "{text}");
        assert!(!text.contains("fixture-inline"), "{text}");
    }

    #[test]
    fn a_password_file_is_trimmed_of_its_newline_and_must_not_be_empty() {
        let dir = tempfile_dir("pwfile");
        let decl = |name: &str| CredentialDecl {
            username: "r".into(),
            password_file: dir.join(name),
            source: SettingSource::GlobalFile("/home/u/.config/forge.toml".into()),
        };
        std::fs::write(dir.join("ok"), "pw-fixture\r\n").unwrap();
        std::fs::write(dir.join("empty"), "\n").unwrap();
        let ok = read_password_file(&decl("ok"));
        let empty = read_password_file(&decl("empty"));
        let missing = read_password_file(&decl("missing"));
        std::fs::remove_dir_all(&dir).ok();
        assert_eq!(ok.unwrap(), "pw-fixture");
        let empty = format!("{:#}", empty.expect_err("empty file"));
        assert!(
            empty.contains("empty") && empty.contains("forge.toml"),
            "{empty}"
        );
        let missing = format!("{:#}", missing.expect_err("missing file"));
        assert!(
            missing.contains("missing") && missing.contains("cannot read"),
            "{missing}"
        );
    }

    #[test]
    fn describe_names_both_sources_and_the_role_but_never_the_password() {
        let target = BoardTarget {
            hosts: vec!["10.0.0.1:9042".into()],
            host_source: SettingSource::GlobalFile("/home/u/.config/forge.toml".into()),
            credential: BoardCredential {
                username: "board_writer".into(),
                password: "pw-fixture".into(),
                source: SettingSource::Env(ENV_PASSWORD_FILE),
            },
        };
        let text = target.describe();
        assert_eq!(
            text,
            "host 10.0.0.1:9042 from /home/u/.config/forge.toml; role board_writer from env \
             FORGE_CQL_USERNAME + FORGE_CQL_PASSWORD_FILE"
        );
        assert!(!format!("{target:?}").contains("pw-fixture"));
    }

    #[test]
    fn describe_says_when_everything_was_the_built_in_default() {
        let target = BoardTarget {
            hosts: vec![DEFAULT_CQL_HOST.into()],
            host_source: SettingSource::BuiltIn,
            credential: BoardCredential::built_in(),
        };
        assert_eq!(
            target.describe(),
            "no config found, used built-in defaults: host 127.0.0.1:9042, built-in development \
             credential ferrosa_admin"
        );
        let flagged = BoardTarget {
            host_source: SettingSource::Explicit,
            ..target
        };
        assert!(
            flagged
                .describe()
                .contains("from the built-in development credential ferrosa_admin"),
            "{}",
            flagged.describe()
        );
    }

    fn tempfile_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("forge_{tag}_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn explicit_wins_over_everything() {
        assert_eq!(
            pick(
                Some("h:1"),
                Some("e:2".into()),
                Some("f:3".into()),
                Some("g:4".into())
            ),
            "h:1"
        );
    }

    #[test]
    fn env_wins_when_no_explicit() {
        assert_eq!(
            pick(
                None,
                Some("e:2".into()),
                Some("f:3".into()),
                Some("g:4".into())
            ),
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
            pick(
                None,
                None,
                Some("project:1".into()),
                Some("global:2".into())
            ),
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
        assert_eq!(
            parse_cql_host_toml(body).unwrap().as_deref(),
            Some("127.0.0.1:47017")
        );
        assert_eq!(parse_debug_stop_toml(body).unwrap(), Some(true));
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
        assert_eq!(resolve_cql_hosts(Some("h:1")).unwrap(), vec!["h:1"]);
        assert_eq!(
            resolve_cql_hosts(Some("n1:19042,n2:19042,n3:19042")).unwrap(),
            vec!["n1:19042", "n2:19042", "n3:19042"]
        );
    }

    #[test]
    fn parses_debug_stop_and_explicit_wins() {
        assert_eq!(
            parse_debug_stop_toml("debug_stop = true\n").unwrap(),
            Some(true)
        );
        assert_eq!(
            parse_debug_stop_toml("debug_stop = false\n").unwrap(),
            Some(false)
        );
        assert_eq!(parse_debug_stop_toml("cql_host = \"h:1\"\n").unwrap(), None);
        // explicit arg short-circuits env/file
        assert!(resolve_debug_stop(Some(true)).unwrap());
        assert!(!resolve_debug_stop(Some(false)).unwrap());
    }

    /// A typo in `FORGE_DEBUG_STOP` used to fall through to `false`, silently
    /// disabling the degraded-board alerting the operator had just switched on.
    /// Asserted on the pure parser: mutating the process environment would race
    /// the other tests in this binary.
    #[test]
    fn an_unparseable_debug_stop_value_is_an_error_not_a_silent_off() {
        assert!(parse_debug_stop_env("1").unwrap());
        assert!(!parse_debug_stop_env(" OFF ").unwrap());
        let err =
            parse_debug_stop_env("sure").expect_err("a non-boolean value must not be read as off");
        assert!(format!("{err:#}").contains("FORGE_DEBUG_STOP"));
    }

    #[test]
    fn parses_cql_host_from_toml() {
        assert_eq!(
            parse_cql_host_toml("cql_host = \"127.0.0.1:19042\"\n")
                .unwrap()
                .as_deref(),
            Some("127.0.0.1:19042")
        );
    }

    #[test]
    fn toml_without_cql_host_is_none() {
        assert_eq!(parse_cql_host_toml("other = 1\n").unwrap(), None);
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
        assert_eq!(got.unwrap().as_deref(), Some("10.0.0.1:9999"));
    }

    /// A `.forge/config.toml` with a syntax error used to be discarded by
    /// `toml::from_str(..).ok()`, so forge queried `127.0.0.1:9042` while the
    /// user was looking at a file naming a different port. The board then read
    /// as empty (or as somebody else's), with nothing to say the file had been
    /// ignored -- the same "I could not look" reported as "there is nothing".
    #[test]
    fn a_malformed_config_is_an_error_not_a_silent_fallback_to_the_default_host() {
        let err = parse_cql_host_toml("cql_host = \n")
            .expect_err("a malformed config must not be silently discarded");
        assert!(
            format!("{err:#}").contains("TOML"),
            "the error should say the file is not valid TOML, got: {err:#}"
        );
        assert!(parse_debug_stop_toml("debug_stop = maybe\n").is_err());
    }

    #[test]
    fn a_config_file_that_is_absent_is_not_an_error() {
        // Absent means "not configured at this layer", which is the normal case
        // and must keep falling through to the next one.
        let missing = std::env::temp_dir().join("forge_no_such_config_file.toml");
        assert!(read_config_file(&missing).unwrap().is_none());
    }

    #[test]
    fn a_config_file_that_cannot_be_read_is_an_error() {
        // A directory where the file should be: it exists, so "not configured"
        // is the wrong conclusion, and reading it fails with something other
        // than NotFound.
        let base = std::env::temp_dir().join(format!("forge_unreadable_{}", std::process::id()));
        let path = base.join("config.toml");
        std::fs::create_dir_all(&path).unwrap();
        let got = read_config_file(&path);
        std::fs::remove_dir_all(&base).ok();
        assert!(
            got.is_err(),
            "a config path that exists but cannot be read must be an error"
        );
    }

    #[test]
    fn a_malformed_project_config_names_the_file_it_came_from() {
        let base = std::env::temp_dir().join(format!("forge_badcfg_{}", std::process::id()));
        std::fs::create_dir_all(base.join(".forge")).unwrap();
        std::fs::write(base.join(".forge").join("config.toml"), "cql_host = \n").unwrap();
        let got = read_config_cql_host(&base);
        std::fs::remove_dir_all(&base).ok();
        let err = got.expect_err("a malformed project config must be an error");
        assert!(
            format!("{err:#}").contains("config.toml"),
            "the error must name the offending file, got: {err:#}"
        );
    }
}
