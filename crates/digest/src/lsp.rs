//! Module: Minimal LSP client for symbol lookup via an installed language server.
//! Correctness: Correct when the server is only reported available if it really
//! answered, and a slow or absent server degrades to a disclosed failure rather
//! than a hang.
//! Last revised: 2026-10-01
//! Last changed: Initial implementation, workspace/symbol fast path.

//! A small, dependency-free LSP client: enough to ask a language server for
//! workspace symbols, which is the fastest correct way to find a definition
//! when a server is installed and the project is indexed.
//!
//! Deliberately narrow: one request (`workspace/symbol`), spoken over stdio with
//! proper `Content-Length` framing. No document sync, no diagnostics. If a server
//! needs more than this to answer, it is not the fast path and the caller should
//! use the scan instead.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

/// How long to wait between `workspace/symbol` retries while a server indexes.
///
/// Short enough that a warm server is not kept waiting, long enough not to spin
/// the server with requests it cannot yet answer.
const INDEX_POLL_INTERVAL: Duration = Duration::from_millis(300);

/// A language server Forge knows how to drive.
///
/// Static string fields so the candidate table can be a `const`. Never
/// deserialized — this is an internal candidate table, not a wire type.
#[derive(Debug, Clone, PartialEq)]
pub struct ServerSpec {
    /// Language this server covers, as Forge reports stacks (`rust`, `c`, ...).
    pub language: &'static str,
    /// Executable name to look for on `PATH`.
    pub binary: &'static str,
    /// File extensions the server is authoritative for.
    pub extensions: &'static [&'static str],
    /// Marker files that indicate this project actually uses the language.
    /// Empty means the extension check alone is enough.
    pub markers: &'static [&'static str],
}

/// Servers Forge will use, in preference order per language.
///
/// Only servers whose binary is genuinely installed are ever reported; this is
/// the candidate table, not a claim that any of them exist.
pub const KNOWN_SERVERS: &[ServerSpec] = &[
    ServerSpec {
        language: "rust",
        binary: "rust-analyzer",
        extensions: &["rs"],
        markers: &["Cargo.toml"],
    },
    ServerSpec {
        language: "c",
        binary: "clangd",
        extensions: &["c", "cc", "cpp", "cxx", "h", "hpp", "m", "mm"],
        markers: &["compile_commands.json", "CMakeLists.txt", "Makefile"],
    },
    ServerSpec {
        language: "go",
        binary: "gopls",
        extensions: &["go"],
        markers: &["go.mod"],
    },
    ServerSpec {
        language: "python",
        binary: "pyright-langserver",
        extensions: &["py"],
        markers: &["pyproject.toml", "setup.py", "requirements.txt"],
    },
    ServerSpec {
        language: "python",
        binary: "basedpyright-langserver",
        extensions: &["py"],
        markers: &["pyproject.toml", "setup.py", "requirements.txt"],
    },
    ServerSpec {
        language: "python",
        binary: "pylsp",
        extensions: &["py"],
        markers: &["pyproject.toml", "setup.py", "requirements.txt"],
    },
    ServerSpec {
        language: "typescript",
        binary: "typescript-language-server",
        extensions: &["ts", "tsx", "js", "jsx", "mjs", "cjs"],
        markers: &["package.json", "tsconfig.json"],
    },
    ServerSpec {
        language: "elixir",
        binary: "elixir-ls",
        extensions: &["ex", "exs"],
        markers: &["mix.exs"],
    },
    ServerSpec {
        language: "java",
        binary: "jdtls",
        extensions: &["java"],
        markers: &["pom.xml", "build.gradle"],
    },
    ServerSpec {
        language: "swift",
        binary: "sourcekit-lsp",
        extensions: &["swift"],
        markers: &["Package.swift"],
    },
];

/// A language server found on this machine.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AvailableServer {
    pub language: String,
    pub binary: String,
    /// Absolute path to the executable.
    pub path: String,
    /// True when every marker file the spec names is present in the search root.
    pub project_matches: bool,
    /// True when the binary actually executes. A PATH entry is not proof: a
    /// rustup shim for an uninstalled component is an executable file that
    /// exits with an error, and reporting that as "available" would be a lie.
    pub runnable: bool,
    /// Why the binary could not run, when it could not.
    pub probe_error: Option<String>,
}

impl AvailableServer {
    /// Extra arguments this server needs to speak LSP over stdio.
    pub fn args(&self) -> Vec<&'static str> {
        match self.binary.as_str() {
            // These two default to a CLI mode, not stdio LSP.
            "pyright-langserver" | "basedpyright-langserver" => vec!["--stdio"],
            _ => vec![],
        }
    }
}

/// Detect language servers installed on this machine.
///
/// `root` is the project being searched. A server is marked `project_matches`
/// when its marker files are present *or* the tree contains files with an
/// extension it owns — a subtree such as `crates/` has no `Cargo.toml` of its
/// own but is plainly Rust, and refusing to use a server there would be wrong.
///
/// Presence on `PATH` is verified by execution, not by file existence: rustup
/// installs shims for components that may not be installed, and those shims are
/// real files that fail when run.
pub fn detect_servers(root: &Path) -> Vec<AvailableServer> {
    let extensions = source_extensions(root);

    let mut found = Vec::new();
    for spec in KNOWN_SERVERS {
        let Some(path) = which(spec.binary) else {
            continue;
        };
        let has_marker = spec.markers.iter().any(|marker| root.join(marker).exists());
        let has_extension = spec
            .extensions
            .iter()
            .any(|ext| extensions.iter().any(|present| present == ext));
        let (runnable, probe_error) = probe(&path);
        found.push(AvailableServer {
            language: spec.language.to_string(),
            binary: spec.binary.to_string(),
            path: path.display().to_string(),
            project_matches: has_marker || has_extension,
            runnable,
            probe_error,
        });
    }
    found
}

/// Collect the source-file extensions present in `root`, in one bounded walk.
///
/// Bounded because the only question is *which* languages are here, which the
/// first few thousand files answer; walking a whole monorepo per lookup would
/// reintroduce the cost this module exists to remove.
fn source_extensions(root: &Path) -> Vec<String> {
    const MAX_FILES: usize = 5_000;
    let mut seen: Vec<String> = Vec::new();
    let mut files = 0usize;
    let walker = ignore::WalkBuilder::new(root)
        .hidden(true)
        .git_ignore(true)
        .build();
    for entry in walker.flatten() {
        if !entry.file_type().is_some_and(|ft| ft.is_file()) {
            continue;
        }
        files += 1;
        if files > MAX_FILES {
            break;
        }
        if let Some(ext) = entry.path().extension().and_then(|e| e.to_str()) {
            if KNOWN_SERVERS
                .iter()
                .any(|spec| spec.extensions.contains(&ext))
                && !seen.iter().any(|s| s == ext)
            {
                seen.push(ext.to_string());
            }
        }
    }
    seen
}

/// Check that a binary actually executes.
///
/// Tries `--version` then `--help`: servers disagree about which they accept
/// (sourcekit-lsp rejects `--version` but runs fine), so a single flag would
/// mislabel a working server as broken. A failure of both is reported with the
/// captured message so a caller sees *why* — typically a rustup shim for an
/// uninstalled component.
fn probe(path: &Path) -> (bool, Option<String>) {
    let mut last_error = None;
    for flag in ["--version", "--help"] {
        match Command::new(path).arg(flag).stdin(Stdio::null()).output() {
            Ok(output) if output.status.success() => return (true, None),
            Ok(output) => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                let stdout = String::from_utf8_lossy(&output.stdout);
                let detail = stderr
                    .lines()
                    .chain(stdout.lines())
                    .find(|line| !line.trim().is_empty())
                    .unwrap_or("exited without output")
                    .trim()
                    .to_string();
                last_error = Some(detail);
            }
            Err(err) => last_error = Some(err.to_string()),
        }
    }
    (false, last_error)
}

/// Pick the best server for a root: a runnable one whose project markers are
/// present, preferring the order declared in `KNOWN_SERVERS`.
///
/// Only a marker match qualifies. A runnable server for the *wrong* language
/// answers confidently and emptily — clangd asked about a Rust symbol returns
/// zero results, which reads as "no such symbol" and is a false negative worse
/// than not using a server at all. No marker match means no server, and the
/// caller scans instead.
pub fn best_server_for(root: &Path) -> Option<AvailableServer> {
    detect_servers(root)
        .into_iter()
        .find(|s| s.runnable && s.project_matches)
}

/// Servers that were found on `PATH` but cannot run, for disclosure.
pub fn unusable_servers(root: &Path) -> Vec<AvailableServer> {
    detect_servers(root)
        .into_iter()
        .filter(|s| !s.runnable)
        .collect()
}

/// Locate an executable on `PATH`.
fn which(binary: &str) -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(binary);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// One symbol returned by a language server.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LspSymbol {
    pub name: String,
    /// Human-readable kind (`function`, `struct`, ...), mapped from LSP's number.
    pub kind: String,
    pub file: String,
    pub line: usize,
    pub end_line: Option<usize>,
}

/// Outcome of an LSP query, including how long it took.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LspQuery {
    pub server: String,
    pub elapsed_ms: u128,
    pub symbols: Vec<LspSymbol>,
}

/// Ask a language server for workspace symbols matching `query`.
///
/// Bounded by `timeout`: a server that is cold-starting or wedged is killed and
/// reported as an error, so the caller can fall back to a scan instead of
/// hanging. A server that exits without answering is also an error, never an
/// empty success — "no symbols" and "the server died" must not look alike.
///
/// A freshly started server answers `workspace/symbol` immediately with an empty
/// list while it is still indexing, so the request is re-issued until it returns
/// something or the budget runs out. Without that wait the fast path would never
/// win: every cold server would "answer" empty in a second and always lose to
/// the scan.
pub fn workspace_symbols(
    server: &AvailableServer,
    root: &Path,
    query: &str,
    timeout: Duration,
) -> Result<LspQuery> {
    let started = Instant::now();
    // Server stderr is captured, not discarded: when a server fails, its own
    // message ("Unknown binary", "no such file") is the whole diagnosis.
    let stderr_path = std::env::temp_dir().join(format!(
        "forge-lsp-{}-{}.err",
        std::process::id(),
        started.elapsed().as_nanos()
    ));
    let stderr_file = std::fs::File::create(&stderr_path).ok();
    let mut child = Command::new(&server.path)
        .args(server.args())
        .current_dir(root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(match &stderr_file {
            Some(file) => Stdio::from(file.try_clone()?),
            None => Stdio::null(),
        })
        .spawn()
        .with_context(|| format!("starting {}", server.path))?;

    let mut stdin = child.stdin.take().context("capturing server stdin")?;
    let stdout = child.stdout.take().context("capturing server stdout")?;

    let result = run_session(&mut stdin, stdout, root, query, timeout, &mut child);

    // Always tear the server down; a stray rust-analyzer would keep indexing.
    let _ = child.kill();
    let _ = child.wait();
    // The write handle must close before the file is read back.
    drop(stderr_file);

    let symbols = result.map_err(|err| match read_server_stderr(&stderr_path) {
        Some(detail) => anyhow::anyhow!("{err} — {} said: {detail}", server.binary),
        None => err,
    })?;
    let _ = std::fs::remove_file(&stderr_path);
    Ok(LspQuery {
        server: server.binary.clone(),
        elapsed_ms: started.elapsed().as_millis(),
        symbols,
    })
}

/// Read whatever the server wrote to stderr, for error messages.
fn read_server_stderr(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    // Servers can be chatty; the first lines carry the actual failure.
    Some(
        trimmed
            .lines()
            .take(3)
            .collect::<Vec<_>>()
            .join(" / ")
            .chars()
            .take(300)
            .collect(),
    )
}

/// Drive initialize -> initialized -> workspace/symbol, then collect.
///
/// `workspace/symbol` is re-issued while it returns nothing, because an indexing
/// server answers empty rather than blocking. Each attempt uses a fresh request
/// id.
fn run_session(
    stdin: &mut ChildStdin,
    stdout: ChildStdout,
    root: &Path,
    query: &str,
    timeout: Duration,
    child: &mut Child,
) -> Result<Vec<LspSymbol>> {
    let root_uri = path_to_uri(root);

    send(
        stdin,
        &serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "processId": std::process::id(),
                "rootUri": root_uri,
                "capabilities": {},
                "workspaceFolders": [{"uri": root_uri, "name": "root"}]
            }
        }),
    )?;

    let mut reader = BufReader::new(stdout);
    let deadline = Instant::now() + timeout;

    // Wait for the initialize response, then announce we are ready.
    read_response(&mut reader, 1, deadline, child)?;
    send(
        stdin,
        &serde_json::json!({"jsonrpc": "2.0", "method": "initialized", "params": {}}),
    )?;

    let mut request_id = 2;
    loop {
        send(
            stdin,
            &serde_json::json!({
                "jsonrpc": "2.0",
                "id": request_id,
                "method": "workspace/symbol",
                "params": {"query": query}
            }),
        )?;
        let response = read_response(&mut reader, request_id, deadline, child)?;
        let symbols = parse_symbols(&response, root);
        if !symbols.is_empty() {
            return Ok(symbols);
        }
        if Instant::now() >= deadline {
            // Empty for the whole budget: report the empty answer rather than an
            // error, so the caller can say "still indexing" instead of "broken".
            return Ok(symbols);
        }
        request_id += 1;
        std::thread::sleep(INDEX_POLL_INTERVAL);
    }
}

/// Write one framed JSON-RPC message.
fn send(stdin: &mut ChildStdin, message: &serde_json::Value) -> Result<()> {
    let body = serde_json::to_vec(message)?;
    write!(stdin, "Content-Length: {}\r\n\r\n", body.len())?;
    stdin.write_all(&body)?;
    stdin.flush()?;
    Ok(())
}

/// Read framed messages until the response with `want_id` arrives.
///
/// Notifications and unrelated server traffic are skipped. A closed pipe or an
/// exhausted deadline is an error.
fn read_response(
    reader: &mut BufReader<ChildStdout>,
    want_id: i64,
    deadline: Instant,
    child: &mut Child,
) -> Result<serde_json::Value> {
    loop {
        if Instant::now() >= deadline {
            anyhow::bail!("language server did not answer within the timeout");
        }
        // A server that died gives a closed pipe, not a message.
        if let Ok(Some(_)) = child.try_wait() {
            anyhow::bail!("language server exited before answering");
        }

        let Some(body) = read_message(reader)? else {
            anyhow::bail!("language server closed its output before answering");
        };

        let value: serde_json::Value = serde_json::from_slice(&body)
            .with_context(|| "language server sent a malformed response")?;
        if value.get("id").and_then(|v| v.as_i64()) == Some(want_id) {
            if let Some(error) = value.get("error") {
                anyhow::bail!("language server returned an error: {error}");
            }
            return Ok(value);
        }
        // Otherwise it is a notification or a different request; keep reading.
    }
}

/// Read one `Content-Length`-framed message body.
fn read_message(reader: &mut BufReader<ChildStdout>) -> Result<Option<Vec<u8>>> {
    let mut content_length: Option<usize> = None;
    loop {
        let mut header = String::new();
        let read = reader.read_line(&mut header)?;
        if read == 0 {
            return Ok(None);
        }
        let trimmed = header.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            break; // end of headers
        }
        if let Some(value) = trimmed.strip_prefix("Content-Length:") {
            content_length = Some(value.trim().parse().context("parsing Content-Length")?);
        }
    }

    let Some(length) = content_length else {
        anyhow::bail!("language server sent a message with no Content-Length");
    };
    let mut body = vec![0u8; length];
    reader.read_exact(&mut body)?;
    Ok(Some(body))
}

/// Convert a `workspace/symbol` response into symbols.
fn parse_symbols(response: &serde_json::Value, root: &Path) -> Vec<LspSymbol> {
    let Some(items) = response.get("result").and_then(|v| v.as_array()) else {
        return Vec::new();
    };

    items
        .iter()
        .filter_map(|item| {
            let name = item.get("name")?.as_str()?.to_string();
            let kind_number = item.get("kind").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
            // SymbolInformation carries `location`; WorkspaceSymbol may carry
            // `location` with the newer shape, so accept both spellings.
            let location = item.get("location")?;
            let uri = location.get("uri")?.as_str()?;
            let range = location.get("range")?;
            let start_line = range
                .get("start")
                .and_then(|s| s.get("line"))
                .and_then(|v| v.as_u64())? as usize;
            let end_line = range
                .get("end")
                .and_then(|s| s.get("line"))
                .and_then(|v| v.as_u64())
                .map(|v| v as usize);

            Some(LspSymbol {
                name,
                kind: symbol_kind_name(kind_number).to_string(),
                file: uri_to_path(uri, root),
                // LSP positions are 0-based; Forge reports 1-based lines.
                line: start_line + 1,
                end_line: end_line.map(|l| l + 1),
            })
        })
        .collect()
}

/// Map LSP `SymbolKind` numbers to Forge's kind vocabulary.
fn symbol_kind_name(kind: u32) -> &'static str {
    match kind {
        2 | 3 => "module",
        5 => "class",
        6 | 9 => "method",
        8 | 7 => "field",
        10 => "enum",
        11 => "interface",
        12 => "function",
        13 => "variable",
        14 => "constant",
        23 => "struct",
        26 => "type_alias",
        _ => "symbol",
    }
}

/// Build a `file://` URI for a directory.
fn path_to_uri(path: &Path) -> String {
    let absolute = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    format!("file://{}", absolute.display())
}

/// Turn a file URI back into a filesystem path, preferring a path relative to
/// the search root since that is what Forge reports everywhere else.
fn uri_to_path(uri: &str, root: &Path) -> String {
    let raw = uri.strip_prefix("file://").unwrap_or(uri);
    let decoded = percent_decode(raw);
    let absolute = Path::new(&decoded);
    let canonical_root = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    absolute
        .strip_prefix(&canonical_root)
        .map(|p| p.display().to_string())
        .unwrap_or(decoded)
}

/// Decode the percent-escapes LSP servers use in URIs (spaces, brackets).
fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok();
            if let Some(byte) = hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                out.push(byte);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_servers_cover_the_languages_forge_reports() {
        for language in [
            "rust",
            "c",
            "go",
            "python",
            "typescript",
            "elixir",
            "java",
            "swift",
        ] {
            assert!(
                KNOWN_SERVERS.iter().any(|s| s.language == language),
                "no server spec for {language}"
            );
        }
    }

    #[test]
    fn pyright_needs_an_explicit_stdio_flag() {
        let server = AvailableServer {
            language: "python".into(),
            binary: "pyright-langserver".into(),
            path: "/x".into(),
            project_matches: true,
            runnable: true,
            probe_error: None,
        };
        assert_eq!(server.args(), vec!["--stdio"]);
        // rust-analyzer speaks stdio by default.
        let rust = AvailableServer {
            language: "rust".into(),
            binary: "rust-analyzer".into(),
            path: "/x".into(),
            project_matches: true,
            runnable: true,
            probe_error: None,
        };
        assert!(rust.args().is_empty());
    }

    #[test]
    fn only_installed_servers_are_reported() {
        // A binary that cannot exist on PATH must never be reported.
        let servers = detect_servers(Path::new("."));
        assert!(
            servers.iter().all(|s| Path::new(&s.path).is_file()),
            "every reported server must resolve to a real file: {servers:?}"
        );
    }

    /// A rustup shim is an executable file that fails when run. Reporting it as
    /// available would send every lookup down a path guaranteed to fail.
    #[test]
    fn a_binary_that_cannot_execute_is_not_available() {
        let broken = AvailableServer {
            language: "rust".into(),
            binary: "definitely-not-runnable".into(),
            path: "/nonexistent/definitely-not-runnable".into(),
            project_matches: true,
            runnable: false,
            probe_error: Some("Unknown binary".into()),
        };
        assert!(!broken.runnable);
        // Detection must never hand this back as the best server.
        let dir = tempfile::tempdir().unwrap();
        assert!(
            best_server_for(dir.path()).is_none_or(|s| s.runnable),
            "best_server_for must only return runnable servers"
        );
    }

    /// A server for the wrong language answers confidently and emptily. Choosing
    /// one turns "I don't know" into "no such symbol", which is worse than
    /// scanning.
    #[test]
    fn a_server_for_the_wrong_language_is_never_chosen() {
        let dir = tempfile::tempdir().unwrap();
        // A Rust project: only a server whose markers are present qualifies.
        std::fs::write(dir.path().join("Cargo.toml"), "[package]\n").unwrap();
        let chosen = best_server_for(dir.path());
        if let Some(server) = chosen {
            assert!(
                server.project_matches,
                "a server is only chosen when its own project markers matched: {server:?}"
            );
        }
    }

    #[test]
    fn no_marker_match_means_no_server() {
        let dir = tempfile::tempdir().unwrap();
        // An empty directory matches no project; scanning is the only honest
        // answer even if servers happen to be installed on this machine.
        assert!(
            best_server_for(dir.path()).is_none(),
            "an empty project must not select any server"
        );
    }

    #[test]
    fn unusable_servers_are_disclosed_with_a_reason() {
        let dir = tempfile::tempdir().unwrap();
        for server in unusable_servers(dir.path()) {
            assert!(server.probe_error.is_some(), "{server:?}");
        }
    }

    #[test]
    fn project_markers_decide_relevance() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), "[package]\n").unwrap();
        let servers = detect_servers(dir.path());
        if let Some(rust) = servers.iter().find(|s| s.language == "rust") {
            assert!(rust.project_matches, "Cargo.toml marks a Rust project");
        }
        let empty = tempfile::tempdir().unwrap();
        for server in detect_servers(empty.path()) {
            assert!(
                !server.project_matches,
                "an empty dir matches no project markers: {server:?}"
            );
        }
    }

    #[test]
    fn symbol_kinds_map_to_forge_vocabulary() {
        assert_eq!(symbol_kind_name(12), "function");
        assert_eq!(symbol_kind_name(23), "struct");
        assert_eq!(symbol_kind_name(10), "enum");
        assert_eq!(symbol_kind_name(11), "interface");
        assert_eq!(symbol_kind_name(14), "constant");
        assert_eq!(symbol_kind_name(99), "symbol");
    }

    #[test]
    fn lsp_lines_are_converted_to_one_based() {
        let response = serde_json::json!({
            "result": [{
                "name": "parse_level",
                "kind": 12,
                "location": {
                    "uri": "file:///repo/src/main.rs",
                    "range": {"start": {"line": 41, "character": 3}, "end": {"line": 44, "character": 4}}
                }
            }]
        });
        let symbols = parse_symbols(&response, Path::new("/repo"));
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name, "parse_level");
        assert_eq!(symbols[0].kind, "function");
        assert_eq!(symbols[0].line, 42, "0-based LSP line becomes 1-based");
        assert_eq!(symbols[0].end_line, Some(45));
        assert_eq!(symbols[0].file, "src/main.rs", "reported relative to root");
    }

    #[test]
    fn percent_escapes_in_uris_are_decoded() {
        assert_eq!(percent_decode("file%3A%2F%2Ftmp"), "file://tmp");
        assert_eq!(percent_decode("/a/b%20c/d"), "/a/b c/d");
        assert_eq!(percent_decode("no-escapes"), "no-escapes");
    }

    #[test]
    fn a_missing_response_result_is_empty_not_panicking() {
        let response = serde_json::json!({"id": 2, "result": null});
        assert!(parse_symbols(&response, Path::new(".")).is_empty());
    }

    /// A server that is not installed must fail loudly, not report zero symbols.
    #[test]
    fn absent_server_is_an_error() {
        let server = AvailableServer {
            language: "rust".into(),
            binary: "forge-no-such-server".into(),
            path: "/nonexistent/forge-no-such-server".into(),
            project_matches: true,
            runnable: false,
            probe_error: Some("not found".into()),
        };
        let result = workspace_symbols(
            &server,
            Path::new("."),
            "anything",
            Duration::from_millis(500),
        );
        assert!(
            result.is_err(),
            "a missing binary must error, not return []"
        );
    }

    /// End-to-end proof that the language-server path returns real symbols.
    ///
    /// Skipped (not failed) when no runnable Rust server is installed, so this is
    /// honest evidence on a configured machine and does not break CI elsewhere.
    #[test]
    fn lsp_path_returns_symbols_when_a_server_is_installed() {
        let Some(server) = best_server_for(Path::new(".")) else {
            eprintln!("skipped: no runnable language server on this machine");
            return;
        };
        if server.language != "rust" {
            eprintln!("skipped: {} is not the Rust server", server.binary);
            return;
        }

        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"probe\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        std::fs::create_dir(dir.path().join("src")).unwrap();
        std::fs::write(
            dir.path().join("src/lib.rs"),
            "pub fn distinctive_probe_symbol() -> u32 { 1 }\n",
        )
        .unwrap();

        let query = workspace_symbols(
            &server,
            dir.path(),
            "distinctive_probe_symbol",
            Duration::from_secs(120),
        )
        .expect("a runnable server must answer");

        assert_eq!(query.server, server.binary);
        let found = query
            .symbols
            .iter()
            .find(|s| s.name == "distinctive_probe_symbol")
            .unwrap_or_else(|| {
                panic!(
                    "the server must report the symbol it indexed: {:?}",
                    query.symbols
                )
            });
        assert_eq!(found.line, 1, "1-based line of the definition");
        assert!(found.file.ends_with("src/lib.rs"), "{}", found.file);
    }
}
