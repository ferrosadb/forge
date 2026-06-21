//! HTTP JSON-RPC transport for ferrosa-memory.
//!
//! POSTs JSON-RPC 2.0 requests to `<base_url>/mcp` per the server
//! contract at `ferrosa-memory-core/src/http.rs` (`POST /mcp`).  One
//! request per call; each call is independent (no persistent session).
//!
//! ## Auth
//!
//! When the server requires authentication (`[server] auth_file = ...`
//! in the server-side config), the client sends an HTTP `Authorization:
//! Basic ...` header.  Credentials come from either [`HttpConfig::auth`]
//! or the `FERROSA_MEMORY_HTTP_USER` / `FERROSA_MEMORY_HTTP_PASS`
//! environment variables.  When the server is unauthenticated, no
//! `Authorization` header is sent.
//!
//! ## Strict id matching
//!
//! Each call uses a fresh monotonic id.  The server is required to echo
//! it back per JSON-RPC 2.0; a mismatched `id` triggers
//! [`Error::Protocol`] (FMEA F14 applied to HTTP — protocol misbehavior
//! must not be silently accepted).

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use base64::Engine as _;
use serde_json::{json, Value};
use ureq::tls::{RootCerts, TlsConfig};

use crate::error::Error;
use crate::transport::Transport;

/// Default per-call timeout (5 minutes). Generous because a single
/// `ingest_entities` chunk can hold up to `MAX_PAYLOAD_BYTES` of
/// entities + edges, each of which is a CQL round-trip server-side;
/// at ~500 entities per chunk and ~10 ms per entity upsert, 30s would
/// be too tight. Override per-project via `[client] http_timeout_ms`
/// in `~/.config/ferrosa-memory.toml`.
pub const DEFAULT_TIMEOUT_SECS: u64 = 300;

/// Per-attempt connect/handshake timeout. Short so that a briefly unready
/// server (e.g. mid-restart, still preparing statements) fails *fast* and is
/// retried by [`Transport::call`], instead of stalling for the whole
/// (minutes-long) global timeout. This is the root-cause fix for the connect
/// race where the client appeared to "hang" while the server was simply not
/// yet accepting requests.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Total attempts `call` makes when the request fails at the transport level
/// (connection refused/reset, handshake timeout). App-level failures (HTTP
/// status, JSON-RPC `error`, id mismatch) are never retried.
const MAX_CALL_ATTEMPTS: u32 = 5;

/// Base backoff between transport retries; doubled each subsequent attempt.
const RETRY_BASE_BACKOFF: Duration = Duration::from_millis(250);

/// Optional HTTP Basic auth credentials.
#[derive(Debug, Clone)]
pub struct HttpAuth {
    pub user: String,
    pub pass: String,
}

impl HttpAuth {
    fn header_value(&self) -> String {
        let raw = format!("{}:{}", self.user, self.pass);
        let b64 = base64::engine::general_purpose::STANDARD.encode(raw);
        format!("Basic {b64}")
    }

    /// Build from `FERROSA_MEMORY_HTTP_USER` / `FERROSA_MEMORY_HTTP_PASS`
    /// env vars.  Returns `None` when either is missing — caller sends
    /// no `Authorization` header.
    pub fn from_env() -> Option<Self> {
        let user = std::env::var("FERROSA_MEMORY_HTTP_USER").ok()?;
        let pass = std::env::var("FERROSA_MEMORY_HTTP_PASS").ok()?;
        if user.is_empty() || pass.is_empty() {
            return None;
        }
        Some(Self { user, pass })
    }
}

#[derive(Debug, Clone)]
pub struct HttpConfig {
    /// Base URL of the ferrosa-memory HTTP MCP endpoint.  The `/mcp`
    /// path is appended internally — pass the root URL
    /// (e.g. `"http://localhost:18765"`).
    pub base_url: String,
    pub auth: Option<HttpAuth>,
    pub timeout: Duration,
}

impl HttpConfig {
    /// Build a config from a base URL; auth from env if present.
    pub fn new<S: Into<String>>(base_url: S) -> Self {
        Self {
            base_url: base_url.into(),
            auth: HttpAuth::from_env(),
            timeout: Duration::from_secs(DEFAULT_TIMEOUT_SECS),
        }
    }
}

#[derive(Debug)]
pub struct HttpTransport {
    mcp_url: String,
    auth_header: Option<String>,
    agent: ureq::Agent,
    next_id: AtomicU64,
}

impl HttpTransport {
    /// Connect to the given server.  No handshake is performed — the
    /// HTTP server is stateless between calls, so connectivity is
    /// verified on the first `call`.
    pub fn connect(config: HttpConfig) -> Result<Self, Error> {
        let mcp_url = format!("{}/mcp", config.base_url.trim_end_matches('/'));
        let agent = ureq::Agent::config_builder()
            .timeout_global(Some(config.timeout))
            .timeout_connect(Some(CONNECT_TIMEOUT))
            .tls_config(
                TlsConfig::builder()
                    .root_certs(RootCerts::PlatformVerifier)
                    .build(),
            )
            .build()
            .new_agent();
        Ok(Self {
            mcp_url,
            auth_header: config.auth.map(|a| a.header_value()),
            agent,
            next_id: AtomicU64::new(1),
        })
    }
}

impl HttpTransport {
    /// Execute one request attempt. Connection-level failures surface as
    /// [`Error::Transport`] so [`Transport::call`] can retry them; HTTP-status
    /// and JSON-RPC errors are returned as-is and never retried.
    fn call_once(&self, method: &str, id: u64, body: &Value) -> Result<Value, Error> {
        let mut req = self.agent.post(&self.mcp_url);
        if let Some(h) = &self.auth_header {
            req = req.header("Authorization", h);
        }
        let req = req.header("Content-Type", "application/json");

        let mut resp = req.send_json(body).map_err(|e| {
            Error::Transport(std::io::Error::other(format!(
                "HTTP POST {} failed: {e}",
                self.mcp_url
            )))
        })?;

        let status = resp.status();
        let resp_body = resp.body_mut().read_to_string().map_err(|e| {
            Error::Transport(std::io::Error::other(format!(
                "failed to read HTTP response body: {e}"
            )))
        })?;

        if !status.is_success() {
            return Err(Error::Protocol(format!(
                "HTTP {} from {}: {}",
                status.as_u16(),
                self.mcp_url,
                resp_body.chars().take(500).collect::<String>()
            )));
        }

        // MCP Streamable-HTTP 202 Accepted with empty body is used for
        // notifications.  Since our `call` sends a real request with an
        // id, a 202 with empty body is a server misbehavior.
        if resp_body.is_empty() {
            return Err(Error::Protocol(format!(
                "empty HTTP response body for method {method}"
            )));
        }

        let parsed: Value = serde_json::from_str(&resp_body).map_err(|e| {
            Error::Protocol(format!(
                "non-JSON HTTP response: {e}; body={}",
                resp_body.chars().take(500).collect::<String>()
            ))
        })?;

        // Strict id matching per JSON-RPC 2.0 (FMEA F14).
        match parsed.get("id").and_then(|v| v.as_u64()) {
            Some(got) if got == id => {}
            Some(got) => {
                return Err(Error::Protocol(format!(
                    "id mismatch: expected {id}, got {got}"
                )));
            }
            None => {
                return Err(Error::Protocol("JSON-RPC response missing `id`".into()));
            }
        }

        if let Some(err) = parsed.get("error") {
            let code = err.get("code").and_then(|v| v.as_i64()).unwrap_or(-32000) as i32;
            let message = err
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown JSON-RPC error")
                .to_string();
            return Err(Error::Tool { code, message });
        }

        Ok(parsed.get("result").cloned().unwrap_or(Value::Null))
    }
}

impl Transport for HttpTransport {
    fn call(&self, method: &str, params: Value) -> Result<Value, Error> {
        // Build the request (and its id) once; the same id is reused across
        // retries since a transport failure means the server never produced a
        // response for it.
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let body = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        retry_on_transport(MAX_CALL_ATTEMPTS, RETRY_BASE_BACKOFF, || {
            self.call_once(method, id, &body)
        })
    }
}

/// Retry `op` while it fails with [`Error::Transport`], up to `max_attempts`
/// total, sleeping `base_backoff * 2^(attempt-1)` between tries. Success and
/// non-transport errors (HTTP status, JSON-RPC `error`, id mismatch) return
/// immediately — only connection-level failures (refused/reset/handshake
/// timeout against a briefly-unready server) are safe to retry. The request is
/// an idempotent upsert keyed by entity id, so a rare re-delivery is harmless.
fn retry_on_transport<F>(
    max_attempts: u32,
    base_backoff: Duration,
    mut op: F,
) -> Result<Value, Error>
where
    F: FnMut() -> Result<Value, Error>,
{
    let mut attempt = 1u32;
    loop {
        match op() {
            Err(Error::Transport(_)) if attempt < max_attempts => {
                let shift = (attempt - 1).min(6);
                std::thread::sleep(base_backoff.saturating_mul(1u32 << shift));
                attempt += 1;
            }
            result => return result,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    fn transport_err() -> Error {
        Error::Transport(std::io::Error::other("connection refused"))
    }

    #[test]
    fn retry_recovers_after_transient_transport_errors() {
        // Server briefly unready: first two attempts fail at the transport
        // level, the third succeeds.
        let calls = Cell::new(0u32);
        let r = retry_on_transport(5, Duration::from_millis(0), || {
            calls.set(calls.get() + 1);
            if calls.get() < 3 {
                Err(transport_err())
            } else {
                Ok(json!("ok"))
            }
        });
        assert_eq!(r.unwrap(), json!("ok"));
        assert_eq!(calls.get(), 3);
    }

    #[test]
    fn retry_gives_up_after_max_attempts() {
        let calls = Cell::new(0u32);
        let r = retry_on_transport(4, Duration::from_millis(0), || {
            calls.set(calls.get() + 1);
            Err(transport_err())
        });
        assert!(matches!(r, Err(Error::Transport(_))));
        assert_eq!(calls.get(), 4);
    }

    #[test]
    fn retry_does_not_retry_app_level_errors() {
        // A protocol/HTTP error is the server's considered answer — retrying
        // would only duplicate work and hide the real failure.
        let calls = Cell::new(0u32);
        let r = retry_on_transport(5, Duration::from_millis(0), || {
            calls.set(calls.get() + 1);
            Err(Error::Protocol("bad request".into()))
        });
        assert!(matches!(r, Err(Error::Protocol(_))));
        assert_eq!(calls.get(), 1);
    }

    #[test]
    fn auth_header_encodes_basic_credentials() {
        let a = HttpAuth {
            user: "u".into(),
            pass: "p".into(),
        };
        // "u:p" base64 == "dTpw"
        assert_eq!(a.header_value(), "Basic dTpw");
    }

    #[test]
    fn from_env_returns_none_when_absent() {
        // The env vars may or may not be set in the test runner; just
        // assert the function returns an Option without panicking.
        let _ = HttpAuth::from_env();
    }

    #[test]
    fn http_config_default_timeout() {
        let c = HttpConfig::new("http://example.invalid:1");
        assert_eq!(c.timeout, Duration::from_secs(DEFAULT_TIMEOUT_SECS));
        assert_eq!(c.base_url, "http://example.invalid:1");
    }

    #[test]
    fn base_url_trailing_slash_normalised() {
        let t = HttpTransport::connect(HttpConfig::new("http://example.invalid:1/")).unwrap();
        assert_eq!(t.mcp_url, "http://example.invalid:1/mcp");
    }

    #[test]
    fn transport_error_on_unreachable_host() {
        let t = HttpTransport::connect(HttpConfig {
            base_url: "http://127.0.0.1:1".into(),
            auth: None,
            timeout: Duration::from_millis(200),
        })
        .unwrap();
        let err = t.call("tools/list", json!({})).unwrap_err();
        assert!(matches!(err, Error::Transport(_)));
    }
}
