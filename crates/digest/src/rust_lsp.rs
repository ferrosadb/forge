//! Module: Query rust-analyzer for workspace symbols through its stdio LSP.
//! Correctness: Rust workspaces use language-server symbol locations when available and retain the source scanner as fallback.
//! Last revised: 2026-10-05
//! Last changed: Added bounded rust-analyzer workspace-symbol lookup for Forge.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};

use crate::lookup::SymbolMatch;

const RESPONSE_LIMIT: usize = 32 * 1024 * 1024;
/// rust-analyzer costs real time to start and answer, and the source walk is
/// now ~1s. These caps exist so a cold or wedged server cannot cost more than
/// the walk would have: the caller races the two and takes whichever answers
/// first, so a slow LSP must bow out early rather than hold the turn.
const SERVER_START_TIMEOUT: Duration = Duration::from_millis(1200);
const REQUEST_TIMEOUT: Duration = Duration::from_millis(1800);

enum WorkerMessage {
    Started(u32),
    Finished(Result<Vec<SymbolMatch>, String>),
}

/// Ask rust-analyzer for matching symbols in a Cargo workspace.
///
/// Returns `Ok(None)` when rust-analyzer is not installed or cannot start so
/// callers can use their normal source-based fallback.
pub fn workspace_symbols(query: &str, workspace: &Path) -> Result<Option<Vec<SymbolMatch>>> {
    let Some(binary) = find_rust_analyzer() else {
        return Ok(None);
    };

    let workspace = workspace.to_path_buf();
    let query = query.to_string();
    let (sender, receiver) = mpsc::sync_channel(2);
    let _worker = thread::spawn(move || {
        let mut child = match Command::new(binary)
            .arg("--stdio")
            .current_dir(&workspace)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(child) => child,
            Err(error) => {
                let _ = sender.send(WorkerMessage::Finished(Err(error.to_string())));
                return;
            }
        };
        let _ = sender.send(WorkerMessage::Started(child.id()));
        let result = query_workspace_symbols(&mut child, &workspace, &query);
        let _ = child.kill();
        let _ = child.wait();
        let _ = sender.send(WorkerMessage::Finished(
            result.map_err(|error| error.to_string()),
        ));
    });

    let pid = match receiver.recv_timeout(SERVER_START_TIMEOUT) {
        Ok(WorkerMessage::Started(pid)) => pid,
        Ok(WorkerMessage::Finished(_)) | Err(RecvTimeoutError::Disconnected) => return Ok(None),
        Err(RecvTimeoutError::Timeout) => return Ok(None),
    };
    match receiver.recv_timeout(REQUEST_TIMEOUT) {
        Ok(WorkerMessage::Finished(Ok(matches))) => Ok(Some(matches)),
        Ok(WorkerMessage::Finished(Err(_))) | Err(RecvTimeoutError::Disconnected) => Ok(None),
        Ok(WorkerMessage::Started(_)) => Ok(None),
        Err(RecvTimeoutError::Timeout) => {
            terminate_server(pid);
            Ok(None)
        }
    }
}

fn terminate_server(pid: u32) {
    // Forge's supported runtime hosts are macOS and Linux, both of which ship
    // the POSIX `kill` utility. The worker owns Child and reaps it after signal.
    let _ = Command::new("kill")
        .args(["-KILL", &pid.to_string()])
        .status();
}

fn find_rust_analyzer() -> Option<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(home) = dirs::home_dir() {
        candidates.push(home.join(".cargo/bin/rust-analyzer"));
    }
    if let Ok(executable) = std::env::current_exe() {
        if let Some(parent) = executable.parent() {
            candidates.push(parent.join("rust-analyzer"));
        }
    }
    if let Some(paths) = std::env::var_os("PATH") {
        candidates.extend(std::env::split_paths(&paths).map(|path| path.join("rust-analyzer")));
    }
    candidates.into_iter().find(|path| path.is_file())
}

fn query_workspace_symbols(
    child: &mut Child,
    workspace: &Path,
    query: &str,
) -> Result<Vec<SymbolMatch>> {
    let stdin = child
        .stdin
        .as_mut()
        .context("rust-analyzer stdin unavailable")?;
    let stdout = child
        .stdout
        .as_mut()
        .context("rust-analyzer stdout unavailable")?;
    let mut reader = BufReader::new(stdout);
    let root = workspace
        .canonicalize()
        .unwrap_or_else(|_| workspace.to_path_buf());
    let root_uri = file_uri(&root);

    write_request(
        stdin,
        1,
        "initialize",
        json!({
            "processId": std::process::id(),
            "rootUri": root_uri,
            "rootPath": root.to_string_lossy(),
            "workspaceFolders": [{ "uri": root_uri, "name": root.file_name().unwrap_or_default().to_string_lossy() }],
            "capabilities": {
                "workspace": { "symbol": { "dynamicRegistration": false } },
                "textDocument": {}
            },
            "clientInfo": { "name": "forge", "version": env!("CARGO_PKG_VERSION") }
        }),
    )?;
    let _initialized = read_response(&mut reader, stdin, 1)?;
    write_notification(stdin, "initialized", json!({}))?;
    write_request(stdin, 2, "workspace/symbol", json!({ "query": query }))?;
    let response = read_response(&mut reader, stdin, 2)?;
    let symbols = response
        .get("result")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("rust-analyzer returned an invalid workspace/symbol response"))?;

    Ok(symbols
        .iter()
        .filter_map(|symbol| symbol_match(symbol, query))
        .collect())
}

fn write_request(stdin: &mut ChildStdin, id: u64, method: &str, params: Value) -> Result<()> {
    write_message(
        stdin,
        json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }),
    )
}

fn write_notification(stdin: &mut ChildStdin, method: &str, params: Value) -> Result<()> {
    write_message(
        stdin,
        json!({ "jsonrpc": "2.0", "method": method, "params": params }),
    )
}

fn write_message(stdin: &mut ChildStdin, message: Value) -> Result<()> {
    let body = serde_json::to_vec(&message)?;
    write!(stdin, "Content-Length: {}\r\n\r\n", body.len())?;
    stdin.write_all(&body)?;
    stdin.flush()?;
    Ok(())
}

fn read_response(
    reader: &mut BufReader<&mut ChildStdout>,
    stdin: &mut ChildStdin,
    expected_id: u64,
) -> Result<Value> {
    loop {
        let mut length = None;
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line)? == 0 {
                return Err(anyhow!("rust-analyzer exited before responding"));
            }
            if line == "\r\n" || line == "\n" {
                break;
            }
            if let Some(value) = line.strip_prefix("Content-Length:") {
                length = Some(
                    value
                        .trim()
                        .parse::<usize>()
                        .context("invalid LSP content length")?,
                );
            }
        }
        let length = length.context("rust-analyzer response has no content length")?;
        if length > RESPONSE_LIMIT {
            return Err(anyhow!("rust-analyzer response exceeded the size limit"));
        }
        let mut body = vec![0; length];
        reader.read_exact(&mut body)?;
        let message: Value = serde_json::from_slice(&body)?;
        if let (Some(id), Some(method)) = (
            message.get("id"),
            message.get("method").and_then(Value::as_str),
        ) {
            let response = if method == "workspace/configuration" {
                let count = message
                    .pointer("/params/items")
                    .and_then(Value::as_array)
                    .map_or(0, Vec::len);
                json!({ "jsonrpc": "2.0", "id": id, "result": vec![Value::Null; count] })
            } else {
                json!({ "jsonrpc": "2.0", "id": id, "result": Value::Null })
            };
            write_message(stdin, response)?;
            continue;
        }
        if message.get("id").and_then(Value::as_u64) == Some(expected_id) {
            if let Some(error) = message.get("error") {
                return Err(anyhow!("rust-analyzer request failed: {error}"));
            }
            return Ok(message);
        }
    }
}

fn symbol_match(symbol: &Value, query: &str) -> Option<SymbolMatch> {
    let name = symbol.get("name")?.as_str()?;
    if !name.contains(query) {
        return None;
    }
    let location = symbol.get("location").unwrap_or(symbol);
    let uri = location.get("uri")?.as_str()?;
    let range = location.get("range")?;
    let line = range.get("start")?.get("line")?.as_u64()? as usize + 1;
    let end_line = range.get("end")?.get("line")?.as_u64()? as usize + 1;
    let file = file_path(uri);
    let kind = match symbol.get("kind").and_then(Value::as_u64).unwrap_or(0) {
        2 => "module",
        5 => "class",
        6 => "method",
        10 => "enum",
        11 => "interface",
        12 => "function",
        13 => "variable",
        14 => "constant",
        23 => "struct",
        26 => "type_parameter",
        _ => "symbol",
    };
    Some(SymbolMatch {
        excerpt_command: format!("frg excerpt {file}:{name}"),
        file,
        line,
        end_line: Some(end_line),
        kind: kind.to_string(),
        preview: name.to_string(),
    })
}

fn file_uri(path: &Path) -> String {
    let path = path.to_string_lossy().replace(' ', "%20");
    format!("file://{path}")
}

fn file_path(uri: &str) -> String {
    uri.strip_prefix("file://")
        .unwrap_or(uri)
        .replace("%20", " ")
}
