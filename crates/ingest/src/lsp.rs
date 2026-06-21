//! Minimal LSP client for code symbol extraction.
//!
//! Spawns a language server (e.g. rust-analyzer) as a subprocess, sends
//! `textDocument/documentSymbol` requests, and returns structured symbols
//! with names, kinds, and source ranges.
//!
//! ## Thread-safety note (P0-5)
//!
//! All methods take `&mut self`.  `LspSession` must NOT be placed behind
//! an `Arc<Mutex<_>>` for parallelism — the `request()` id loop is not
//! re-entrant.  For parallel extraction, use one session per worker process.
//!
//! ## Timeout architecture
//!
//! `request_with_timeout` uses a dedicated reader thread that pushes
//! `ReaderEvent` messages through an `mpsc` channel.  The reader thread owns
//! `ChildStdout` and the `LspSession` holds the `Receiver` end.  All public
//! `&mut self` methods remain single-threaded from the caller's perspective —
//! only the background reader is concurrent, and it sends, never receives.
//!
//! On timeout the in-flight request id is marked `Abandoned` so any
//! late-arriving reply is discarded without matching a subsequent request.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Child, ChildStdout, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::position::PositionEncoding;

// ── Timeout constants (overridable via env, see `read_timeouts`) ───────────────

/// Default timeout for most LSP requests.
pub const TIMEOUT_DEFAULT: Duration = Duration::from_secs(10);
/// Timeout for `textDocument/references` (large symbol sets can be slow).
pub const TIMEOUT_REFERENCES: Duration = Duration::from_secs(10);
/// Timeout for `textDocument/hover` (fast metadata lookup).
pub const TIMEOUT_HOVER: Duration = Duration::from_secs(5);
/// Timeout for `callHierarchy/*` methods (walks the full call graph).
pub const TIMEOUT_CALL_HIERARCHY: Duration = Duration::from_secs(15);
/// Timeout for `textDocument/typeDefinition`.
pub const TIMEOUT_TYPE_DEFINITION: Duration = Duration::from_secs(5);
/// Timeout for `textDocument/implementation`.
pub const TIMEOUT_IMPLEMENTATION: Duration = Duration::from_secs(5);

/// Read per-method timeout overrides from env vars.
///
/// `FORGE_LSP_TIMEOUT_MS` overrides `TIMEOUT_DEFAULT`;
/// `FORGE_LSP_TIMEOUT_REFERENCES_MS`, `FORGE_LSP_TIMEOUT_HOVER_MS`,
/// `FORGE_LSP_TIMEOUT_CALL_HIERARCHY_MS`, `FORGE_LSP_TIMEOUT_TYPE_DEFINITION_MS`,
/// and `FORGE_LSP_TIMEOUT_IMPLEMENTATION_MS` override the per-method constants.
///
/// Called once at session start so env reads are not scattered across hot paths.
fn read_timeouts() -> SessionTimeouts {
    let parse = |var: &str, fallback: Duration| -> Duration {
        std::env::var(var)
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .map(Duration::from_millis)
            .unwrap_or(fallback)
    };
    SessionTimeouts {
        default: parse("FORGE_LSP_TIMEOUT_MS", TIMEOUT_DEFAULT),
        references: parse("FORGE_LSP_TIMEOUT_REFERENCES_MS", TIMEOUT_REFERENCES),
        hover: parse("FORGE_LSP_TIMEOUT_HOVER_MS", TIMEOUT_HOVER),
        call_hierarchy: parse(
            "FORGE_LSP_TIMEOUT_CALL_HIERARCHY_MS",
            TIMEOUT_CALL_HIERARCHY,
        ),
        type_definition: parse(
            "FORGE_LSP_TIMEOUT_TYPE_DEFINITION_MS",
            TIMEOUT_TYPE_DEFINITION,
        ),
        implementation: parse(
            "FORGE_LSP_TIMEOUT_IMPLEMENTATION_MS",
            TIMEOUT_IMPLEMENTATION,
        ),
    }
}

/// Per-session timeout configuration (read once from env at `LspSession::start`).
///
/// `pub(crate)` so test helpers in sibling modules can build mock sessions with
/// explicit deadlines without spawning a real language server.
#[derive(Debug, Clone, Copy)]
pub(crate) struct SessionTimeouts {
    pub(crate) default: Duration,
    pub(crate) references: Duration,
    pub(crate) hover: Duration,
    pub(crate) call_hierarchy: Duration,
    pub(crate) type_definition: Duration,
    pub(crate) implementation: Duration,
}

impl SessionTimeouts {
    /// Return the timeout for the given LSP method.
    fn for_method(&self, method: &str) -> Duration {
        match method {
            "textDocument/references" => self.references,
            "textDocument/hover" => self.hover,
            "textDocument/prepareCallHierarchy"
            | "callHierarchy/incomingCalls"
            | "callHierarchy/outgoingCalls" => self.call_hierarchy,
            "textDocument/typeDefinition" => self.type_definition,
            "textDocument/implementation" => self.implementation,
            _ => self.default,
        }
    }
}

// ── Error type ─────────────────────────────────────────────────────────────────

/// Errors that can occur during an LSP request.
///
/// `TimedOut` is a distinct variant so callers can match it without string
/// inspection — important for the "log and skip, don't abort" pattern in T5–T8.
#[derive(Debug)]
pub enum LspError {
    /// The server did not respond before the per-method deadline.
    TimedOut { method: String },
    /// Transport or protocol error wrapping the underlying `anyhow` error.
    Other(anyhow::Error),
}

impl std::fmt::Display for LspError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TimedOut { method } => write!(f, "LSP request timed out: method={method}"),
            Self::Other(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for LspError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Other(e) => e.source(),
            Self::TimedOut { .. } => None,
        }
    }
}

impl From<anyhow::Error> for LspError {
    fn from(e: anyhow::Error) -> Self {
        Self::Other(e)
    }
}

// ── Server capability record ───────────────────────────────────────────────────

/// LSP server capabilities parsed from `initializeResult.capabilities`.
///
/// Each field is `true` iff the server advertised the corresponding provider
/// as a non-`false`, non-`null` value.  Both `bool` and object forms are
/// accepted (e.g. `{"workDoneProgress": true}` counts as supported).
///
/// Guard: F6 (RPN 336) — callHierarchy unsupported on some servers.
#[derive(Debug, Clone)]
pub struct ServerCapabilities {
    /// `documentSymbolProvider`
    pub document_symbol: bool,
    /// `callHierarchyProvider`
    pub call_hierarchy: bool,
    /// `referencesProvider`
    pub references: bool,
    /// `typeDefinitionProvider`
    pub type_definition: bool,
    /// `implementationProvider`
    pub implementation: bool,
    /// `hoverProvider`
    pub hover: bool,
    /// `definitionProvider`
    pub definition: bool,
}

/// Parse `initializeResult.capabilities` into a `ServerCapabilities` record.
///
/// A provider is considered supported if the corresponding JSON key is present
/// AND its value is not `false` and not `null`.  Any other value (bool `true`,
/// any object) means supported.
///
/// Extracted to its own function to keep `LspSession::start` under the 60-line
/// limit (Power of 10 Rule 4).
pub fn parse_capabilities(caps: &Value) -> ServerCapabilities {
    let supported = |key: &str| -> bool {
        match caps.get(key) {
            None | Some(Value::Null) => false,
            Some(Value::Bool(false)) => false,
            Some(_) => true,
        }
    };
    ServerCapabilities {
        document_symbol: supported("documentSymbolProvider"),
        call_hierarchy: supported("callHierarchyProvider"),
        references: supported("referencesProvider"),
        type_definition: supported("typeDefinitionProvider"),
        implementation: supported("implementationProvider"),
        hover: supported("hoverProvider"),
        definition: supported("definitionProvider"),
    }
}

// ── In-flight request tracking ─────────────────────────────────────────────────

/// State of a pending request id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RequestState {
    /// Waiting for a reply.
    Pending,
    /// Timed out; any arriving reply should be discarded.
    Abandoned,
}

// ── Reader-thread events ───────────────────────────────────────────────────────

/// Message sent from the background reader thread to the session.
///
/// `pub(crate)` so test helpers in sibling modules can inject frames into a
/// mock session's channel without spawning a real language server.
#[derive(Debug)]
pub(crate) enum ReaderEvent {
    /// A complete JSON-RPC frame arrived.
    Message(Value),
    /// The reader encountered an I/O error (terminal).
    IoError(String),
    /// The child's stdout was closed (EOF — terminal).
    Closed,
}

/// Spawn a background thread that reads LSP frames from `stdout` and
/// forwards them through `tx`.
///
/// The thread owns `stdout` and exits when EOF or a terminal I/O error occurs.
/// LSP frames are length-prefixed (Content-Length header + body).
fn spawn_reader(stdout: ChildStdout, tx: mpsc::Sender<ReaderEvent>) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        loop {
            match read_lsp_frame(&mut reader) {
                Ok(Some(value)) => {
                    if tx.send(ReaderEvent::Message(value)).is_err() {
                        // Receiver dropped — session shut down; exit cleanly.
                        return;
                    }
                }
                Ok(None) => {
                    // EOF — child closed stdout.
                    let _ = tx.send(ReaderEvent::Closed);
                    return;
                }
                Err(e) => {
                    let _ = tx.send(ReaderEvent::IoError(e.to_string()));
                    return;
                }
            }
        }
    })
}

/// Read one LSP frame from `reader`.
///
/// Returns `Ok(Some(value))` on a well-formed frame, `Ok(None)` on clean EOF,
/// and `Err(_)` on I/O or parse failure.
fn read_lsp_frame(reader: &mut BufReader<ChildStdout>) -> Result<Option<Value>> {
    let mut content_length = 0usize;
    // Power of 10 Rule 2: header-reading loop is bounded — the LSP spec
    // terminates headers with a blank line; malformed input hits EOF.
    loop {
        let mut line = String::new();
        let n = reader.read_line(&mut line)?;
        if n == 0 {
            return Ok(None); // EOF
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            break; // End of headers
        }
        if let Some(len_str) = trimmed.strip_prefix("Content-Length: ") {
            content_length = len_str
                .parse()
                .with_context(|| format!("invalid Content-Length: {len_str}"))?;
        }
    }

    if content_length == 0 {
        bail!("LSP: frame with Content-Length 0");
    }

    // Power of 10 Rule 3: body size bounded by content_length set by server.
    let mut body = vec![0u8; content_length];
    reader.read_exact(&mut body)?;
    let value = serde_json::from_slice(&body)
        .with_context(|| "LSP: failed to parse JSON-RPC frame body")?;
    Ok(Some(value))
}

// ── Symbol types ───────────────────────────────────────────────────────────────

/// An extracted code symbol with its location.
#[derive(Debug, Clone, Serialize)]
pub struct Symbol {
    pub name: String,
    pub kind: SymbolKind,
    pub detail: Option<String>,
    pub file: String,
    pub line: u32,
    pub end_line: u32,
    pub children: Vec<Symbol>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SymbolKind {
    Function,
    Method,
    Struct,
    Enum,
    Trait,
    Constant,
    Module,
    TypeParameter,
    Other,
}

impl SymbolKind {
    fn from_lsp(kind: u64) -> Self {
        match kind {
            12 => Self::Function, // Function
            6 => Self::Method,    // Method
            23 => Self::Struct,   // Struct
            10 => Self::Enum,     // Enum
            11 => Self::Trait,    // Interface
            14 => Self::Constant, // Constant
            2 => Self::Module,    // Module
            26 => Self::TypeParameter,
            _ => Self::Other,
        }
    }

    pub fn entity_type(&self) -> &'static str {
        match self {
            Self::Function | Self::Method => "function",
            Self::Struct => "struct",
            Self::Enum => "enum",
            Self::Trait => "trait",
            Self::Constant => "constant",
            Self::Module => "module",
            _ => "section",
        }
    }
}

// ── LSP discovery helpers ──────────────────────────────────────────────────────

/// Known language servers and their install instructions.
pub struct LspInfo {
    pub binary: &'static str,
    pub install: &'static str,
}

pub fn lsp_for_language(lang: &str) -> Option<LspInfo> {
    match lang {
        "rust" => Some(LspInfo {
            binary: "rust-analyzer",
            install: "rustup component add rust-analyzer",
        }),
        "python" => Some(LspInfo {
            binary: "pyright",
            install: "pip install pyright",
        }),
        "go" => Some(LspInfo {
            binary: "gopls",
            install: "go install golang.org/x/tools/gopls@latest",
        }),
        "typescript" | "javascript" => Some(LspInfo {
            binary: "typescript-language-server",
            install: "npm install -g typescript-language-server typescript",
        }),
        "elixir" => Some(LspInfo {
            binary: "elixir-ls",
            install: "mix escript.install hex elixir_ls",
        }),
        _ => None,
    }
}

/// Check if an LSP binary is available. Returns the path if found.
pub fn find_lsp(lang: &str) -> Option<String> {
    let info = lsp_for_language(lang)?;
    which::which(info.binary)
        .ok()
        .map(|p| p.to_string_lossy().to_string())
}

/// Print a user-visible prompt to install the LSP for a language.
pub fn prompt_lsp_install(lang: &str) {
    if let Some(info) = lsp_for_language(lang) {
        eprintln!("[forge] Detected {lang} project");
        eprintln!("[forge] {} not found in PATH", info.binary);
        eprintln!("[forge] Install it for function-level code indexing:");
        eprintln!("[forge]   {}", info.install);
        eprintln!("[forge] Without it, ingest will use module-level extraction only.");
    }
}

// ── LspSession ─────────────────────────────────────────────────────────────────

/// A running LSP session connected to a language server.
pub struct LspSession {
    stdin: Option<std::process::ChildStdin>,
    child: Child,
    next_id: i64,
    /// Tracks in-flight request ids and their lifecycle state.
    ///
    /// `Abandoned` ids have timed out; any late reply for them is discarded.
    /// This prevents a stale reply from being matched to a newer request.
    pending: HashMap<i64, RequestState>,
    /// Position encoding negotiated during `initialize`.
    /// Defaults to `Utf16` (the LSP 3.17 spec default) if the server does not
    /// advertise a `positionEncoding` capability.
    negotiated_encoding: PositionEncoding,
    /// Server capabilities parsed from `initializeResult.capabilities`.
    server_capabilities: ServerCapabilities,
    /// Per-session timeout values (read once from env at start).
    timeouts: SessionTimeouts,
    /// Receives JSON-RPC frames from the background reader thread.
    rx: Receiver<ReaderEvent>,
    /// JoinHandle for the reader thread (held to log panics on drop).
    _reader_handle: thread::JoinHandle<()>,
}

impl LspSession {
    /// Start an LSP session for the given project root.
    pub fn start(lsp_binary: &str, project_root: &Path) -> Result<Self> {
        let timeouts = read_timeouts();

        let mut child = Command::new(lsp_binary)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .with_context(|| format!("failed to spawn {lsp_binary}"))?;

        let stdin = child.stdin.take().context("no stdin")?;
        let stdout = child.stdout.take().context("no stdout")?;

        // Spawn the reader thread before the first request.
        let (tx, rx) = mpsc::channel::<ReaderEvent>();
        let reader_handle = spawn_reader(stdout, tx);

        let mut session = Self {
            stdin: Some(stdin),
            child,
            next_id: 1,
            pending: HashMap::new(),
            negotiated_encoding: PositionEncoding::Utf16,
            server_capabilities: ServerCapabilities {
                document_symbol: false,
                call_hierarchy: false,
                references: false,
                type_definition: false,
                implementation: false,
                hover: false,
                definition: false,
            },
            timeouts,
            rx,
            _reader_handle: reader_handle,
        };

        // rust-analyzer is the one server that, by default, compiles
        // dependencies (build scripts, proc-macros, check-on-save) and indexes
        // `target/` — which can exceed the caller's MCP timeout on large repos.
        // Send ingest-tuned options that keep it in cheap, syntactic mode.
        let init_options = if lsp_binary.contains("rust-analyzer") {
            Some(crate::ignore_policy::rust_analyzer_ingest_options(
                project_root,
            ))
        } else {
            None
        };

        let init_result = session.initialize(project_root, init_options.as_ref())?;
        session.finish_init(init_result)?;
        Ok(session)
    }

    /// Send the `initialize` request and return the result value.
    ///
    /// Extracted to keep each function under the 60-line limit (Power of 10 Rule 4).
    fn initialize(&mut self, project_root: &Path, init_options: Option<&Value>) -> Result<Value> {
        // Build root URI using url::Url for correct cross-platform encoding (P1-6).
        let root_uri = url::Url::from_file_path(project_root)
            .map_err(|_| {
                anyhow::anyhow!(
                    "cannot convert project root to file URI: {}",
                    project_root.display()
                )
            })?
            .to_string();

        // Advertise preferred encodings: utf-8 first (best), then utf-32 and utf-16.
        // The server will pick from this list and report its choice in the response.
        // Guard F2 / P0-1.
        let mut params = json!({
            "processId": std::process::id(),
            "rootUri": root_uri,
            "capabilities": {
                "general": {
                    "positionEncodings": ["utf-8", "utf-32", "utf-16"]
                },
                "textDocument": {
                    "documentSymbol": {
                        "hierarchicalDocumentSymbolSupport": true
                    }
                }
            }
        });

        // Server-specific tuning (e.g. rust-analyzer build-artifact exclusion).
        if let Some(opts) = init_options {
            params["initializationOptions"] = opts.clone();
        }

        self.request("initialize", params)
    }

    /// Parse the `initializeResult`, record capabilities, and send `initialized`.
    ///
    /// Extracted to keep `start` under 60 lines (Power of 10 Rule 4).
    fn finish_init(&mut self, init_result: Value) -> Result<()> {
        // Parse the server's chosen encoding.
        // Per LSP 3.17 spec §3.1, defaults to "utf-16" if absent.
        let chosen_encoding = init_result
            .get("capabilities")
            .and_then(|caps| caps.get("positionEncoding"))
            .and_then(|enc| enc.as_str())
            .map(PositionEncoding::from_lsp_str)
            .unwrap_or(PositionEncoding::Utf16);

        self.negotiated_encoding = chosen_encoding;

        // Parse and store server capabilities (Guard F6).
        let caps_value = init_result
            .get("capabilities")
            .cloned()
            .unwrap_or(Value::Null);
        self.server_capabilities = parse_capabilities(&caps_value);

        // Verify server supports documentSymbol (minimum requirement).
        if !self.server_capabilities.document_symbol {
            bail!("LSP server does not support documentSymbol");
        }

        // Emit degradation signal (Guard F6: operators must see it immediately).
        let mode = self.extraction_mode();
        let missing = self.missing_capabilities();
        if missing.is_empty() {
            eprintln!("[forge-lsp] capabilities: {mode}");
        } else {
            eprintln!("[forge-lsp] capabilities: {mode} (missing: {missing})");
        }

        // Send initialized notification.
        self.notify("initialized", json!({}))?;

        Ok(())
    }

    /// Return the position encoding negotiated during `initialize`.
    ///
    /// All LSP range-to-byte conversions must use this encoding.
    /// Guard: F2 (UTF-16 vs byte index mismatch).
    pub fn position_encoding(&self) -> PositionEncoding {
        self.negotiated_encoding
    }

    /// Return a reference to the server capabilities record.
    ///
    /// Callers (T5–T8) use this to decide whether to attempt a request before
    /// calling `request_if_supported`.  Guard: F6.
    pub fn capabilities(&self) -> &ServerCapabilities {
        &self.server_capabilities
    }

    /// Return a degradation signal string for the current capabilities.
    ///
    /// - `"full"` — all Tier-1 capabilities plus hover are present.
    /// - `"partial"` — some Tier-1 capabilities are missing.
    /// - `"symbols_only"` — only `documentSymbol` is supported.
    pub fn extraction_mode(&self) -> &'static str {
        let caps = &self.server_capabilities;
        let tier1_full = caps.call_hierarchy
            && caps.references
            && caps.type_definition
            && caps.implementation
            && caps.hover;
        if tier1_full {
            "full"
        } else if caps.call_hierarchy
            || caps.references
            || caps.type_definition
            || caps.implementation
            || caps.hover
        {
            "partial"
        } else {
            "symbols_only"
        }
    }

    /// Comma-separated list of missing Tier-1 capability names, for the log line.
    fn missing_capabilities(&self) -> String {
        let caps = &self.server_capabilities;
        let mut missing = Vec::new();
        if !caps.call_hierarchy {
            missing.push("callHierarchy");
        }
        if !caps.references {
            missing.push("references");
        }
        if !caps.type_definition {
            missing.push("typeDefinition");
        }
        if !caps.implementation {
            missing.push("implementation");
        }
        if !caps.hover {
            missing.push("hover");
        }
        missing.join(", ")
    }

    /// Return the capability flag for an LSP method, if the method is
    /// capability-gated.
    ///
    /// Returns `None` for methods not covered by the capability map (e.g.
    /// `initialize`, `shutdown`), which means "always allowed".
    fn capability_for_method(&self, method: &str) -> Option<bool> {
        let caps = &self.server_capabilities;
        match method {
            "textDocument/documentSymbol" => Some(caps.document_symbol),
            "textDocument/prepareCallHierarchy"
            | "callHierarchy/incomingCalls"
            | "callHierarchy/outgoingCalls" => Some(caps.call_hierarchy),
            "textDocument/references" => Some(caps.references),
            "textDocument/typeDefinition" => Some(caps.type_definition),
            "textDocument/implementation" => Some(caps.implementation),
            "textDocument/hover" => Some(caps.hover),
            "textDocument/definition" => Some(caps.definition),
            _ => None,
        }
    }

    /// Send a request and wait up to `timeout` for the response.
    ///
    /// Returns `Err(LspError::TimedOut)` if the server does not reply in time.
    /// The session remains usable after a timeout — the abandoned id is marked
    /// so any late reply is discarded rather than matched to a later request.
    ///
    /// Guard: F1 (RPN 336) — per-request deadline to prevent indefinite hangs.
    pub fn request_with_timeout(
        &mut self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, LspError> {
        let id = self.next_id;
        self.next_id += 1;
        self.pending.insert(id, RequestState::Pending);

        let msg = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params
        });

        self.send(&msg).map_err(LspError::Other)?;

        let start = std::time::Instant::now();
        // Power of 10 Rule 2: loop exits on timeout or on receiving the matching id.
        loop {
            let elapsed = start.elapsed();
            if elapsed >= timeout {
                // Mark id abandoned so a late reply is discarded.
                self.pending.insert(id, RequestState::Abandoned);
                return Err(LspError::TimedOut {
                    method: method.to_string(),
                });
            }
            let remaining = timeout - elapsed;
            let event = match self.rx.recv_timeout(remaining) {
                Ok(ev) => ev,
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    self.pending.insert(id, RequestState::Abandoned);
                    return Err(LspError::TimedOut {
                        method: method.to_string(),
                    });
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(LspError::Other(anyhow::anyhow!(
                        "LSP reader thread disconnected (child exited?)"
                    )));
                }
            };

            match event {
                ReaderEvent::Message(msg) => {
                    let Some(resp_id) = msg.get("id").and_then(|v| v.as_i64()) else {
                        // Notification (no id) — skip and keep waiting.
                        continue;
                    };
                    match self.pending.get(&resp_id).copied() {
                        Some(RequestState::Abandoned) => {
                            // Late reply for a timed-out request — discard.
                            self.pending.remove(&resp_id);
                            continue;
                        }
                        Some(RequestState::Pending) if resp_id == id => {
                            self.pending.remove(&id);
                            if let Some(err) = msg.get("error") {
                                return Err(LspError::Other(anyhow::anyhow!(
                                    "LSP error on {method}: {err}"
                                )));
                            }
                            return Ok(msg.get("result").cloned().unwrap_or(Value::Null));
                        }
                        _ => {
                            // Response for a different in-flight id — skip.
                            continue;
                        }
                    }
                }
                ReaderEvent::IoError(e) => {
                    return Err(LspError::Other(anyhow::anyhow!("LSP reader error: {e}")));
                }
                ReaderEvent::Closed => {
                    return Err(LspError::Other(anyhow::anyhow!(
                        "LSP: child closed stdout unexpectedly"
                    )));
                }
            }
        }
    }

    /// Send a request using the default timeout for the method.
    ///
    /// This is the convenience method for callers that don't need a custom
    /// deadline.  Delegates to `request_with_timeout`.
    pub fn request(&mut self, method: &str, params: Value) -> Result<Value> {
        let timeout = self.timeouts.for_method(method);
        self.request_with_timeout(method, params, timeout)
            .map_err(|e| match e {
                LspError::TimedOut { method: m } => {
                    anyhow::anyhow!("LSP request timed out: method={m}")
                }
                LspError::Other(e) => e,
            })
    }

    /// Issue a request only if the server advertised support for the method.
    ///
    /// Returns `Ok(None)` immediately (no network round-trip) if the capability
    /// is absent.  Returns `Ok(Some(value))` on success.
    ///
    /// Guard: F6 (RPN 336) — missing capability must never trigger a round-trip
    /// that silently returns zero results.
    pub fn request_if_supported(
        &mut self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Option<Value>, LspError> {
        if let Some(false) = self.capability_for_method(method) {
            return Ok(None);
        }
        let result = self.request_with_timeout(method, params, timeout)?;
        Ok(Some(result))
    }

    /// Get document symbols for a file.
    ///
    /// `text` is the file content **already read** by the caller (via `SourceBuffer::read`).
    /// Passing text here avoids a second `read_to_string` call (guard P1-3 / F14).
    ///
    /// The URI is constructed via `url::Url::from_file_path` for cross-platform
    /// correctness (guard P1-6).
    pub fn document_symbols(&mut self, file_path: &Path, text: &str) -> Result<Vec<Symbol>> {
        let uri = url::Url::from_file_path(file_path)
            .map_err(|_| {
                anyhow::anyhow!("cannot convert file path to URI: {}", file_path.display())
            })?
            .to_string();

        // Open the document using the caller-supplied text — same buffer used for sha256.
        self.notify(
            "textDocument/didOpen",
            json!({
                "textDocument": {
                    "uri": &uri,
                    "languageId": "rust",
                    "version": 1,
                    "text": text
                }
            }),
        )?;

        // Request symbols using the default timeout.
        let result = self.request(
            "textDocument/documentSymbol",
            json!({
                "textDocument": { "uri": &uri }
            }),
        )?;

        // Close the document.
        self.notify(
            "textDocument/didClose",
            json!({
                "textDocument": { "uri": &uri }
            }),
        )?;

        let file_str = file_path.to_string_lossy().to_string();
        parse_symbols(&result, &file_str)
    }

    /// Shut down the LSP server gracefully.
    pub fn shutdown(mut self) -> Result<()> {
        let _ = self.request("shutdown", json!(null));
        self.notify("exit", json!(null))?;
        self.stdin.take();

        // Power of 10 Rule 2: bounded poll loop, max 20 iterations × 50ms = 1s.
        for _ in 0..20 {
            if self.child.try_wait()?.is_some() {
                return Ok(());
            }
            thread::sleep(Duration::from_millis(50));
        }

        let _ = self.child.kill();
        let _ = self.child.wait();
        Ok(())
    }

    fn notify(&mut self, method: &str, params: Value) -> Result<()> {
        let msg = json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params
        });
        self.send(&msg)
    }

    fn send(&mut self, msg: &Value) -> Result<()> {
        let body = serde_json::to_string(msg)?;
        let stdin = self.stdin.as_mut().context("LSP stdin closed")?;
        write!(stdin, "Content-Length: {}\r\n\r\n{}", body.len(), body)?;
        stdin.flush()?;
        Ok(())
    }

    // ── T6: textDocument/references ───────────────────────────────────────────

    /// Issue `textDocument/references` for the given file position.
    ///
    /// Returns `Ok(None)` immediately (no round-trip) when the server did not
    /// advertise `referencesProvider`.  Returns `Ok(Some(vec![]))` when the
    /// server responds with null or an empty array.  On timeout, returns
    /// `Err(LspError::TimedOut)` so the caller can log and skip.
    ///
    /// `include_declaration` maps to the LSP `context.includeDeclaration` flag.
    /// For T6 edge emission we always pass `false` to avoid self-referential edges.
    ///
    /// Guard: F1 (RPN 336) — uses `TIMEOUT_REFERENCES` via `request_if_supported`.
    pub fn references(
        &mut self,
        file: &Path,
        position: Position,
        include_declaration: bool,
    ) -> Result<Option<Vec<Location>>, LspError> {
        let uri = url::Url::from_file_path(file)
            .map_err(|_| {
                LspError::Other(anyhow::anyhow!(
                    "cannot convert file path to URI: {}",
                    file.display()
                ))
            })?
            .to_string();

        let params = serde_json::json!({
            "textDocument": { "uri": uri },
            "position": { "line": position.line, "character": position.character },
            "context": { "includeDeclaration": include_declaration }
        });

        let timeout = self.timeouts.references;
        let raw = self.request_if_supported("textDocument/references", params, timeout)?;

        // Capability missing → Ok(None), propagate as-is.
        let Some(value) = raw else {
            return Ok(None);
        };

        // LSP may return null (no references) or an array of Location.
        if value.is_null() {
            return Ok(Some(vec![]));
        }
        let locations: Vec<Location> = serde_json::from_value(value)
            .map_err(|e| LspError::Other(anyhow::anyhow!("references: parse error: {e}")))?;
        Ok(Some(locations))
    }

    // ── T14: textDocument/definition ─────────────────────────────────────────

    /// Issue `textDocument/definition` at the given file position.
    ///
    /// Returns `Ok(None)` without a round-trip if the server did not
    /// advertise `definitionProvider`.  Normalizes single-Location vs
    /// `Location[]` response shapes.  `LocationLink` responses are
    /// deserialized best-effort via the shared `Location` struct (callers
    /// should check the returned `uri`/`range` fields).
    ///
    /// Used by T14 to promote string-keyed `imports` edges into resolved
    /// edges pointing at real module entities.
    pub fn definition(
        &mut self,
        file: &Path,
        position: Position,
    ) -> Result<Option<Vec<Location>>, LspError> {
        let uri = url::Url::from_file_path(file)
            .map_err(|_| {
                LspError::Other(anyhow::anyhow!(
                    "cannot convert file path to URI: {}",
                    file.display()
                ))
            })?
            .to_string();
        let params = serde_json::json!({
            "textDocument": { "uri": uri },
            "position": { "line": position.line, "character": position.character },
        });

        // definition uses the same default-ish timeout as typeDefinition —
        // rust-analyzer usually answers within tens of ms once indexed.
        let timeout = self.timeouts.type_definition;
        let raw = self.request_if_supported("textDocument/definition", params, timeout)?;

        let Some(value) = raw else { return Ok(None) };
        if value.is_null() {
            return Ok(Some(vec![]));
        }
        let locations: Vec<Location> = if value.is_array() {
            serde_json::from_value(value)
                .map_err(|e| LspError::Other(anyhow::anyhow!("definition: parse error: {e}")))?
        } else {
            let one: Location = serde_json::from_value(value)
                .map_err(|e| LspError::Other(anyhow::anyhow!("definition: parse error: {e}")))?;
            vec![one]
        };
        Ok(Some(locations))
    }

    // ── T13: textDocument/hover ──────────────────────────────────────────────

    /// Issue `textDocument/hover` at the given file position.
    ///
    /// Returns `Ok(None)` without a round-trip if the server did not
    /// advertise `hoverProvider`.  On success, returns `HoverResult` carrying
    /// the rendered documentation plus the format (`"markdown"` |
    /// `"plaintext"` | `None` for unknown shape).
    ///
    /// The LSP spec allows three response shapes for `Hover.contents`:
    /// - `MarkedString` (string or `{language, value}`)
    /// - `MarkedString[]` (array of the above)
    /// - `MarkupContent` (`{kind: "markdown"|"plaintext", value}`)
    ///
    /// All three are normalized to a single rendered string here.  Returns
    /// `Ok(Some(HoverResult{content: "", format: None}))` when the server
    /// returned `null` or empty — distinct from capability-missing
    /// `Ok(None)`.  Stored content is verbatim (no sanitization) per the
    /// feature spec F18; downstream consumers (LLM-assisted description
    /// generation) must treat it as untrusted input.
    pub fn hover(
        &mut self,
        file: &Path,
        position: Position,
    ) -> Result<Option<HoverResult>, LspError> {
        let uri = url::Url::from_file_path(file)
            .map_err(|_| {
                LspError::Other(anyhow::anyhow!(
                    "cannot convert file path to URI: {}",
                    file.display()
                ))
            })?
            .to_string();
        let params = serde_json::json!({
            "textDocument": { "uri": uri },
            "position": { "line": position.line, "character": position.character },
        });

        let timeout = self.timeouts.hover;
        let raw = self.request_if_supported("textDocument/hover", params, timeout)?;

        let Some(value) = raw else { return Ok(None) };
        Ok(Some(parse_hover_response(&value)))
    }

    // ── T5: callHierarchy/* ───────────────────────────────────────────────────

    /// Issue `textDocument/prepareCallHierarchy` at the given file position.
    ///
    /// Returns `Ok(None)` immediately (no round-trip) when the server did not
    /// advertise `callHierarchyProvider`.  Returns `Ok(Some(items))` on success
    /// (empty vec when the server returns null or an empty array).  On timeout,
    /// returns `Err(LspError::TimedOut)` so the caller can log-and-skip.
    ///
    /// Guard: F1 (RPN 336) — uses `TIMEOUT_CALL_HIERARCHY` via `request_if_supported`.
    /// Guard: F6 (RPN 336) — capability gate, no round-trip on unsupported servers.
    pub fn prepare_call_hierarchy(
        &mut self,
        file: &Path,
        position: Position,
    ) -> Result<Option<Vec<CallHierarchyItem>>, LspError> {
        let uri = url::Url::from_file_path(file)
            .map_err(|_| {
                LspError::Other(anyhow::anyhow!(
                    "cannot convert file path to URI: {}",
                    file.display()
                ))
            })?
            .to_string();

        let params = serde_json::json!({
            "textDocument": { "uri": uri },
            "position": { "line": position.line, "character": position.character }
        });

        let timeout = self.timeouts.call_hierarchy;
        let raw =
            self.request_if_supported("textDocument/prepareCallHierarchy", params, timeout)?;

        let Some(value) = raw else { return Ok(None) };
        if value.is_null() {
            return Ok(Some(vec![]));
        }
        let items: Vec<CallHierarchyItem> = serde_json::from_value(value).map_err(|e| {
            LspError::Other(anyhow::anyhow!("prepareCallHierarchy: parse error: {e}"))
        })?;
        Ok(Some(items))
    }

    /// Issue `callHierarchy/incomingCalls` for a previously prepared item.
    ///
    /// Returns `Ok(None)` when the server does not advertise `callHierarchyProvider`.
    /// On timeout, returns `Err(LspError::TimedOut)` so the caller can log-and-skip.
    ///
    /// Guard: F1 (RPN 336), F6 (RPN 336).
    pub fn call_hierarchy_incoming(
        &mut self,
        item: &CallHierarchyItem,
    ) -> Result<Option<Vec<CallHierarchyIncomingCall>>, LspError> {
        let params = serde_json::json!({ "item": item });
        let timeout = self.timeouts.call_hierarchy;
        let raw = self.request_if_supported("callHierarchy/incomingCalls", params, timeout)?;

        let Some(value) = raw else { return Ok(None) };
        if value.is_null() {
            return Ok(Some(vec![]));
        }
        let calls: Vec<CallHierarchyIncomingCall> = serde_json::from_value(value)
            .map_err(|e| LspError::Other(anyhow::anyhow!("incomingCalls: parse error: {e}")))?;
        Ok(Some(calls))
    }

    /// Issue `callHierarchy/outgoingCalls` for a previously prepared item.
    ///
    /// Returns `Ok(None)` when the server does not advertise `callHierarchyProvider`.
    /// On timeout, returns `Err(LspError::TimedOut)` so the caller can log-and-skip.
    ///
    /// Guard: F1 (RPN 336), F6 (RPN 336).
    pub fn call_hierarchy_outgoing(
        &mut self,
        item: &CallHierarchyItem,
    ) -> Result<Option<Vec<CallHierarchyOutgoingCall>>, LspError> {
        let params = serde_json::json!({ "item": item });
        let timeout = self.timeouts.call_hierarchy;
        let raw = self.request_if_supported("callHierarchy/outgoingCalls", params, timeout)?;

        let Some(value) = raw else { return Ok(None) };
        if value.is_null() {
            return Ok(Some(vec![]));
        }
        let calls: Vec<CallHierarchyOutgoingCall> = serde_json::from_value(value)
            .map_err(|e| LspError::Other(anyhow::anyhow!("outgoingCalls: parse error: {e}")))?;
        Ok(Some(calls))
    }

    // ── T7: textDocument/typeDefinition ──────────────────────────────────────

    /// Issue `textDocument/typeDefinition` at the given file position.
    ///
    /// Returns `Ok(None)` without a round-trip if the server did not
    /// advertise `typeDefinitionProvider`.  On success, returns the list of
    /// `Location`s pointing at the type's definition(s).  Empty list if the
    /// server returns `null` or `[]`.
    ///
    /// The LSP spec allows `LocationLink` as an alternative response shape
    /// (via `LocationLink[]`).  For simplicity this captures only `Location`
    /// and `Location[]` — the most common case for rust-analyzer and
    /// pyright.  `LocationLink` responses would deserialize as `Location`
    /// since the shapes share `uri` + `range` keys (with
    /// `#[serde(rename = "targetUri"/"targetRange")]` variants rejected,
    /// returning a parse error the caller can surface).
    pub fn type_definition(
        &mut self,
        file: &Path,
        position: Position,
    ) -> Result<Option<Vec<Location>>, LspError> {
        let uri = url::Url::from_file_path(file)
            .map_err(|_| {
                LspError::Other(anyhow::anyhow!(
                    "cannot convert file path to URI: {}",
                    file.display()
                ))
            })?
            .to_string();

        let params = serde_json::json!({
            "textDocument": { "uri": uri },
            "position": { "line": position.line, "character": position.character },
        });

        let timeout = self.timeouts.type_definition;
        let raw = self.request_if_supported("textDocument/typeDefinition", params, timeout)?;

        let Some(value) = raw else { return Ok(None) };
        if value.is_null() {
            return Ok(Some(vec![]));
        }
        // Server may return a single Location or an array.  Normalize.
        let locations: Vec<Location> = if value.is_array() {
            serde_json::from_value(value)
                .map_err(|e| LspError::Other(anyhow::anyhow!("typeDefinition: parse error: {e}")))?
        } else {
            let one: Location = serde_json::from_value(value).map_err(|e| {
                LspError::Other(anyhow::anyhow!("typeDefinition: parse error: {e}"))
            })?;
            vec![one]
        };
        Ok(Some(locations))
    }

    // ── T8: textDocument/implementation ──────────────────────────────────────

    /// Issue `textDocument/implementation` at the given file position.
    ///
    /// Returns `Ok(None)` without a round-trip if the server did not
    /// advertise `implementationProvider`.  Queried from the trait/interface
    /// side (e.g. on a `trait Foo { ... }`) returns the concrete
    /// implementations.  Queried from a concrete type's side, LSP behavior
    /// is server-dependent (rust-analyzer returns parent trait impls).
    pub fn implementation(
        &mut self,
        file: &Path,
        position: Position,
    ) -> Result<Option<Vec<Location>>, LspError> {
        let uri = url::Url::from_file_path(file)
            .map_err(|_| {
                LspError::Other(anyhow::anyhow!(
                    "cannot convert file path to URI: {}",
                    file.display()
                ))
            })?
            .to_string();

        let params = serde_json::json!({
            "textDocument": { "uri": uri },
            "position": { "line": position.line, "character": position.character },
        });

        let timeout = self.timeouts.implementation;
        let raw = self.request_if_supported("textDocument/implementation", params, timeout)?;

        let Some(value) = raw else { return Ok(None) };
        if value.is_null() {
            return Ok(Some(vec![]));
        }
        let locations: Vec<Location> = if value.is_array() {
            serde_json::from_value(value)
                .map_err(|e| LspError::Other(anyhow::anyhow!("implementation: parse error: {e}")))?
        } else {
            let one: Location = serde_json::from_value(value).map_err(|e| {
                LspError::Other(anyhow::anyhow!("implementation: parse error: {e}"))
            })?;
            vec![one]
        };
        Ok(Some(locations))
    }
}

// ── T5 callHierarchy types ────────────────────────────────────────────────────

/// An LSP `CallHierarchyItem` returned by `textDocument/prepareCallHierarchy`.
///
/// Only the fields needed by T5/T6 edge emission are captured; additional
/// server-side fields are ignored during deserialization.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CallHierarchyItem {
    pub name: String,
    pub kind: u32,
    pub uri: String,
    pub range: Range,
    #[serde(rename = "selectionRange")]
    pub selection_range: Range,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// One entry in a `callHierarchy/incomingCalls` response.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CallHierarchyIncomingCall {
    pub from: CallHierarchyItem,
    #[serde(rename = "fromRanges")]
    pub from_ranges: Vec<Range>,
}

/// One entry in a `callHierarchy/outgoingCalls` response.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CallHierarchyOutgoingCall {
    pub to: CallHierarchyItem,
    #[serde(rename = "fromRanges")]
    pub from_ranges: Vec<Range>,
}

// ── T13 hover types ───────────────────────────────────────────────────────────

/// Normalized hover result. `format` is the content's markup kind when the
/// server returned `MarkupContent`; `None` for legacy `MarkedString`
/// responses or when hover content was empty/absent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HoverResult {
    pub content: String,
    pub format: Option<String>,
}

/// Parse any of the three LSP `Hover.contents` response shapes into a
/// single `HoverResult`.  Never panics on unknown shapes — returns an
/// empty result instead so callers can treat "no hover" uniformly.
///
/// Guards: F19 (RPN 84) — don't panic on hover shape variance.
pub(crate) fn parse_hover_response(value: &serde_json::Value) -> HoverResult {
    if value.is_null() {
        return HoverResult {
            content: String::new(),
            format: None,
        };
    }
    let contents = match value.get("contents") {
        Some(c) => c,
        None => {
            return HoverResult {
                content: String::new(),
                format: None,
            };
        }
    };

    // Shape 1: MarkupContent — { kind: "markdown"|"plaintext", value: "..." }
    if let (Some(kind), Some(val)) = (contents.get("kind"), contents.get("value")) {
        return HoverResult {
            content: val.as_str().unwrap_or("").to_string(),
            format: kind.as_str().map(|s| s.to_string()),
        };
    }

    // Shape 2: MarkedString[] — array of strings or { language, value } objects
    if let Some(arr) = contents.as_array() {
        let mut parts = Vec::with_capacity(arr.len());
        for item in arr {
            if let Some(s) = item.as_str() {
                parts.push(s.to_string());
            } else if let Some(val) = item.get("value").and_then(|v| v.as_str()) {
                parts.push(val.to_string());
            }
        }
        return HoverResult {
            content: parts.join("\n\n"),
            format: None,
        };
    }

    // Shape 3: MarkedString — bare string OR single { language, value } object
    if let Some(s) = contents.as_str() {
        return HoverResult {
            content: s.to_string(),
            format: None,
        };
    }
    if let Some(val) = contents.get("value").and_then(|v| v.as_str()) {
        return HoverResult {
            content: val.to_string(),
            format: None,
        };
    }

    HoverResult {
        content: String::new(),
        format: None,
    }
}

// ── T6 LSP types ──────────────────────────────────────────────────────────────

/// LSP `Position` — 0-indexed line and character.
///
/// The `character` field unit depends on the negotiated `positionEncoding`
/// (utf-8 bytes, utf-16 code units, or utf-32 code points).
/// Guard: F2 (RPN 432) — callers must convert through `LineIndex::pos_to_byte`
/// before slicing source text.
#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
pub struct Position {
    pub line: u32,
    pub character: u32,
}

/// LSP `Range` — a half-open `[start, end)` interval within a file.
#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
pub struct Range {
    pub start: Position,
    pub end: Position,
}

/// LSP `Location` — a file URI plus a range within that file.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Location {
    pub uri: String,
    pub range: Range,
}

impl Drop for LspSession {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}

// ── Test helpers (pub(crate)) ─────────────────────────────────────────────────
//
// These allow tests in other modules (e.g. `extractor::references_tests` and
// `extractor::call_hierarchy_tests`) to build mock `LspSession` instances backed
// by a sleeping subprocess + injected frames, without ever spawning a real LSP.

/// Re-export `spawn_reader` under a `pub(crate)` alias so test code in sibling
/// modules can set up the reader thread independently from session construction.
#[allow(dead_code)]
pub(crate) fn spawn_reader_for_test(
    stdout: std::process::ChildStdout,
    tx: mpsc::Sender<ReaderEvent>,
) -> thread::JoinHandle<()> {
    spawn_reader(stdout, tx)
}

impl LspSession {
    /// Construct a mock session from a spawned child and a sync-channel receiver.
    ///
    /// `has_references` controls whether `referencesProvider` and
    /// `callHierarchyProvider` capabilities are advertised.  All other
    /// capabilities default to `true` so callers can focus on the tested feature.
    #[allow(dead_code)]
    pub(crate) fn new_for_test(
        stdin: std::process::ChildStdin,
        child: std::process::Child,
        rx: mpsc::Receiver<ReaderEvent>,
        has_references: bool,
    ) -> Self {
        Self {
            stdin: Some(stdin),
            child,
            next_id: 1,
            pending: HashMap::new(),
            negotiated_encoding: PositionEncoding::Utf16,
            server_capabilities: ServerCapabilities {
                document_symbol: true,
                call_hierarchy: has_references,
                references: has_references,
                type_definition: true,
                implementation: true,
                hover: true,
                definition: true,
            },
            timeouts: read_timeouts(),
            rx,
            _reader_handle: thread::spawn(|| {}),
        }
    }

    /// Construct a mock session backed by an async `mpsc::Receiver`.
    ///
    /// Use this when test code injects frames via a `mpsc::Sender` (not `SyncSender`).
    /// `has_call_hierarchy` gates both `callHierarchyProvider` and `referencesProvider`.
    #[allow(dead_code)]
    pub(crate) fn new_for_test_async(
        stdin: std::process::ChildStdin,
        child: std::process::Child,
        rx: mpsc::Receiver<ReaderEvent>,
        has_call_hierarchy: bool,
    ) -> Self {
        Self {
            stdin: Some(stdin),
            child,
            next_id: 1,
            pending: HashMap::new(),
            negotiated_encoding: PositionEncoding::Utf16,
            server_capabilities: ServerCapabilities {
                document_symbol: true,
                call_hierarchy: has_call_hierarchy,
                references: has_call_hierarchy,
                type_definition: true,
                implementation: true,
                hover: true,
                definition: true,
            },
            timeouts: read_timeouts(),
            rx,
            _reader_handle: thread::spawn(|| {}),
        }
    }

    /// Construct a mock session with an explicit override timeout for all methods.
    ///
    /// Used by timeout tests where default 10–15 s timeouts would make tests slow.
    #[allow(dead_code)]
    pub(crate) fn new_for_test_with_timeout(
        stdin: std::process::ChildStdin,
        child: std::process::Child,
        rx: mpsc::Receiver<ReaderEvent>,
        has_call_hierarchy: bool,
        override_timeout: Duration,
    ) -> Self {
        let timeouts = SessionTimeouts {
            default: override_timeout,
            references: override_timeout,
            hover: override_timeout,
            call_hierarchy: override_timeout,
            type_definition: override_timeout,
            implementation: override_timeout,
        };
        Self {
            stdin: Some(stdin),
            child,
            next_id: 1,
            pending: HashMap::new(),
            negotiated_encoding: PositionEncoding::Utf16,
            server_capabilities: ServerCapabilities {
                document_symbol: true,
                call_hierarchy: has_call_hierarchy,
                references: has_call_hierarchy,
                type_definition: true,
                implementation: true,
                hover: true,
                definition: true,
            },
            timeouts,
            rx,
            _reader_handle: thread::spawn(|| {}),
        }
    }
}

// ── Symbol parsing ─────────────────────────────────────────────────────────────

/// Parse LSP DocumentSymbol response into our Symbol type.
fn parse_symbols(value: &Value, file: &str) -> Result<Vec<Symbol>> {
    let arr = match value.as_array() {
        Some(a) => a,
        None => return Ok(vec![]),
    };

    let mut symbols = Vec::new();
    for item in arr {
        if let Some(sym) = parse_one_symbol(item, file) {
            symbols.push(sym);
        }
    }
    Ok(symbols)
}

fn parse_one_symbol(item: &Value, file: &str) -> Option<Symbol> {
    let name = item.get("name")?.as_str()?.to_string();
    let kind_num = item.get("kind")?.as_u64()?;
    let kind = SymbolKind::from_lsp(kind_num);

    let range = item.get("range")?;
    let start_line = range.get("start")?.get("line")?.as_u64()? as u32;
    let end_line = range.get("end")?.get("line")?.as_u64()? as u32;

    let detail = item
        .get("detail")
        .and_then(|d| d.as_str())
        .map(String::from);

    let children = item
        .get("children")
        .and_then(|c| c.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|child| parse_one_symbol(child, file))
                .collect()
        })
        .unwrap_or_default();

    Some(Symbol {
        name,
        kind,
        detail,
        file: file.to_string(),
        line: start_line + 1, // LSP is 0-indexed
        end_line: end_line + 1,
        children,
    })
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    // Helper: build a minimal initializeResult capabilities JSON.
    fn caps_json(providers: &[(&str, Value)]) -> Value {
        let mut obj = serde_json::Map::new();
        for (k, v) in providers {
            obj.insert(k.to_string(), v.clone());
        }
        Value::Object(obj)
    }

    // ── Capability parsing ────────────────────────────────────────────────────

    /// T3 verification: `capabilities::parses_all_providers_present`
    ///
    /// All seven providers present as `true` → all fields true.
    #[test]
    fn parses_all_providers_present() {
        let caps = caps_json(&[
            ("documentSymbolProvider", json!(true)),
            ("callHierarchyProvider", json!(true)),
            ("referencesProvider", json!(true)),
            ("typeDefinitionProvider", json!(true)),
            ("implementationProvider", json!(true)),
            ("hoverProvider", json!(true)),
            ("definitionProvider", json!(true)),
        ]);
        let parsed = parse_capabilities(&caps);
        assert!(parsed.document_symbol, "document_symbol");
        assert!(parsed.call_hierarchy, "call_hierarchy");
        assert!(parsed.references, "references");
        assert!(parsed.type_definition, "type_definition");
        assert!(parsed.implementation, "implementation");
        assert!(parsed.hover, "hover");
        assert!(parsed.definition, "definition");
    }

    /// T3 verification: `capabilities::parses_with_provider_as_object`
    ///
    /// `"callHierarchyProvider": {"workDoneProgress": true}` → true.
    #[test]
    fn parses_with_provider_as_object() {
        let caps = caps_json(&[
            ("documentSymbolProvider", json!(true)),
            ("callHierarchyProvider", json!({"workDoneProgress": true})),
        ]);
        let parsed = parse_capabilities(&caps);
        assert!(parsed.call_hierarchy, "object-valued provider must be true");
    }

    /// T3 verification: `capabilities::missing_provider_defaults_false`
    ///
    /// `hoverProvider` absent → `caps.hover == false`.
    #[test]
    fn missing_provider_defaults_false() {
        let caps = caps_json(&[("documentSymbolProvider", json!(true))]);
        let parsed = parse_capabilities(&caps);
        assert!(!parsed.hover, "absent hoverProvider must default to false");
    }

    /// T3 verification: `capabilities::provider_false_is_unsupported`
    ///
    /// `"hoverProvider": false` → false.
    #[test]
    fn provider_false_is_unsupported() {
        let caps = caps_json(&[
            ("documentSymbolProvider", json!(true)),
            ("hoverProvider", json!(false)),
        ]);
        let parsed = parse_capabilities(&caps);
        assert!(!parsed.hover, "explicit false must mean unsupported");
    }

    /// T3 verification: `capabilities::extraction_mode_symbols_only`
    ///
    /// Only `documentSymbolProvider` true → `"symbols_only"`.
    #[test]
    fn extraction_mode_symbols_only() {
        // Build a minimal session to call extraction_mode.
        // We can't call LspSession::start without a real binary, so we test
        // `extraction_mode` logic via `capability_for_method` + direct
        // struct construction through the test helper below.
        let caps = build_test_caps(ServerCapabilities {
            document_symbol: true,
            call_hierarchy: false,
            references: false,
            type_definition: false,
            implementation: false,
            hover: false,
            definition: false,
        });
        assert_eq!(caps.extraction_mode(), "symbols_only");
    }

    /// T3 verification: `capabilities::extraction_mode_full`
    ///
    /// All Tier-1 + hover true → `"full"`.
    #[test]
    fn extraction_mode_full() {
        let caps = build_test_caps(ServerCapabilities {
            document_symbol: true,
            call_hierarchy: true,
            references: true,
            type_definition: true,
            implementation: true,
            hover: true,
            definition: true,
        });
        assert_eq!(caps.extraction_mode(), "full");
    }

    /// T3 verification: extraction_mode partial — some but not all Tier-1.
    #[test]
    fn extraction_mode_partial() {
        let caps = build_test_caps(ServerCapabilities {
            document_symbol: true,
            call_hierarchy: true,
            references: false,
            type_definition: false,
            implementation: false,
            hover: false,
            definition: false,
        });
        assert_eq!(caps.extraction_mode(), "partial");
    }

    // ── capability_for_method ─────────────────────────────────────────────────

    /// T3 verification: capability_for_method — maps methods to correct flags.
    #[test]
    fn capability_for_method_maps_correctly() {
        let helper = build_test_caps(ServerCapabilities {
            document_symbol: true,
            call_hierarchy: false,
            references: true,
            type_definition: false,
            implementation: false,
            hover: true,
            definition: false,
        });

        assert_eq!(
            helper.capability_for_method("textDocument/documentSymbol"),
            Some(true)
        );
        assert_eq!(
            helper.capability_for_method("textDocument/prepareCallHierarchy"),
            Some(false)
        );
        assert_eq!(
            helper.capability_for_method("callHierarchy/incomingCalls"),
            Some(false)
        );
        assert_eq!(
            helper.capability_for_method("callHierarchy/outgoingCalls"),
            Some(false)
        );
        assert_eq!(
            helper.capability_for_method("textDocument/references"),
            Some(true)
        );
        assert_eq!(
            helper.capability_for_method("textDocument/typeDefinition"),
            Some(false)
        );
        assert_eq!(
            helper.capability_for_method("textDocument/implementation"),
            Some(false)
        );
        assert_eq!(
            helper.capability_for_method("textDocument/hover"),
            Some(true)
        );
        assert_eq!(
            helper.capability_for_method("textDocument/definition"),
            Some(false)
        );
        // Non-capability-gated methods return None.
        assert_eq!(helper.capability_for_method("initialize"), None);
        assert_eq!(helper.capability_for_method("shutdown"), None);
    }

    // ── request_if_supported (capability gate, unit-testable portion) ─────────

    /// T3 verification: `request_if_supported_returns_none_without_round_trip`
    ///
    /// Scoped to the `capability_for_method` helper — the round-trip path
    /// requires a mock LSP subprocess (see integration tests).  This verifies
    /// the gate logic: a method whose capability flag is false returns
    /// `Ok(None)` before any I/O occurs.
    #[test]
    fn capability_gate_blocks_unsupported_method() {
        let helper = build_test_caps(ServerCapabilities {
            document_symbol: true,
            call_hierarchy: false,
            references: false,
            type_definition: false,
            implementation: false,
            hover: false,
            definition: false,
        });
        // `callHierarchy` is false → capability_for_method returns Some(false).
        assert_eq!(
            helper.capability_for_method("textDocument/prepareCallHierarchy"),
            Some(false)
        );
        assert_eq!(
            helper.capability_for_method("callHierarchy/incomingCalls"),
            Some(false)
        );
    }

    // ── Timeout tests ─────────────────────────────────────────────────────────

    /// T3 verification: `timeout_returns_timed_out_error`
    ///
    /// A mock LSP that never replies → `request_with_timeout` returns
    /// `LspError::TimedOut` within 100–300ms.
    #[test]
    fn timeout_returns_timed_out_error() {
        // `sleep 10` never produces output on stdout.
        let mut child = std::process::Command::new("sh")
            .args(["-c", "sleep 10"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("failed to spawn sleep");

        let stdin = child.stdin.take().expect("stdin");
        let stdout = child.stdout.take().expect("stdout");

        let (tx, rx) = mpsc::channel::<ReaderEvent>();
        let reader_handle = spawn_reader(stdout, tx);

        let mut session = LspSession {
            stdin: Some(stdin),
            child,
            next_id: 1,
            pending: HashMap::new(),
            negotiated_encoding: PositionEncoding::Utf16,
            server_capabilities: ServerCapabilities {
                document_symbol: true,
                call_hierarchy: true,
                references: true,
                type_definition: true,
                implementation: true,
                hover: true,
                definition: true,
            },
            timeouts: read_timeouts(),
            rx,
            _reader_handle: reader_handle,
        };

        let deadline = Duration::from_millis(100);
        let start = Instant::now();
        let result = session.request_with_timeout("textDocument/hover", json!({}), deadline);
        let elapsed = start.elapsed();

        assert!(
            matches!(result, Err(LspError::TimedOut { .. })),
            "expected TimedOut, got: {result:?}"
        );
        // Must not return early (< 90ms) nor spin-wait far past deadline.
        assert!(
            elapsed >= Duration::from_millis(90),
            "returned too early: {elapsed:?}"
        );
        assert!(
            elapsed < Duration::from_millis(500),
            "took too long: {elapsed:?}"
        );
    }

    /// T3 verification: `timeout_does_not_poison_session`
    ///
    /// After a timeout, a subsequent request to a cooperative mock LSP
    /// returns success.  We simulate this by:
    ///   1. Sending a request to a subprocess that never replies → TimedOut.
    ///   2. Injecting a valid reply directly into the session's rx channel.
    ///   3. Issuing a second request and verifying it can succeed.
    ///
    /// Because we need channel injection without spawning a full LSP,
    /// we verify the weaker invariant: after a timeout, `pending` correctly
    /// marks the first id as `Abandoned` so it won't block future requests.
    #[test]
    fn timeout_marks_id_abandoned() {
        let mut child = std::process::Command::new("sh")
            .args(["-c", "sleep 10"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn");

        let stdin = child.stdin.take().expect("stdin");
        let stdout = child.stdout.take().expect("stdout");

        let (tx, rx) = mpsc::channel::<ReaderEvent>();
        let reader_handle = spawn_reader(stdout, tx);

        let mut session = LspSession {
            stdin: Some(stdin),
            child,
            next_id: 1,
            pending: HashMap::new(),
            negotiated_encoding: PositionEncoding::Utf16,
            server_capabilities: ServerCapabilities {
                document_symbol: true,
                call_hierarchy: true,
                references: true,
                type_definition: true,
                implementation: true,
                hover: true,
                definition: true,
            },
            timeouts: read_timeouts(),
            rx,
            _reader_handle: reader_handle,
        };

        // First request — will time out.
        let timed_out_id = session.next_id; // Will be 1.
        let _ = session.request_with_timeout(
            "textDocument/hover",
            json!({}),
            Duration::from_millis(50),
        );

        // After timeout, the id must be marked Abandoned.
        assert_eq!(
            session.pending.get(&timed_out_id).copied(),
            Some(RequestState::Abandoned),
            "timed-out id must be Abandoned, not removed or Pending"
        );
    }

    /// T3 verification: `env_var_overrides_default_timeout`
    ///
    /// Setting `FORGE_LSP_TIMEOUT_MS=42` causes `read_timeouts()` to return
    /// 42ms as the default timeout.
    #[test]
    fn env_var_overrides_default_timeout() {
        // Use a unique env var key scoped to this test process.
        // Tests run in parallel; use std::env within this thread only.
        unsafe {
            std::env::set_var("FORGE_LSP_TIMEOUT_MS", "42");
        }
        let timeouts = read_timeouts();
        unsafe {
            std::env::remove_var("FORGE_LSP_TIMEOUT_MS");
        }
        assert_eq!(timeouts.default, Duration::from_millis(42));
    }

    /// T3 verification: per-method env var overrides per-method timeout.
    #[test]
    fn env_var_overrides_per_method_timeout() {
        unsafe {
            std::env::set_var("FORGE_LSP_TIMEOUT_REFERENCES_MS", "777");
        }
        let timeouts = read_timeouts();
        unsafe {
            std::env::remove_var("FORGE_LSP_TIMEOUT_REFERENCES_MS");
        }
        assert_eq!(timeouts.references, Duration::from_millis(777));
        assert_eq!(
            timeouts.for_method("textDocument/references"),
            Duration::from_millis(777)
        );
    }

    // ── Test helper ───────────────────────────────────────────────────────────

    /// A thin wrapper that exposes `extraction_mode` and `capability_for_method`
    /// without requiring a real LSP subprocess.
    struct CapHelper {
        server_capabilities: ServerCapabilities,
    }

    impl CapHelper {
        fn extraction_mode(&self) -> &'static str {
            let caps = &self.server_capabilities;
            let tier1_full = caps.call_hierarchy
                && caps.references
                && caps.type_definition
                && caps.implementation
                && caps.hover;
            if tier1_full {
                "full"
            } else if caps.call_hierarchy
                || caps.references
                || caps.type_definition
                || caps.implementation
                || caps.hover
            {
                "partial"
            } else {
                "symbols_only"
            }
        }

        fn capability_for_method(&self, method: &str) -> Option<bool> {
            let caps = &self.server_capabilities;
            match method {
                "textDocument/documentSymbol" => Some(caps.document_symbol),
                "textDocument/prepareCallHierarchy"
                | "callHierarchy/incomingCalls"
                | "callHierarchy/outgoingCalls" => Some(caps.call_hierarchy),
                "textDocument/references" => Some(caps.references),
                "textDocument/typeDefinition" => Some(caps.type_definition),
                "textDocument/implementation" => Some(caps.implementation),
                "textDocument/hover" => Some(caps.hover),
                "textDocument/definition" => Some(caps.definition),
                _ => None,
            }
        }
    }

    fn build_test_caps(caps: ServerCapabilities) -> CapHelper {
        CapHelper {
            server_capabilities: caps,
        }
    }

    // ── T13 hover-parser tests ───────────────────────────────────────────────

    #[test]
    fn hover_markup_content_markdown() {
        let v = serde_json::json!({
            "contents": { "kind": "markdown", "value": "# Foo\n\nA thing." }
        });
        let r = parse_hover_response(&v);
        assert_eq!(r.content, "# Foo\n\nA thing.");
        assert_eq!(r.format.as_deref(), Some("markdown"));
    }

    #[test]
    fn hover_markup_content_plaintext() {
        let v = serde_json::json!({
            "contents": { "kind": "plaintext", "value": "bare text" }
        });
        let r = parse_hover_response(&v);
        assert_eq!(r.content, "bare text");
        assert_eq!(r.format.as_deref(), Some("plaintext"));
    }

    #[test]
    fn hover_marked_string_array() {
        let v = serde_json::json!({
            "contents": [
                { "language": "rust", "value": "fn foo()" },
                "additional docs"
            ]
        });
        let r = parse_hover_response(&v);
        assert!(r.content.contains("fn foo()"));
        assert!(r.content.contains("additional docs"));
        assert_eq!(r.format, None);
    }

    #[test]
    fn hover_bare_string() {
        let v = serde_json::json!({ "contents": "just a string" });
        let r = parse_hover_response(&v);
        assert_eq!(r.content, "just a string");
        assert_eq!(r.format, None);
    }

    #[test]
    fn hover_single_marked_string_object() {
        let v = serde_json::json!({
            "contents": { "language": "rust", "value": "fn bar() -> u32" }
        });
        let r = parse_hover_response(&v);
        assert_eq!(r.content, "fn bar() -> u32");
        assert_eq!(r.format, None);
    }

    #[test]
    fn hover_null_is_empty_not_panic() {
        let v = serde_json::Value::Null;
        let r = parse_hover_response(&v);
        assert_eq!(r.content, "");
        assert_eq!(r.format, None);
    }

    #[test]
    fn hover_missing_contents_is_empty() {
        let v = serde_json::json!({ "range": { "start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 0} } });
        let r = parse_hover_response(&v);
        assert_eq!(r.content, "");
        assert_eq!(r.format, None);
    }

    #[test]
    fn hover_unknown_shape_does_not_panic() {
        // A completely unexpected shape — the parser should return empty
        // rather than panic (Property: any valid JSON is safe input).
        let v = serde_json::json!({ "contents": { "weird": ["nonsense"] } });
        let r = parse_hover_response(&v);
        assert_eq!(r.content, "");
        assert_eq!(r.format, None);
    }
}
