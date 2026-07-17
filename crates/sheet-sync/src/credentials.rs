//! Resolves which Google credential flow this run uses — a headless
//! service account, or the interactive OAuth installed-app loopback flow —
//! and provides the flow-agnostic entry points (`access_token`/`authorize`)
//! that `crates/cli` calls instead of reaching into `oauth`/`service_account`
//! directly.
//!
//! Service account is preferred whenever it's configured: it's the only
//! option that works in a headless/CI/agent environment (no browser, no
//! interactive consent), and an operator who has gone to the trouble of
//! provisioning a service account key clearly wants it used.

use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::oauth::{self, AccessToken, OAuthClient};
use crate::service_account::{self, ServiceAccount};

/// Env var naming a Google service-account key JSON path directly. Takes
/// precedence over `.forge/config.toml`, mirroring
/// `oauth::CLIENT_SECRET_ENV`'s precedence for the OAuth path.
const SERVICE_ACCOUNT_ENV: &str = "FORGE_GOOGLE_SERVICE_ACCOUNT";

/// The resolved Google credential source for this run.
#[derive(Debug)]
pub enum GoogleCreds {
    ServiceAccount(ServiceAccount),
    OAuth(OAuthClient),
}

#[derive(Debug, Default, Deserialize)]
struct ForgeConfig {
    google: Option<GoogleSection>,
}

#[derive(Debug, Default, Deserialize)]
struct GoogleSection {
    service_account_path: Option<String>,
}

/// Parses `[google] service_account_path` out of a `.forge/config.toml`
/// body. Pure; testable. Mirrors
/// `oauth::parse_config_client_secret_path`.
fn parse_config_service_account_path(body: &str) -> Option<String> {
    toml::from_str::<ForgeConfig>(body)
        .ok()
        .and_then(|c| c.google)
        .and_then(|g| g.service_account_path)
}

/// Walks up from `start` looking for a `.forge/config.toml` with a
/// `[google] service_account_path`, mirroring
/// `oauth::locate_config_client_secret_path` exactly (that helper is
/// private to `oauth`, so this duplicates the ancestor-walk rather than
/// sharing it — same convention as the small helper duplication already in
/// this crate, e.g. `push_plan::cell` vs `mapping::cell`).
fn locate_config_service_account_path(start: &Path) -> Option<String> {
    for dir in start.ancestors() {
        let candidate = dir.join(".forge").join("config.toml");
        if let Ok(body) = std::fs::read_to_string(&candidate) {
            if let Some(path) = parse_config_service_account_path(&body) {
                return Some(path);
            }
        }
    }
    None
}

/// Resolves the service-account key JSON path per [`resolve`]'s
/// precedence (`FORGE_GOOGLE_SERVICE_ACCOUNT` env, else
/// `[google] service_account_path` in the nearest `.forge/config.toml`),
/// or `None` if neither is configured — not an error on its own, since the
/// OAuth path is a legitimate fallback.
fn resolve_service_account_path() -> anyhow::Result<Option<PathBuf>> {
    if let Ok(raw) = std::env::var(SERVICE_ACCOUNT_ENV) {
        if !raw.trim().is_empty() {
            return Ok(Some(PathBuf::from(raw)));
        }
    }

    let cwd = std::env::current_dir()
        .map_err(|e| anyhow::anyhow!("credentials: failed to read current directory: {e}"))?;
    Ok(locate_config_service_account_path(&cwd).map(PathBuf::from))
}

/// Resolves which Google credential flow this run should use.
///
/// Prefers a service account (headless, no browser) whenever
/// `FORGE_GOOGLE_SERVICE_ACCOUNT` or `.forge/config.toml`'s
/// `[google] service_account_path` names one; otherwise falls back to the
/// interactive OAuth installed-app flow via [`OAuthClient::load`]. Fails
/// loud, naming *both* configuration routes, only if neither is
/// configured — there is no default Google credential to fall back to.
pub fn resolve() -> anyhow::Result<GoogleCreds> {
    if let Some(path) = resolve_service_account_path()? {
        let body = std::fs::read_to_string(&path).map_err(|e| {
            anyhow::anyhow!(
                "credentials: failed to read Google service-account key file {}: {e}",
                path.display()
            )
        })?;
        let sa = service_account::parse_service_account_json(&body)?;
        return Ok(GoogleCreds::ServiceAccount(sa));
    }

    match OAuthClient::load() {
        Ok(client) => Ok(GoogleCreds::OAuth(client)),
        Err(oauth_err) => Err(anyhow::anyhow!(
            "credentials: no Google credentials configured — for headless/non-interactive auth, set {SERVICE_ACCOUNT_ENV} to a service-account key JSON path (or add `[google] service_account_path = \"...\"` to .forge/config.toml); for interactive auth: {oauth_err}"
        )),
    }
}

/// Returns a fresh [`AccessToken`] for `alias` via whichever credential
/// flow [`resolve`] selects.
pub fn access_token(alias: &str) -> anyhow::Result<AccessToken> {
    match resolve()? {
        GoogleCreds::ServiceAccount(sa) => service_account::access_token(&sa),
        GoogleCreds::OAuth(client) => oauth::access_token(alias, &client),
    }
}

/// The outcome of [`authorize`], returned to CLI/MCP callers so they can
/// report which flow actually ran.
#[derive(Debug, Clone, serde::Serialize)]
pub struct AuthOutcome {
    pub authorized: bool,
    pub method: String,
}

/// Authorizes `alias` for whichever credential flow [`resolve`] selects.
/// A service account needs no interactive step at all — [`resolve`] having
/// already loaded and validated the key is sufficient, so this is a no-op
/// beyond that. An OAuth client runs the full interactive browser consent
/// flow via [`oauth::authorize`].
pub fn authorize(alias: &str) -> anyhow::Result<AuthOutcome> {
    match resolve()? {
        GoogleCreds::ServiceAccount(_) => Ok(AuthOutcome {
            authorized: true,
            method: "service_account".to_string(),
        }),
        GoogleCreds::OAuth(client) => {
            oauth::authorize(alias, &client)?;
            Ok(AuthOutcome {
                authorized: true,
                method: "oauth".to_string(),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // Both `resolve`'s env-var checks mutate process-global state
    // (`std::env::var`/`remove_var`, `std::env::set_current_dir`), so
    // tests that touch `FORGE_GOOGLE_SERVICE_ACCOUNT`/`FORGE_GOOGLE_OAUTH_CLIENT`
    // or the cwd must never run concurrently with each other or with
    // `oauth`'s own `CLIENT_SECRET_ENV_LOCK`-guarded test — Rust's default
    // `cargo test` runner runs tests in the same crate on multiple
    // threads, and both env vars/cwd are genuinely process-global, so an
    // ad hoc per-module lock does not fully prevent a race against
    // `oauth::tests`. This lock at least prevents this module's own tests
    // from racing each other.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    const VALID_SA_JSON: &str = r#"{
        "type": "service_account",
        "client_email": "t@x.iam.gserviceaccount.com",
        "private_key": "-----BEGIN PRIVATE KEY-----\nnotarealkey\n-----END PRIVATE KEY-----\n",
        "token_uri": "https://oauth2.googleapis.com/token"
    }"#;

    #[test]
    fn resolve_prefers_service_account_from_env() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());

        let prior_sa = std::env::var(SERVICE_ACCOUNT_ENV).ok();
        let prior_oauth = std::env::var(oauth::CLIENT_SECRET_ENV).ok();
        std::env::remove_var(oauth::CLIENT_SECRET_ENV);

        let tmpdir = tempfile::tempdir().expect("tempdir creation");
        let sa_path = tmpdir.path().join("sa.json");
        std::fs::write(&sa_path, VALID_SA_JSON).expect("write fixture SA json");
        std::env::set_var(SERVICE_ACCOUNT_ENV, &sa_path);

        let result = resolve();

        std::env::remove_var(SERVICE_ACCOUNT_ENV);
        if let Some(value) = prior_sa {
            std::env::set_var(SERVICE_ACCOUNT_ENV, value);
        }
        if let Some(value) = prior_oauth {
            std::env::set_var(oauth::CLIENT_SECRET_ENV, value);
        }

        match result.expect("service account should resolve") {
            GoogleCreds::ServiceAccount(sa) => {
                assert_eq!(sa.client_email, "t@x.iam.gserviceaccount.com");
            }
            GoogleCreds::OAuth(_) => panic!("expected ServiceAccount, got OAuth"),
        }
    }

    #[test]
    fn resolve_errs_naming_both_options_when_neither_configured() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());

        let prior_sa = std::env::var(SERVICE_ACCOUNT_ENV).ok();
        let prior_oauth = std::env::var(oauth::CLIENT_SECRET_ENV).ok();
        std::env::remove_var(SERVICE_ACCOUNT_ENV);
        std::env::remove_var(oauth::CLIENT_SECRET_ENV);

        // Run from a tempdir with no `.forge/config.toml` in any ancestor
        // this process could plausibly be running from, matching
        // `oauth::tests::load_errs_when_env_unset_and_no_config_file`.
        let tmpdir = tempfile::tempdir().expect("tempdir creation");
        let original_cwd = std::env::current_dir().expect("read cwd");
        std::env::set_current_dir(tmpdir.path()).expect("chdir to tempdir");

        let result = resolve();

        std::env::set_current_dir(original_cwd).expect("restore cwd");
        if let Some(value) = prior_sa {
            std::env::set_var(SERVICE_ACCOUNT_ENV, value);
        }
        if let Some(value) = prior_oauth {
            std::env::set_var(oauth::CLIENT_SECRET_ENV, value);
        }

        let err = result.expect_err("no config source should fail loud");
        let msg = err.to_string();
        assert!(msg.contains(SERVICE_ACCOUNT_ENV));
        assert!(msg.contains("service_account_path"));
        assert!(msg.contains(oauth::CLIENT_SECRET_ENV));
        assert!(msg.contains("client_secret_path"));
    }
}
