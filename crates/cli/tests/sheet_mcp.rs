//! MCP contract tests for the `sheet_auth`/`sheet_pull`/`sheet_push` tools.
//!
//! These exercise the full `frg --mcp` pipeline: spawn the binary, send
//! JSON-RPC requests over stdin, and assert the response shape. Only the
//! fail-loud bad-alias path is exercised here (no live OAuth client or CQL
//! cluster available in CI) — the happy path is covered by the crate's own
//! `sync`/`board_exec` unit tests against fakes.

use std::io::Write;
use std::process::{Command, Stdio};

use serde_json::Value;

/// Uses a temp HOME so the server doesn't pick up any local `.forge/config.toml`.
fn mcp_request(requests: &[Value]) -> Vec<Value> {
    let bin = env!("CARGO_BIN_EXE_frg");
    let tmp_home = tempfile::tempdir().expect("failed to create temp HOME");

    let mut child = Command::new(bin)
        .arg("--mcp")
        .env("HOME", tmp_home.path())
        .current_dir(tmp_home.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to start frg --mcp");

    {
        let stdin = child.stdin.as_mut().unwrap();
        for req in requests {
            serde_json::to_writer(&mut *stdin, req).unwrap();
            stdin.write_all(b"\n").unwrap();
        }
    }

    let output = child.wait_with_output().expect("failed to read stdout");
    let stdout = String::from_utf8_lossy(&output.stdout);

    stdout
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).expect("invalid JSON response"))
        .collect()
}

#[test]
fn sheet_tools_appear_in_tools_list() {
    let responses = mcp_request(&[
        serde_json::json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}),
        serde_json::json!({"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}),
    ]);

    let list_resp = responses
        .iter()
        .find(|r| r["id"] == 2)
        .expect("tools/list response missing");
    let tools = list_resp["result"]["tools"]
        .as_array()
        .expect("tools should be an array");

    for name in ["sheet_auth", "sheet_pull", "sheet_push"] {
        let tool = tools.iter().find(|t| t["name"] == name);
        assert!(tool.is_some(), "{name} tool must appear in tools/list");
        assert_eq!(
            tool.unwrap()["inputSchema"]["properties"]["alias"]["type"],
            "string",
            "{name} tool should have an 'alias' string property"
        );
    }

    let pull_schema = &tools.iter().find(|t| t["name"] == "sheet_pull").unwrap()["inputSchema"];
    assert!(pull_schema["properties"].get("dry_run").is_some());
    assert!(pull_schema["properties"].get("cql_host").is_some());

    let push_schema = &tools.iter().find(|t| t["name"] == "sheet_push").unwrap()["inputSchema"];
    assert!(push_schema["properties"].get("row_id").is_some());
    assert!(push_schema["properties"].get("status").is_some());
    assert!(push_schema["properties"].get("fix_ver").is_some());
    assert!(push_schema["properties"].get("notes").is_some());
}

#[test]
fn sheet_pull_nonexistent_alias_fails_loud_over_mcp() {
    let req = serde_json::json!({
        "jsonrpc":"2.0","id":1,"method":"tools/call",
        "params":{"name":"sheet_pull","arguments":{"alias":"__no_such_alias__"}}
    });
    let responses = mcp_request(&[req]);
    let resp = responses
        .iter()
        .find(|r| r["id"] == 1)
        .expect("tools/call response missing");

    assert_eq!(
        resp["result"]["isError"], true,
        "expected isError:true for a missing alias, got: {resp}"
    );
    let text = resp["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_default();
    assert!(
        text.contains("__no_such_alias__"),
        "expected the error text to name the missing alias, got: {text}"
    );
}

#[test]
fn sheet_auth_nonexistent_alias_fails_loud_over_mcp() {
    let req = serde_json::json!({
        "jsonrpc":"2.0","id":1,"method":"tools/call",
        "params":{"name":"sheet_auth","arguments":{"alias":"__no_such_alias__"}}
    });
    let responses = mcp_request(&[req]);
    let resp = responses
        .iter()
        .find(|r| r["id"] == 1)
        .expect("tools/call response missing");

    assert_eq!(
        resp["result"]["isError"], true,
        "expected isError:true for a missing alias, got: {resp}"
    );
}

#[test]
fn sheet_push_nonexistent_alias_fails_loud_over_mcp() {
    let req = serde_json::json!({
        "jsonrpc":"2.0","id":1,"method":"tools/call",
        "params":{"name":"sheet_push","arguments":{"alias":"__no_such_alias__","row_id":"ROW-1"}}
    });
    let responses = mcp_request(&[req]);
    let resp = responses
        .iter()
        .find(|r| r["id"] == 1)
        .expect("tools/call response missing");

    assert_eq!(
        resp["result"]["isError"], true,
        "expected isError:true for a missing alias, got: {resp}"
    );
}
