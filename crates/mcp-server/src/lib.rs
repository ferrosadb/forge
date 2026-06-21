use std::collections::VecDeque;
use std::io::{self, BufRead, Write};
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

fn debug_log(msg: &str) {
    if std::env::var_os("FORGE_MCP_DEBUG").is_some() {
        eprintln!("{msg}");
    }
}

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// An MCP tool definition sent in `tools/list` responses.
#[derive(Debug, Clone, Serialize, Default)]
pub struct ToolDef {
    pub name: String,
    pub description: String,
    #[serde(rename = "inputSchema")]
    pub input_schema: Value,
    /// Tier for progressive disclosure: 1=always visible, 2=stack-detected, 3=on-demand.
    /// Not serialized in the MCP wire format. Defaults to 1 (always visible).
    #[serde(skip, default)]
    pub tier: u8,
    /// Optional annotations for tool behavior
    #[serde(skip_serializing_if = "Option::is_none")]
    pub annotations: Option<ToolAnnotations>,
}

/// Annotations for MCP tools
#[derive(Debug, Clone, Serialize, Default)]
pub struct ToolAnnotations {
    /// Whether the tool is read-only (doesn't modify state)
    #[serde(rename = "readOnly")]
    pub read_only: bool,
}

/// Result of calling a tool.
#[derive(Debug, Serialize)]
pub struct ToolResult {
    pub content: Vec<ContentBlock>,
    #[serde(rename = "isError")]
    pub is_error: bool,
}

/// A single content block inside a tool result.
#[derive(Debug, Serialize)]
pub struct ContentBlock {
    #[serde(rename = "type")]
    pub content_type: String,
    pub text: String,
}

/// A handler function that receives tool arguments and returns a result string
/// or an error string.
pub type ToolHandler = Arc<dyn Fn(Value) -> Result<String, String> + Send + Sync + 'static>;

/// A tool together with its handler.
#[derive(Clone)]
pub struct ToolRegistration {
    pub def: ToolDef,
    pub handler: ToolHandler,
}

// ---------------------------------------------------------------------------
// JSON-RPC wire types (internal)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct RpcRequest {
    #[allow(dead_code)]
    jsonrpc: String,
    /// `None` for notifications.
    id: Option<Value>,
    method: String,
    #[serde(default)]
    params: Value,
}

enum ReaderEvent {
    Request(RpcRequest),
    Cancelled(String),
    Malformed(String),
    ReadError(String),
    Closed,
}

// ---------------------------------------------------------------------------
// Server
// ---------------------------------------------------------------------------

/// A JSON-RPC 2.0 MCP stdio server.
pub struct McpServer {
    tools: Vec<ToolRegistration>,
    server_name: String,
    server_version: String,
    /// Detected project stacks (e.g., "rust", "elixir"). Set via `set_detected_stacks`.
    detected_stacks: Vec<String>,
}

impl McpServer {
    /// Create a new server with the given name and version.
    pub fn new(name: &str, version: &str) -> Self {
        Self {
            tools: Vec::new(),
            server_name: name.to_owned(),
            server_version: version.to_owned(),
            detected_stacks: Vec::new(),
        }
    }

    /// Register a tool with its definition and handler.
    pub fn register_tool(&mut self, def: ToolDef, handler: ToolHandler) {
        self.tools.push(ToolRegistration { def, handler });
    }

    /// Set the detected project stacks for tier-2 tool filtering.
    pub fn set_detected_stacks(&mut self, stacks: Vec<String>) {
        self.detected_stacks = stacks;
    }

    /// Return the list of tool definitions visible for the given stacks,
    /// applying tier-based filtering (tier 1 always, tier 2 if stack matches,
    /// tier 3 never).
    pub fn tool_defs_visible(&self, stacks: &[String]) -> Vec<&ToolDef> {
        self.tools
            .iter()
            .map(|r| &r.def)
            .filter(|d| match d.tier {
                1 => true,
                2 => is_tool_for_stacks(&d.name, stacks),
                _ => false,
            })
            .collect()
    }

    /// Run the server loop: read JSONL from stdin, write JSONL to stdout.
    pub fn run(&self) -> Result<()> {
        let stdout = io::stdout();
        let mut out = stdout.lock();
        let (tx, rx) = mpsc::channel();

        thread::spawn(move || {
            let stdin = io::stdin();
            for line in stdin.lock().lines() {
                let line = match line {
                    Ok(l) => l,
                    Err(e) => {
                        let _ = tx.send(ReaderEvent::ReadError(e.to_string()));
                        return;
                    }
                };

                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }

                match serde_json::from_str::<RpcRequest>(trimmed) {
                    Ok(request) => {
                        if request.id.is_none() && request.method == "notifications/cancelled" {
                            let request_id = request
                                .params
                                .get("requestId")
                                .cloned()
                                .unwrap_or(Value::Null)
                                .to_string();
                            let _ = tx.send(ReaderEvent::Cancelled(request_id));
                        } else {
                            let _ = tx.send(ReaderEvent::Request(request));
                        }
                    }
                    Err(e) => {
                        let _ = tx.send(ReaderEvent::Malformed(e.to_string()));
                    }
                }
            }

            let _ = tx.send(ReaderEvent::Closed);
        });

        let mut pending = VecDeque::new();

        loop {
            let event = match pending.pop_front() {
                Some(event) => event,
                None => match rx.recv() {
                    Ok(event) => event,
                    Err(_) => ReaderEvent::Closed,
                },
            };

            match event {
                ReaderEvent::Request(request) => {
                    debug_log(&format!("[mcp-server] received method={}", request.method));

                    let id = match &request.id {
                        Some(id) => id.clone(),
                        None => {
                            self.handle_notification(&request.method, &request.params);
                            continue;
                        }
                    };

                    let response = match self.handle_request_until_cancelled(
                        id,
                        &request.method,
                        &request.params,
                        &rx,
                        &mut pending,
                    ) {
                        RequestOutcome::Response(response) => response,
                        RequestOutcome::Cancelled => {
                            debug_log("[mcp-server] request cancelled");
                            continue;
                        }
                    };

                    let mut bytes = serde_json::to_vec(&response)?;
                    bytes.push(b'\n');
                    out.write_all(&bytes)?;
                    out.flush()?;
                }
                ReaderEvent::Cancelled(_) => {
                    debug_log("[mcp-server] cancellation for unknown/inactive request");
                }
                ReaderEvent::Malformed(e) => {
                    eprintln!("[mcp-server] malformed JSON, skipping: {e}");
                }
                ReaderEvent::ReadError(e) => {
                    eprintln!("[mcp-server] stdin read error: {e}");
                    break;
                }
                ReaderEvent::Closed => break,
            }
        }

        debug_log("[mcp-server] stdin closed, exiting");
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Internal dispatch
    // -----------------------------------------------------------------------

    fn handle_notification(&self, method: &str, _params: &Value) {
        debug_log(&format!(
            "[mcp-server] notification: {method} (no response sent)"
        ));
    }

    fn handle_request(&self, id: Value, method: &str, params: &Value) -> Value {
        match method {
            "initialize" => self.respond_initialize(id),
            "tools/list" => self.respond_tools_list(id),
            "tools/call" => self.respond_tools_call(id, params),
            _ => {
                eprintln!("[mcp-server] unknown method: {method}");
                json_error(id, -32601, "Method not found")
            }
        }
    }

    fn handle_request_until_cancelled(
        &self,
        id: Value,
        method: &str,
        params: &Value,
        rx: &mpsc::Receiver<ReaderEvent>,
        pending: &mut VecDeque<ReaderEvent>,
    ) -> RequestOutcome {
        if method != "tools/call" {
            return RequestOutcome::Response(self.handle_request(id, method, params));
        }

        let request_id = id.to_string();
        let params = params.clone();
        let reg = self
            .tool_registration(
                params
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default(),
            )
            .cloned();
        let (result_tx, result_rx) = mpsc::channel();

        thread::spawn(move || {
            let response = match reg {
                None => {
                    let name = params
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string();
                    if name.is_empty() {
                        json_error(id, -32602, "Missing required param: name")
                    } else {
                        json_error(id, -32601, &format!("Tool not found: {name}"))
                    }
                }
                Some(reg) => respond_tool_call(id, &reg, &params),
            };
            let _ = result_tx.send(response);
        });

        loop {
            if let Ok(response) = result_rx.try_recv() {
                return RequestOutcome::Response(response);
            }

            match rx.recv_timeout(Duration::from_millis(50)) {
                Ok(ReaderEvent::Request(request)) => {
                    pending.push_back(ReaderEvent::Request(request))
                }
                Ok(ReaderEvent::Cancelled(cancelled_id)) => {
                    if cancelled_id == request_id {
                        return RequestOutcome::Cancelled;
                    }
                    pending.push_back(ReaderEvent::Cancelled(cancelled_id));
                }
                Ok(ReaderEvent::Malformed(e)) => pending.push_back(ReaderEvent::Malformed(e)),
                Ok(ReaderEvent::ReadError(e)) => pending.push_back(ReaderEvent::ReadError(e)),
                Ok(ReaderEvent::Closed) => pending.push_back(ReaderEvent::Closed),
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => pending.push_back(ReaderEvent::Closed),
            }
        }
    }

    fn respond_initialize(&self, id: Value) -> Value {
        json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "protocolVersion": "2024-11-05",
                "capabilities": {
                    "tools": {"listChanged": false}
                },
                "serverInfo": {
                    "name": self.server_name,
                    "version": self.server_version
                }
            }
        })
    }

    fn respond_tools_list(&self, id: Value) -> Value {
        let defs = self.tool_defs_visible(&self.detected_stacks);
        json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "tools": defs
            }
        })
    }

    fn respond_tools_call(&self, id: Value, params: &Value) -> Value {
        let name = match params.get("name").and_then(|v| v.as_str()) {
            Some(n) => n,
            None => {
                return json_error(id, -32602, "Missing required param: name");
            }
        };

        match self.tool_registration(name) {
            None => json_error(id, -32601, &format!("Tool not found: {name}")),
            Some(reg) => respond_tool_call(id, reg, params),
        }
    }

    fn tool_registration(&self, name: &str) -> Option<&ToolRegistration> {
        self.tools.iter().find(|r| r.def.name == name)
    }
}

enum RequestOutcome {
    Response(Value),
    Cancelled,
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Check if a tier-2 tool matches any of the detected project stacks.
fn is_tool_for_stacks(tool_name: &str, stacks: &[String]) -> bool {
    let required_stacks: &[&str] = match tool_name {
        "cargo" | "clippy" => &["rust"],
        "dotnet_tools" => &["c#"],
        "mix_compile" | "mix_test" | "mix_format_check" | "mix_deps" => &["elixir"],
        "npm_tools" => &["javascript", "typescript"],
        "python_tools" => &["python"],
        "go_tools" => &["go"],
        _ => return true, // unknown tier-2 tool — show by default
    };

    stacks.iter().any(|s| required_stacks.contains(&s.as_str()))
}

fn json_error(id: Value, code: i32, message: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": code,
            "message": message
        }
    })
}

fn respond_tool_call(id: Value, reg: &ToolRegistration, params: &Value) -> Value {
    let arguments = params.get("arguments").cloned().unwrap_or(Value::Null);
    let started = Instant::now();
    let tool_name = reg.def.name.clone();
    debug_log(&format!("[mcp-server] tools/call start name={tool_name}"));
    let (text, is_error) = match (reg.handler)(arguments) {
        Ok(s) => (s, false),
        Err(e) => (format!("Error: {e}"), true),
    };
    let duration_ms = started.elapsed().as_millis() as u64;
    debug_log(&format!(
        "[mcp-server] tools/call finish name={tool_name} duration_ms={duration_ms} is_error={is_error}"
    ));
    // Per the MCP spec, `structuredContent` must be the tool's structured RESULT —
    // clients that render it then show the data, not a metadata envelope. Wrap the
    // payload under `result` (structuredContent must be an object) and move call
    // metadata to `_meta`. `content[0].text` stays as the fallback for clients that
    // don't read structuredContent.
    let result_value: Value =
        serde_json::from_str::<Value>(&text).unwrap_or_else(|_| Value::String(text.clone()));
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": {
            "content": [{"type": "text", "text": text}],
            "structuredContent": { "result": result_value },
            "_meta": {
                "tool": tool_name,
                "duration_ms": duration_ms,
                "is_error": is_error
            },
            "isError": is_error
        }
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn make_server() -> McpServer {
        McpServer::new("test-server", "0.1.0")
    }

    fn add_echo_tool(server: &mut McpServer) {
        server.register_tool(
            ToolDef {
                name: "echo".to_owned(),
                description: "Echoes its input".to_owned(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "text": {"type": "string"}
                    },
                    "required": ["text"]
                }),
                tier: 1,
                annotations: None,
            },
            std::sync::Arc::new(|args| {
                let text = args
                    .get("text")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_owned();
                Ok(text)
            }),
        );
    }

    fn add_failing_tool(server: &mut McpServer) {
        server.register_tool(
            ToolDef {
                name: "fail".to_owned(),
                description: "Always fails".to_owned(),
                input_schema: json!({"type": "object", "properties": {}}),
                tier: 1,
                annotations: None,
            },
            std::sync::Arc::new(|_| Err("intentional failure".to_owned())),
        );
    }

    // -----------------------------------------------------------------------

    #[test]
    fn test_tool_registration_and_listing() {
        let mut server = make_server();
        assert_eq!(server.tools.len(), 0);

        add_echo_tool(&mut server);
        assert_eq!(server.tools.len(), 1);
        assert_eq!(server.tools[0].def.name, "echo");

        add_failing_tool(&mut server);
        assert_eq!(server.tools.len(), 2);

        // tools/list response
        let resp = server.respond_tools_list(json!(1));
        let tools = &resp["result"]["tools"];
        assert!(tools.is_array());
        let arr = tools.as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["name"], "echo");
        assert_eq!(arr[1]["name"], "fail");
    }

    #[test]
    fn test_initialize_response() {
        let server = make_server();
        let resp = server.respond_initialize(json!(42));

        assert_eq!(resp["jsonrpc"], "2.0");
        assert_eq!(resp["id"], 42);
        assert_eq!(resp["result"]["protocolVersion"], "2024-11-05");
        assert_eq!(resp["result"]["serverInfo"]["name"], "test-server");
        assert_eq!(resp["result"]["serverInfo"]["version"], "0.1.0");
        assert_eq!(
            resp["result"]["capabilities"]["tools"]["listChanged"],
            false
        );
    }

    #[test]
    fn test_initialize_response_string_id() {
        let server = make_server();
        let resp = server.respond_initialize(json!("req-abc"));
        assert_eq!(resp["id"], "req-abc");
    }

    #[test]
    fn test_tool_call_success() {
        let mut server = make_server();
        add_echo_tool(&mut server);

        let params = json!({"name": "echo", "arguments": {"text": "hello world"}});
        let resp = server.respond_tools_call(json!(1), &params);

        assert_eq!(resp["jsonrpc"], "2.0");
        assert_eq!(resp["id"], 1);
        assert_eq!(resp["result"]["isError"], false);
        let content = &resp["result"]["content"];
        assert!(content.is_array());
        let block = &content[0];
        assert_eq!(block["type"], "text");
        assert_eq!(block["text"], "hello world");
        // structuredContent carries the actual result; metadata lives in _meta.
        assert_eq!(resp["result"]["structuredContent"]["result"], "hello world");
        assert_eq!(resp["result"]["_meta"]["tool"], "echo");
        assert!(resp["result"]["_meta"]["duration_ms"].is_number());
        assert_eq!(resp["result"]["_meta"]["is_error"], false);
    }

    #[test]
    fn test_tool_call_error_response() {
        let mut server = make_server();
        add_failing_tool(&mut server);

        let params = json!({"name": "fail", "arguments": {}});
        let resp = server.respond_tools_call(json!(2), &params);

        assert_eq!(resp["result"]["isError"], true);
        let text = resp["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.starts_with("Error:"));
        assert!(text.contains("intentional failure"));
    }

    #[test]
    fn test_tool_call_missing_name_param() {
        let server = make_server();
        let params = json!({"arguments": {}});
        let resp = server.respond_tools_call(json!(3), &params);

        assert!(resp.get("error").is_some());
        assert_eq!(resp["error"]["code"], -32602);
    }

    #[test]
    fn test_tool_call_unknown_tool() {
        let server = make_server();
        let params = json!({"name": "nonexistent", "arguments": {}});
        let resp = server.respond_tools_call(json!(4), &params);

        assert!(resp.get("error").is_some());
        assert_eq!(resp["error"]["code"], -32601);
        assert!(resp["error"]["message"]
            .as_str()
            .unwrap()
            .contains("nonexistent"));
    }

    #[test]
    fn test_unknown_method() {
        let server = make_server();
        let resp = server.handle_request(json!(5), "foo/bar", &json!({}));

        assert!(resp.get("error").is_some());
        assert_eq!(resp["error"]["code"], -32601);
        assert_eq!(resp["error"]["message"], "Method not found");
    }

    #[test]
    fn test_tool_def_serializes_input_schema() {
        let def = ToolDef {
            name: "my_tool".to_owned(),
            description: "does stuff".to_owned(),
            input_schema: json!({"type": "object", "properties": {}}),
            tier: 1,
            annotations: None,
        };
        let v = serde_json::to_value(&def).unwrap();
        // camelCase rename
        assert!(v.get("inputSchema").is_some());
        assert!(v.get("input_schema").is_none());
    }

    #[test]
    fn test_tool_call_null_arguments() {
        // arguments key absent — handler receives Value::Null
        let mut server = make_server();
        server.register_tool(
            ToolDef {
                name: "nullary".to_owned(),
                description: "takes no args".to_owned(),
                input_schema: json!({"type": "object", "properties": {}}),
                tier: 1,
                annotations: None,
            },
            std::sync::Arc::new(|args| {
                if args.is_null() {
                    Ok("null args ok".to_owned())
                } else {
                    Ok("non-null".to_owned())
                }
            }),
        );

        let params = json!({"name": "nullary"});
        let resp = server.respond_tools_call(json!(10), &params);
        assert_eq!(resp["result"]["isError"], false);
        assert_eq!(resp["result"]["content"][0]["text"], "null args ok");
    }
}
