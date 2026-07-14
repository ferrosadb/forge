//! OAuth 2.0 "installed application" loopback flow for Google Sheets
//! access, plus the on-disk refresh-token cache that lets later runs skip
//! the interactive consent step.
//!
//! This module is the I/O shell around a handful of *pure* helpers
//! ([`parse_client_secret_json`], [`parse_token_response`],
//! [`build_auth_url`], [`parse_redirect_query`]) that carry all the
//! branching logic and are unit-tested directly, without sockets or a
//! filesystem. [`authorize`]/[`access_token`]/[`OAuthClient::load`] are
//! thin glue over those helpers plus `std::net`/`std::fs`/`ureq`.
//!
//! ## Secrets
//!
//! Access tokens, refresh tokens, and authorization codes must never
//! appear in an `anyhow` error message or log line verbatim — see
//! `redact_token_fields`, applied to every HTTP error body this module
//! surfaces.

use std::collections::HashMap;
use std::io::{BufRead, Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// Env var naming a Google OAuth `client_secret.json` path directly. Takes
/// precedence over `.forge/config.toml`.
const CLIENT_SECRET_ENV: &str = "FORGE_GOOGLE_OAUTH_CLIENT";

/// The Sheets API scope this crate requests (read + write cell values).
const SHEETS_SCOPE: &str = "https://www.googleapis.com/auth/spreadsheets";

/// Per-request timeout for token-endpoint calls, matching the pattern in
/// `crates/ingest`/`crates/fmem-client`.
const HTTP_TIMEOUT: Duration = Duration::from_secs(30);

/// Upper bound, in bytes, on the loopback redirect's HTTP request line
/// read in [`authorize`]. Generous for a real
/// `GET /?code=...&state=... HTTP/1.1` line (which is at most a few
/// hundred bytes even with long OAuth codes/state), while still capping
/// growth if a local connection never sends a newline.
const LOOPBACK_REQUEST_LINE_LIMIT: u64 = 8192;

/// OAuth client identity and endpoints, loaded from a Google
/// `client_secret.json`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OAuthClient {
    pub client_id: String,
    pub client_secret: String,
    pub auth_uri: String,
    pub token_uri: String,
}

/// A short-lived Sheets API bearer token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccessToken {
    pub token: String,
}

/// Google's `client_secret.json` shape: the real fields live nested under
/// either an `"installed"` key (desktop apps, what this crate uses) or a
/// `"web"` key (web apps) — never both, but we accept either so a
/// misconfigured/copy-pasted credential type still works.
#[derive(Debug, Deserialize)]
struct ClientSecretFile {
    installed: Option<ClientSecretInner>,
    web: Option<ClientSecretInner>,
}

#[derive(Debug, Deserialize)]
struct ClientSecretInner {
    client_id: String,
    client_secret: String,
    auth_uri: String,
    token_uri: String,
}

/// Parses a Google OAuth `client_secret.json` body. Pure — no filesystem,
/// no network — so this is unit-testable directly.
pub fn parse_client_secret_json(body: &str) -> anyhow::Result<OAuthClient> {
    let file: ClientSecretFile = serde_json::from_str(body)
        .map_err(|e| anyhow::anyhow!("oauth: invalid client_secret JSON: {e}"))?;
    let inner = file.installed.or(file.web).ok_or_else(|| {
        anyhow::anyhow!(
            "oauth: client_secret JSON has neither an `installed` nor a `web` key — is this a Google OAuth client_secret.json?"
        )
    })?;
    Ok(OAuthClient {
        client_id: inner.client_id,
        client_secret: inner.client_secret,
        auth_uri: inner.auth_uri,
        token_uri: inner.token_uri,
    })
}

#[derive(Debug, Default, Deserialize)]
struct ForgeConfig {
    google: Option<GoogleSection>,
}

#[derive(Debug, Default, Deserialize)]
struct GoogleSection {
    client_secret_path: Option<String>,
}

/// Parses `[google] client_secret_path` out of a `.forge/config.toml`
/// body. Pure; testable.
fn parse_config_client_secret_path(body: &str) -> Option<String> {
    toml::from_str::<ForgeConfig>(body)
        .ok()
        .and_then(|c| c.google)
        .and_then(|g| g.client_secret_path)
}

/// Walks up from `start` looking for a `.forge/config.toml` with a
/// `[google] client_secret_path`, mirroring the ancestor-walk pattern in
/// `crate::config::SheetMapping::alias_path` /
/// `crates/tasks/src/config.rs::read_config_cql_host`.
fn locate_config_client_secret_path(start: &Path) -> Option<String> {
    for dir in start.ancestors() {
        let candidate = dir.join(".forge").join("config.toml");
        if let Ok(body) = std::fs::read_to_string(&candidate) {
            if let Some(path) = parse_config_client_secret_path(&body) {
                return Some(path);
            }
        }
    }
    None
}

impl OAuthClient {
    /// Loads the OAuth client from `FORGE_GOOGLE_OAUTH_CLIENT` (a
    /// `client_secret.json` path), else `[google] client_secret_path` in
    /// the nearest `.forge/config.toml` walking up from the cwd. Fails
    /// loud, naming both ways to configure it, if neither is present —
    /// there is no default Google OAuth client to fall back to.
    pub fn load() -> anyhow::Result<OAuthClient> {
        let path = resolve_client_secret_path()?;
        let body = std::fs::read_to_string(&path).map_err(|e| {
            anyhow::anyhow!(
                "oauth: failed to read Google OAuth client secret file {}: {e}",
                path.display()
            )
        })?;
        parse_client_secret_json(&body)
    }
}

/// Resolves the `client_secret.json` path per [`OAuthClient::load`]'s
/// precedence, failing loud if neither source is configured.
fn resolve_client_secret_path() -> anyhow::Result<PathBuf> {
    if let Ok(raw) = std::env::var(CLIENT_SECRET_ENV) {
        if !raw.trim().is_empty() {
            return Ok(PathBuf::from(raw));
        }
    }

    let cwd = std::env::current_dir()
        .map_err(|e| anyhow::anyhow!("oauth: failed to read current directory: {e}"))?;
    if let Some(path) = locate_config_client_secret_path(&cwd) {
        return Ok(PathBuf::from(path));
    }

    anyhow::bail!(
        "oauth: no Google OAuth client configured — set {CLIENT_SECRET_ENV} to a client_secret.json path (from Google Cloud Console → APIs & Services → Credentials → OAuth client), or add `[google] client_secret_path = \"...\"` to .forge/config.toml"
    )
}

/// The Google token endpoint's JSON response shape.
#[derive(Debug, Clone, Deserialize)]
pub struct TokenResponse {
    pub access_token: String,
    #[serde(default)]
    pub refresh_token: Option<String>,
    #[serde(default)]
    pub expires_in: Option<u64>,
}

/// Parses a token-endpoint JSON response. Pure — no network — so this is
/// unit-testable directly. A missing `refresh_token` (Google only returns
/// one on the *first* consent, or when `prompt=consent` forces a fresh
/// one) parses as `None`, not an error — callers decide whether that's
/// acceptable for their flow.
pub fn parse_token_response(body: &str) -> anyhow::Result<TokenResponse> {
    serde_json::from_str(body)
        .map_err(|e| anyhow::anyhow!("oauth: invalid token response JSON (parse error: {e})"))
}

/// Percent-encodes `s` for use in a URL query component (unreserved set:
/// ASCII alphanumerics plus `-_.~`; everything else, byte-by-byte, so
/// multi-byte UTF-8 sequences percent-encode correctly). Pure; no
/// dependency on the `url` crate, which isn't otherwise needed here.
fn url_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Percent-decodes `s` (also treating `+` as a space, per
/// `application/x-www-form-urlencoded`/query-string convention). Pure;
/// the inverse of [`url_encode`] as used by [`parse_redirect_query`].
fn url_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                if let Ok(byte) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                    out.push(byte);
                    i += 3;
                    continue;
                }
                out.push(bytes[i]);
                i += 1;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            other => {
                out.push(other);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Builds the Google consent-screen URL for the installed-app loopback
/// flow: `response_type=code`, `access_type=offline` (request a refresh
/// token), `prompt=consent` (force one even on a repeat authorization),
/// plus the caller's `client_id`/`redirect_uri`/`scope`/`state`, all
/// percent-encoded. Pure — no network — so this is unit-testable
/// directly.
pub fn build_auth_url(
    client: &OAuthClient,
    redirect_uri: &str,
    state: &str,
    scope: &str,
) -> String {
    format!(
        "{}?response_type=code&client_id={}&redirect_uri={}&scope={}&state={}&access_type=offline&prompt=consent",
        client.auth_uri,
        url_encode(&client.client_id),
        url_encode(redirect_uri),
        url_encode(scope),
        url_encode(state),
    )
}

/// Parses the query string off an HTTP request line
/// (`"GET /?code=abc&state=xyz HTTP/1.1"`) as sent by the browser to the
/// loopback listener. Pure — no sockets — so this is unit-testable
/// directly. Fails loud if the request line is malformed or has no
/// `code` param — a redirect without a code means the user denied
/// consent or Google sent an error, either way there is nothing to
/// exchange.
pub fn parse_redirect_query(request_line: &str) -> anyhow::Result<HashMap<String, String>> {
    let mut parts = request_line.split_whitespace();
    parts
        .next()
        .filter(|method| !method.is_empty())
        .ok_or_else(|| anyhow::anyhow!("oauth: empty redirect request line"))?;
    let target = parts.next().ok_or_else(|| {
        anyhow::anyhow!(
            "oauth: malformed redirect request line (no request target): {request_line:?}"
        )
    })?;

    let query = target.split_once('?').map(|(_, q)| q).unwrap_or("");
    let mut map = HashMap::new();
    for pair in query.split('&') {
        if pair.is_empty() {
            continue;
        }
        let mut kv = pair.splitn(2, '=');
        let key = kv.next().unwrap_or("");
        let value = kv.next().unwrap_or("");
        if key.is_empty() {
            continue;
        }
        map.insert(url_decode(key), url_decode(value));
    }

    if !map.contains_key("code") {
        anyhow::bail!(
            "oauth: redirect request has no `code` query param (user may have denied consent): {request_line:?}"
        );
    }

    Ok(map)
}

/// Best-effort redaction of token-shaped JSON values from `s`, so an
/// echoed HTTP error body never leaks a live token into a returned error
/// message or log line. Scans for `"<key>":"` for each sensitive key and
/// replaces the following quoted value with `REDACTED`. Pure; not a
/// claimed security boundary on its own (defense in depth alongside never
/// interpolating a token directly), but it's this module's only
/// diagnostic surface that touches raw HTTP response bodies.
fn redact_token_fields(s: &str) -> String {
    let mut out = s.to_string();
    for key in ["access_token", "refresh_token", "id_token"] {
        let needle = format!("\"{key}\":\"");
        // `search_from` must advance past each redacted region — the key
        // prefix itself is never removed, so re-searching from the start
        // of `out` would re-find the same occurrence forever.
        let mut search_from = 0;
        while let Some(rel_start) = out[search_from..].find(&needle) {
            let start = search_from + rel_start;
            let value_start = start + needle.len();
            match out[value_start..].find('"') {
                Some(rel_end) => {
                    let value_end = value_start + rel_end;
                    out.replace_range(value_start..value_end, "REDACTED");
                    search_from = value_start + "REDACTED".len() + 1; // past the closing quote
                }
                None => break,
            }
        }
    }
    out
}

/// One `ureq::Agent` config shared by the token-endpoint calls in this
/// module, matching `crates/ingest`'s 30s-timeout pattern.
///
/// `http_status_as_error(false)` is essential here, not cosmetic: `ureq`
/// v3 defaults to turning any 4xx/5xx into `Err(Error::StatusCode)` at
/// `.send_form()` time, which would make the `status.is_success()`
/// check (and the [`redact_token_fields`]-scrubbed `bail!`) in
/// [`post_token_request`] unreachable dead code and throw away Google's
/// `error_description` in favor of a bare status number.
fn build_agent() -> ureq::Agent {
    let config = ureq::Agent::config_builder()
        .timeout_global(Some(HTTP_TIMEOUT))
        .http_status_as_error(false)
        .build();
    ureq::Agent::new_with_config(config)
}

/// POSTs a `application/x-www-form-urlencoded` request to `token_uri` and
/// parses the JSON response. Fails loud on a non-2xx status, including a
/// redacted excerpt of the response body (Google's token-endpoint error
/// bodies are `{"error": ..., "error_description": ...}` and don't
/// normally carry a token, but [`redact_token_fields`] scrubs one anyway
/// as defense in depth).
fn post_token_request(
    agent: &ureq::Agent,
    token_uri: &str,
    form: &[(&str, &str)],
) -> anyhow::Result<TokenResponse> {
    let mut resp = agent
        .post(token_uri)
        .send_form(form.iter().copied())
        .map_err(|e| anyhow::anyhow!("oauth: token request to {token_uri} failed: {e}"))?;

    let status = resp.status();
    let body = resp
        .body_mut()
        .read_to_string()
        .map_err(|e| anyhow::anyhow!("oauth: failed to read token response body: {e}"))?;

    if !status.is_success() {
        anyhow::bail!(
            "oauth: token request to {token_uri} returned HTTP {}: {}",
            status.as_u16(),
            redact_token_fields(&body.chars().take(500).collect::<String>())
        );
    }

    parse_token_response(&body)
}

/// Non-cryptographic but unpredictable-enough `state` value for the CSRF
/// guard: mixes wall-clock time, process id, and the loopback port,
/// base64url-encoded. This is I/O-adjacent glue (not the deterministic
/// pure engine), so `SystemTime`/`process::id` are fine here — see the
/// crate's task brief. Deliberately not unit-tested (nothing pure to
/// assert about randomness).
fn generate_state(port: u16) -> String {
    use base64::Engine as _;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let raw = format!("{}-{}-{}", now.as_nanos(), std::process::id(), port);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(raw)
}

/// The on-disk refresh-token cache shape: `{ "refresh_token": "..." }`.
#[derive(Debug, Serialize, Deserialize)]
struct TokenCache {
    refresh_token: String,
}

/// Path to the cached refresh token for `alias`:
/// `dirs::config_dir()/forge/sheet-sync/<alias>.json`.
pub fn token_cache_path(alias: &str) -> PathBuf {
    dirs::config_dir()
        .expect("oauth: no config directory available on this platform")
        .join("forge")
        .join("sheet-sync")
        .join(format!("{alias}.json"))
}

/// Best-effort 0600/0700-style tightening of the token cache file and its
/// parent directory, mirroring `crate::state`'s
/// `tighten_permissions_best_effort` (duplicated locally since that
/// helper is private to its module). Never fails the caller — a chmod
/// failure on an unsupported filesystem is not a reason to lose a
/// successful token save. Unlike `state.json`, this file holds a live
/// OAuth **refresh token**, so a swallowed chmod failure here is a
/// swallowed secret-exposure risk — per the repo's fail-loud/disclosure
/// rules, best-effort is fine but silent is not: each failure is logged
/// to stderr (path + OS error only, never token text) so a permissive
/// mode on a shared/multi-user machine is observable instead of hidden.
#[cfg(unix)]
fn tighten_permissions_best_effort(path: &Path) {
    use std::os::unix::fs::PermissionsExt;

    if let Err(e) = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)) {
        eprintln!(
            "warning: could not tighten permissions on {} (refresh token may be readable by other local users): {e}",
            path.display()
        );
    }
    if let Some(parent) = path.parent() {
        if let Err(e) = std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700)) {
            eprintln!(
                "warning: could not tighten permissions on {} (refresh token's directory may be accessible by other local users): {e}",
                parent.display()
            );
        }
    }
}

#[cfg(not(unix))]
fn tighten_permissions_best_effort(_path: &Path) {}

/// Persists `refresh_token` to `alias`'s cache file, creating parent dirs
/// and best-effort chmod'ing to 0600/0700.
fn save_refresh_token(alias: &str, refresh_token: &str) -> anyhow::Result<()> {
    let path = token_cache_path(alias);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| {
            anyhow::anyhow!(
                "oauth: failed to create token cache directory {}: {e}",
                parent.display()
            )
        })?;
    }

    let body = serde_json::to_string_pretty(&TokenCache {
        refresh_token: refresh_token.to_string(),
    })
    .map_err(|e| anyhow::anyhow!("oauth: failed to serialize token cache: {e}"))?;

    std::fs::write(&path, body).map_err(|e| {
        anyhow::anyhow!("oauth: failed to write token cache {}: {e}", path.display())
    })?;

    tighten_permissions_best_effort(&path);

    Ok(())
}

/// Loads the cached refresh token for `alias`. Fails loud, telling the
/// caller how to fix it, if no cache exists yet.
fn load_refresh_token(alias: &str) -> anyhow::Result<String> {
    let path = token_cache_path(alias);
    let body = std::fs::read_to_string(&path).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            anyhow::anyhow!(
                "oauth: no cached credentials for alias {alias:?} — run `frg sheet auth {alias}` first"
            )
        } else {
            anyhow::anyhow!("oauth: failed to read token cache {}: {e}", path.display())
        }
    })?;
    let cache: TokenCache = serde_json::from_str(&body).map_err(|e| {
        anyhow::anyhow!("oauth: invalid token cache JSON at {}: {e}", path.display())
    })?;
    Ok(cache.refresh_token)
}

/// Runs the installed-app OAuth loopback flow end to end for `alias` and
/// persists the resulting refresh token:
///
/// 1. Bind an ephemeral loopback listener (`127.0.0.1:0`) and read back
///    the OS-assigned port.
/// 2. Print the consent URL (built by [`build_auth_url`]) to stderr — this
///    crate does not depend on a browser-opener, the caller copies/opens
///    the URL themselves.
/// 3. Accept exactly one connection, parse the redirect's `code`/`state`
///    (via [`parse_redirect_query`]), and fail loud on a `state` mismatch
///    (CSRF guard) before responding `200 OK` with a short HTML body.
/// 4. Exchange `code` at `client.token_uri` for an access + refresh token.
/// 5. Persist the refresh token via `save_refresh_token`, failing loud
///    if Google didn't return one.
pub fn authorize(alias: &str, client: &OAuthClient) -> anyhow::Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0")
        .map_err(|e| anyhow::anyhow!("oauth: failed to bind loopback listener: {e}"))?;
    let port = listener
        .local_addr()
        .map_err(|e| anyhow::anyhow!("oauth: failed to read loopback listener address: {e}"))?
        .port();
    let redirect_uri = format!("http://127.0.0.1:{port}/");
    let expected_state = generate_state(port);

    let auth_url = build_auth_url(client, &redirect_uri, &expected_state, SHEETS_SCOPE);
    eprintln!("Open this URL to authorize:\n{auth_url}");

    let (mut stream, _) = listener.accept().map_err(|e| {
        anyhow::anyhow!("oauth: failed to accept loopback redirect connection: {e}")
    })?;

    // Bounded via `.take(LOOPBACK_REQUEST_LINE_LIMIT)`: this is a
    // request line from a local browser redirect, not untrusted network
    // input, but nothing here guarantees a newline ever arrives (a
    // malformed or truncated local payload), so an unbounded
    // `read_line` could otherwise grow `request_line` without limit. A
    // normal `GET /?code=...&state=... HTTP/1.1` line is well under the
    // cap, so behavior for a real redirect is unchanged.
    let mut reader = std::io::BufReader::new(&stream).take(LOOPBACK_REQUEST_LINE_LIMIT);
    let mut request_line = String::new();
    reader
        .read_line(&mut request_line)
        .map_err(|e| anyhow::anyhow!("oauth: failed to read loopback redirect request: {e}"))?;

    let query = parse_redirect_query(&request_line)?;
    let code = query
        .get("code")
        .expect("parse_redirect_query already guarantees `code` is present");
    let got_state = query.get("state").map(String::as_str).unwrap_or("");
    if got_state != expected_state {
        anyhow::bail!(
            "oauth: redirect `state` did not match the value this flow generated — possible CSRF, aborting without exchanging the code"
        );
    }

    let response_body =
        "<html><body>Authorization complete \u{2014} you can close this tab.</body></html>";
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        response_body.len(),
        response_body
    );
    stream
        .write_all(response.as_bytes())
        .map_err(|e| anyhow::anyhow!("oauth: failed to write loopback redirect response: {e}"))?;

    let agent = build_agent();
    let form = [
        ("grant_type", "authorization_code"),
        ("code", code.as_str()),
        ("redirect_uri", redirect_uri.as_str()),
        ("client_id", client.client_id.as_str()),
        ("client_secret", client.client_secret.as_str()),
    ];
    let token_response = post_token_request(&agent, &client.token_uri, &form)?;

    let refresh_token = token_response.refresh_token.ok_or_else(|| {
        anyhow::anyhow!(
            "oauth: Google did not return a refresh_token for this grant — revoke the app's prior access at https://myaccount.google.com/permissions and re-run authorize (this flow already sends prompt=consent, so a revoke-then-retry should force a fresh refresh_token)"
        )
    })?;

    save_refresh_token(alias, &refresh_token)
}

/// Returns a fresh [`AccessToken`] for `alias` by refreshing the cached
/// refresh token. Fails loud (naming the `authorize` command to run) if
/// no cache exists yet.
pub fn access_token(alias: &str, client: &OAuthClient) -> anyhow::Result<AccessToken> {
    let refresh_token = load_refresh_token(alias)?;
    let agent = build_agent();
    let form = [
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh_token.as_str()),
        ("client_id", client.client_id.as_str()),
        ("client_secret", client.client_secret.as_str()),
    ];
    let token_response = post_token_request(&agent, &client.token_uri, &form)?;
    Ok(AccessToken {
        token: token_response.access_token,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // `OAuthClient::load`'s env-var test mutates process-global state
    // (`std::env::var`/`remove_var`), so it must never run concurrently
    // with another test that also touches `FORGE_GOOGLE_OAUTH_CLIENT`.
    // This crate has exactly one such test; the mutex is a belt-and-
    // braces guard against a future second one racing it.
    static CLIENT_SECRET_ENV_LOCK: Mutex<()> = Mutex::new(());

    const INSTALLED_JSON: &str = r#"{
        "installed": {
            "client_id": "abc123.apps.googleusercontent.com",
            "client_secret": "shh-secret",
            "auth_uri": "https://accounts.google.com/o/oauth2/auth",
            "token_uri": "https://oauth2.googleapis.com/token"
        }
    }"#;

    const WEB_JSON: &str = r#"{
        "web": {
            "client_id": "web-client-id",
            "client_secret": "web-secret",
            "auth_uri": "https://accounts.google.com/o/oauth2/auth",
            "token_uri": "https://oauth2.googleapis.com/token"
        }
    }"#;

    #[test]
    fn parse_client_secret_json_reads_installed_form() {
        let client = parse_client_secret_json(INSTALLED_JSON).expect("valid installed JSON");
        assert_eq!(client.client_id, "abc123.apps.googleusercontent.com");
        assert_eq!(client.client_secret, "shh-secret");
        assert_eq!(client.auth_uri, "https://accounts.google.com/o/oauth2/auth");
        assert_eq!(client.token_uri, "https://oauth2.googleapis.com/token");
    }

    #[test]
    fn parse_client_secret_json_reads_web_form() {
        let client = parse_client_secret_json(WEB_JSON).expect("valid web JSON");
        assert_eq!(client.client_id, "web-client-id");
        assert_eq!(client.client_secret, "web-secret");
    }

    #[test]
    fn parse_client_secret_json_rejects_garbage() {
        let err = parse_client_secret_json("not json at all").expect_err("garbage should fail");
        assert!(err.to_string().contains("invalid client_secret JSON"));
    }

    #[test]
    fn parse_client_secret_json_rejects_missing_installed_and_web() {
        let err = parse_client_secret_json(r#"{"other": {}}"#).expect_err("neither key present");
        assert!(err.to_string().contains("installed"));
    }

    #[test]
    fn load_errs_when_env_unset_and_no_config_file() {
        let _guard = CLIENT_SECRET_ENV_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());

        let prior = std::env::var(CLIENT_SECRET_ENV).ok();
        std::env::remove_var(CLIENT_SECRET_ENV);

        // Run from a tempdir with no `.forge/config.toml` in any ancestor
        // this process could plausibly be running from — guarded further
        // by asserting the error text names both configuration routes.
        let tmpdir = tempfile::tempdir().expect("tempdir creation");
        let original_cwd = std::env::current_dir().expect("read cwd");
        std::env::set_current_dir(tmpdir.path()).expect("chdir to tempdir");

        let result = OAuthClient::load();

        std::env::set_current_dir(original_cwd).expect("restore cwd");
        if let Some(value) = prior {
            std::env::set_var(CLIENT_SECRET_ENV, value);
        }

        let err = result.expect_err("no config source should fail loud");
        assert!(err.to_string().contains(CLIENT_SECRET_ENV));
        assert!(err.to_string().contains("client_secret_path"));
    }

    #[test]
    fn parse_token_response_parses_access_and_refresh() {
        let body = r#"{"access_token":"AT","refresh_token":"RT","expires_in":3600}"#;
        let parsed = parse_token_response(body).expect("valid token response");
        assert_eq!(parsed.access_token, "AT");
        assert_eq!(parsed.refresh_token, Some("RT".to_string()));
        assert_eq!(parsed.expires_in, Some(3600));
    }

    #[test]
    fn parse_token_response_missing_refresh_token_is_none() {
        let body = r#"{"access_token":"AT2","expires_in":60}"#;
        let parsed = parse_token_response(body).expect("valid token response");
        assert_eq!(parsed.access_token, "AT2");
        assert_eq!(parsed.refresh_token, None);
    }

    #[test]
    fn parse_token_response_rejects_garbage() {
        let err = parse_token_response("not json").expect_err("garbage should fail");
        assert!(err.to_string().contains("invalid token response JSON"));
    }

    fn test_client() -> OAuthClient {
        OAuthClient {
            client_id: "id-123".to_string(),
            client_secret: "secret".to_string(),
            auth_uri: "https://accounts.google.com/o/oauth2/auth".to_string(),
            token_uri: "https://oauth2.googleapis.com/token".to_string(),
        }
    }

    #[test]
    fn build_auth_url_contains_required_params() {
        let client = test_client();
        let url = build_auth_url(
            &client,
            "http://127.0.0.1:5555/",
            "state-xyz",
            "https://www.googleapis.com/auth/spreadsheets",
        );
        assert!(url.starts_with("https://accounts.google.com/o/oauth2/auth?"));
        assert!(url.contains("response_type=code"));
        assert!(url.contains("access_type=offline"));
        assert!(url.contains("prompt=consent"));
        assert!(url.contains("client_id=id-123"));
        assert!(url.contains("state=state-xyz"));
        // redirect_uri and scope must be percent-encoded, not raw.
        assert!(url.contains("redirect_uri=http%3A%2F%2F127.0.0.1%3A5555%2F"));
        assert!(url.contains("scope=https%3A%2F%2Fwww.googleapis.com%2Fauth%2Fspreadsheets"));
    }

    #[test]
    fn parse_redirect_query_extracts_code_and_state() {
        let map = parse_redirect_query("GET /?code=abc&state=xyz HTTP/1.1").expect("valid line");
        assert_eq!(map.get("code"), Some(&"abc".to_string()));
        assert_eq!(map.get("state"), Some(&"xyz".to_string()));
    }

    #[test]
    fn parse_redirect_query_decodes_percent_encoding() {
        let map =
            parse_redirect_query("GET /?code=a%2Fb%3Dc&state=x%20y HTTP/1.1").expect("valid line");
        assert_eq!(map.get("code"), Some(&"a/b=c".to_string()));
        assert_eq!(map.get("state"), Some(&"x y".to_string()));
    }

    #[test]
    fn parse_redirect_query_missing_code_is_err() {
        let err = parse_redirect_query("GET /?state=xyz HTTP/1.1").expect_err("missing code");
        assert!(err.to_string().contains("code"));
    }

    #[test]
    fn parse_redirect_query_malformed_line_is_err() {
        let err = parse_redirect_query("").expect_err("empty line");
        assert!(err.to_string().contains("redirect request line"));
    }

    #[test]
    fn redact_token_fields_scrubs_known_keys() {
        let body = r#"{"access_token":"super-secret-value","other":"kept"}"#;
        let redacted = redact_token_fields(body);
        assert!(!redacted.contains("super-secret-value"));
        assert!(redacted.contains("REDACTED"));
        assert!(redacted.contains("kept"));
    }

    /// This is the shape [`post_token_request`] actually redacts once
    /// `http_status_as_error(false)` lets a non-2xx response reach the
    /// `status.is_success()` check instead of short-circuiting as a
    /// `ureq::Error::StatusCode` at `.send_form()` — a realistic Google
    /// `invalid_grant` error body (plus a fake leaked `access_token`
    /// fragment appended, standing in for defense-in-depth against a
    /// token Google's token endpoint isn't documented to echo back).
    /// Asserts the secret is scrubbed while `error_description` — the
    /// diagnostic text this whole fix exists to preserve — survives.
    #[test]
    fn redact_token_fields_preserves_error_description_but_scrubs_leaked_token() {
        let body = r#"{"error":"invalid_grant","error_description":"Token has been expired or revoked.","access_token":"ya29.SECRET"}"#;
        let redacted = redact_token_fields(body);
        assert!(!redacted.contains("ya29.SECRET"));
        assert!(redacted.contains("REDACTED"));
        assert!(redacted.contains("invalid_grant"));
        assert!(redacted.contains("Token has been expired or revoked."));
    }

    #[test]
    fn url_encode_decode_round_trips() {
        let raw = "a b/c=d&e";
        let encoded = url_encode(raw);
        assert_eq!(url_decode(&encoded), raw);
    }

    #[test]
    fn token_cache_path_uses_alias_and_forge_sheet_sync_dir() {
        let path = token_cache_path("spoton-qa");
        assert!(path.ends_with("forge/sheet-sync/spoton-qa.json"));
    }
}
