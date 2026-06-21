//! Integration test: verify the `ingest` tool is exposed via the MCP server
//! and returns valid structured output when called.

use std::io::Write;
use std::process::{Command, Stdio};

use serde_json::Value;

/// Send JSON-RPC requests to the MCP server and return the parsed responses.
/// Uses a temp HOME to avoid picking up local ferrosa-memory.toml config.
fn mcp_request(requests: &[Value]) -> Vec<Value> {
    let bin = env!("CARGO_BIN_EXE_frg");
    let tmp_home = tempfile::tempdir().expect("failed to create temp HOME");

    let mut child = Command::new(bin)
        .arg("--mcp")
        .env("HOME", tmp_home.path())
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
fn ingest_tool_appears_in_tools_list() {
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

    let ingest = tools.iter().find(|t| t["name"] == "ingest");
    assert!(ingest.is_some(), "ingest tool must appear in tools/list");

    let schema = &ingest.unwrap()["inputSchema"];
    assert!(
        schema["properties"].get("path").is_some(),
        "ingest tool should have a 'path' property"
    );
    assert!(
        schema["properties"].get("mcp_bin").is_some(),
        "ingest tool should have an 'mcp_bin' property"
    );

    // Also verify ingest_url tool is registered
    let ingest_url = tools.iter().find(|t| t["name"] == "ingest_url");
    assert!(
        ingest_url.is_some(),
        "ingest_url tool must appear in tools/list"
    );
    let url_schema = &ingest_url.unwrap()["inputSchema"];
    assert!(
        url_schema["properties"].get("url").is_some(),
        "ingest_url tool should have a 'url' property"
    );
    // ingest_url must accept the unified MCP-transport args so callers can
    // persist the extracted IngestReport — without persistence the tool
    // would return a fresh session_id that nothing in fmem has heard of
    // (phantom entities).  The `cql` arg from the retired Python/CQL loader
    // (0.6.x) was replaced by `mcp_bin` + `dry_run` when forge unified its
    // ingest path on the MCP transport in 0.8.0+.
    assert!(
        url_schema["properties"].get("mcp_bin").is_some(),
        "ingest_url tool should have an 'mcp_bin' property to enable MCP-transport persistence"
    );
    assert!(
        url_schema["properties"].get("dry_run").is_some(),
        "ingest_url tool should have a 'dry_run' property for extraction-only mode"
    );

    // ingest_paper has the same persistence contract.
    let ingest_paper = tools.iter().find(|t| t["name"] == "ingest_paper");
    assert!(
        ingest_paper.is_some(),
        "ingest_paper tool must appear in tools/list"
    );
    let paper_schema = &ingest_paper.unwrap()["inputSchema"];
    assert!(
        paper_schema["properties"].get("input").is_some(),
        "ingest_paper tool should have an 'input' property"
    );
    assert!(
        paper_schema["properties"].get("mcp_bin").is_some(),
        "ingest_paper tool should have an 'mcp_bin' property to enable MCP-transport persistence"
    );
    assert!(
        paper_schema["properties"].get("dry_run").is_some(),
        "ingest_paper tool should have a 'dry_run' property for extraction-only mode"
    );
    assert!(
        paper_schema["properties"].get("session").is_some(),
        "ingest_paper tool should have a 'session' property"
    );
    assert!(
        paper_schema["properties"].get("tenant").is_some(),
        "ingest_paper tool should have a 'tenant' property"
    );
}

#[test]
fn ingest_tool_returns_entities_and_edges() {
    // Point at the forge workspace root
    let workspace_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_string_lossy()
        .to_string();

    let responses = mcp_request(&[
        serde_json::json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}),
        serde_json::json!({
            "jsonrpc":"2.0","id":2,"method":"tools/call",
            // `dry_run: true` → extraction only, no ferrosa-memory call.
            // Without this the tool errors out (no MCP transport in test env),
            // which is the correct no-silent-extract-only behavior added in 0.8.2.
            "params":{"name":"ingest","arguments":{"path": workspace_root, "dry_run": true}}
        }),
    ]);

    let call_resp = responses
        .iter()
        .find(|r| r["id"] == 2)
        .expect("tools/call response missing");

    assert_eq!(
        call_resp["result"]["isError"], false,
        "ingest call should not be an error"
    );

    let text = call_resp["result"]["content"][0]["text"]
        .as_str()
        .expect("response should contain text");

    let report: Value = serde_json::from_str(text).expect("response text should be valid JSON");

    assert_eq!(report["language"], "rust");
    assert!(
        !report["entities"].as_array().unwrap().is_empty(),
        "should return entities"
    );
    assert!(
        !report["edges"].as_array().unwrap().is_empty(),
        "should return edges"
    );
    assert!(
        report["summary"]["total_entities"].as_u64().unwrap() > 0,
        "summary should have entity count"
    );
}
