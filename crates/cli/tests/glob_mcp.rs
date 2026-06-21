//! MCP contract tests for the `glob` tool.
//!
//! These tests exercise the full `frg --mcp` pipeline: spawn the binary,
//! send a JSON-RPC request over stdin, and assert the response shape.

use std::io::Write;
use std::process::{Command, Stdio};

fn frg_bin() -> std::path::PathBuf {
    let mut p = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.pop(); // crates/
    p.pop(); // forge/
    p.push("target");
    p.push("debug");
    p.push("frg");
    p
}

fn send_rpc(request: &str) -> String {
    let mut child = Command::new(frg_bin())
        .arg("--mcp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn frg --mcp");
    {
        let stdin = child.stdin.as_mut().expect("stdin");
        stdin.write_all(request.as_bytes()).expect("write request");
        stdin.write_all(b"\n").expect("write newline");
    }
    let output = child.wait_with_output().expect("wait for frg");
    String::from_utf8_lossy(&output.stdout).to_string()
}

#[test]
fn glob_tool_registered_and_returns_results() {
    // Build a hermetic fixture so the test doesn't depend on CWD.
    let td = tempfile::tempdir().expect("tempdir");
    let root = td.path();
    std::fs::create_dir_all(root.join("inner")).unwrap();
    std::fs::write(root.join("a.rs"), "fn a() {}\n").unwrap();
    std::fs::write(root.join("inner/b.rs"), "fn b() {}\n").unwrap();

    let pattern = format!("{}/**/*.rs", root.display());
    let req = format!(
        r#"{{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{{"name":"glob","arguments":{{"pattern":"{}","allow_absolute":true,"no_gitignore":true}}}}}}"#,
        pattern.replace('\\', "\\\\")
    );
    let out = send_rpc(&req);
    // The tool body is JSON-escaped inside the `text` field, so backslash-
    // quoting is expected in the raw bytes.
    assert!(out.contains(r#"\"$schema_version\":1"#), "got: {out}");
    assert!(out.contains("a.rs"), "got: {out}");
    assert!(out.contains("b.rs"), "got: {out}");
    assert!(out.contains(r#""isError":false"#), "got: {out}");
    assert!(out.contains(r#"\"total_matched\":2"#), "got: {out}");
}

#[test]
fn glob_tool_rejects_absolute_without_flag() {
    let req = r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"glob","arguments":{"pattern":"/etc/*"}}}"#;
    let out = send_rpc(req);
    // Error is reported in the JSON-RPC error channel or in the tool result;
    // either way "absolute" must appear in the response.
    assert!(out.to_lowercase().contains("absolute"), "got: {out}");
}

#[test]
fn glob_tool_rejects_parent_escape() {
    let req = r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"glob","arguments":{"pattern":"../../**"}}}"#;
    let out = send_rpc(req);
    assert!(out.contains(".."), "got: {out}");
}

#[test]
fn glob_tool_appears_in_list() {
    let req = r#"{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}"#;
    let out = send_rpc(req);
    assert!(out.contains("\"name\":\"glob\""), "got: {out}");
    // Verify read-only annotation survives the wire format.
    assert!(out.contains("\"readOnly\":true"), "got: {out}");
}
