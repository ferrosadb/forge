//! Integration tests for `frg tool-aliases` command

use serde_json::Value;
use std::process::Command;

/// Run the frg tool-aliases command and return stdout
fn run_aliases_cmd(args: &[&str]) -> String {
    let bin = env!("CARGO_BIN_EXE_frg");
    let output = Command::new(bin)
        .args(args)
        .output()
        .expect("failed to execute frg tool-aliases");

    String::from_utf8_lossy(&output.stdout).to_string()
}

#[test]
fn tool_aliases_json_returns_valid_json() {
    let output = run_aliases_cmd(&["tool-aliases", "--format", "json"]);

    // Parse the JSON - this will panic if invalid
    let value: Value = serde_json::from_str(&output)
        .expect("tool-aliases --format json did not return valid JSON");

    // Verify it's an object, not array or primitive
    assert!(value.is_object(), "Expected JSON object, got {:?}", value);
}

#[test]
fn tool_aliases_json_has_version_field() {
    let output = run_aliases_cmd(&["tool-aliases", "--format", "json"]);
    let value: Value = serde_json::from_str(&output).unwrap();

    let version = value.get("version").expect("Missing 'version' field");
    assert_eq!(version, 1, "Expected version to be 1");
}

#[test]
fn tool_aliases_json_has_canonical_tools_list() {
    let output = run_aliases_cmd(&["tool-aliases", "--format", "json"]);
    let value: Value = serde_json::from_str(&output).unwrap();

    let canonical_tools = value
        .get("canonical_tools")
        .expect("Missing 'canonical_tools' field");
    assert!(
        canonical_tools.is_array(),
        "canonical_tools should be an array"
    );

    let tools = canonical_tools.as_array().unwrap();
    assert!(!tools.is_empty(), "canonical_tools should not be empty");

    // Verify expected tools are present
    let tool_names: Vec<&str> = tools.iter().filter_map(|v| v.as_str()).collect();

    assert!(
        tool_names.contains(&"foundry.edit_file"),
        "Missing foundry.edit_file"
    );
    assert!(
        tool_names.contains(&"foundry.execute_command"),
        "Missing foundry.execute_command"
    );
    assert!(tool_names.contains(&"forge.digest"), "Missing forge.digest");
}

#[test]
fn tool_aliases_json_has_builtin_aliases() {
    let output = run_aliases_cmd(&["tool-aliases", "--format", "json"]);
    let value: Value = serde_json::from_str(&output).unwrap();

    let aliases = value.get("aliases").expect("Missing 'aliases' field");
    assert!(aliases.is_object(), "aliases should be an object");

    let alias_map = aliases.as_object().unwrap();

    // Verify key aliases exist
    assert_eq!(
        alias_map.get("Edit").and_then(|v| v.as_str()),
        Some("foundry.edit_file")
    );
    assert_eq!(
        alias_map.get("Bash").and_then(|v| v.as_str()),
        Some("foundry.execute_command")
    );
    assert_eq!(
        alias_map.get("Read").and_then(|v| v.as_str()),
        Some("foundry.read_file")
    );
    assert_eq!(
        alias_map.get("Write").and_then(|v| v.as_str()),
        Some("foundry.write_file")
    );

    // Verify new convenience aliases
    assert_eq!(
        alias_map.get("forge_clippy").and_then(|v| v.as_str()),
        Some("cargo")
    );
    assert_eq!(
        alias_map.get("clippy").and_then(|v| v.as_str()),
        Some("cargo")
    );
    assert_eq!(
        alias_map.get("todo_write").and_then(|v| v.as_str()),
        Some("todowrite")
    );
    assert_eq!(
        alias_map.get("TodoWrite").and_then(|v| v.as_str()),
        Some("todowrite")
    );
}

#[test]
fn tool_aliases_table_format_prints_human_readable() {
    let output = run_aliases_cmd(&["tool-aliases", "--format", "table"]);

    // Table format should NOT be valid JSON
    assert!(
        !output.trim().starts_with('{'),
        "Table format should not be JSON"
    );

    // Should contain expected headers
    assert!(output.contains("Forge Tool Aliases"), "Missing header");
    assert!(
        output.contains("Canonical Tools:"),
        "Missing Canonical Tools section"
    );
    assert!(output.contains("Aliases:"), "Missing Aliases section");
}

#[test]
fn tool_aliases_has_fuzzy_suggestions() {
    let output = run_aliases_cmd(&["tool-aliases", "--format", "json"]);
    let value: Value = serde_json::from_str(&output).unwrap();

    let fuzzy = value
        .get("fuzzy_suggestions")
        .expect("Missing 'fuzzy_suggestions' field");
    assert!(fuzzy.is_object(), "fuzzy_suggestions should be an object");
}

#[test]
fn tool_aliases_merges_project_local_config() {
    use std::fs;
    use tempfile::TempDir;

    // Create a temp directory with a project-local alias file
    let temp_dir = TempDir::new().unwrap();
    let alias_file = temp_dir.path().join("forge-aliases.toml");

    fs::write(
        &alias_file,
        r#"
[[alias]]
from = "my_custom_edit"
to = "foundry.edit_file"
"#,
    )
    .unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_frg"))
        .args(["tool-aliases", "--format", "json"])
        .current_dir(temp_dir.path())
        .output()
        .expect("failed to execute frg tool-aliases");

    let value: Value = serde_json::from_str(&String::from_utf8_lossy(&output.stdout)).unwrap();
    let aliases = value.get("aliases").unwrap().as_object().unwrap();

    assert!(
        aliases.contains_key("my_custom_edit"),
        "Project-local alias not merged"
    );
    assert_eq!(
        aliases.get("my_custom_edit").unwrap().as_str(),
        Some("foundry.edit_file")
    );
}

#[test]
fn tool_aliases_mcp_tool_registered() {
    use std::io::Write;
    use std::process::{Command, Stdio};

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

    let requests = vec![
        serde_json::json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}),
        serde_json::json!({"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}),
    ];

    {
        let stdin = child.stdin.as_mut().unwrap();
        for req in &requests {
            serde_json::to_writer(&mut *stdin, req).unwrap();
            stdin.write_all(b"\n").unwrap();
        }
    }

    let output = child.wait_with_output().expect("failed to read stdout");
    let stdout = String::from_utf8_lossy(&output.stdout);

    let responses: Vec<Value> = stdout
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).expect("invalid JSON response"))
        .collect();

    let tools_resp = responses
        .iter()
        .find(|r| r.get("id").and_then(|v| v.as_u64()) == Some(2))
        .expect("No tools/list response");

    let tools = tools_resp
        .get("result")
        .and_then(|r| r.get("tools"))
        .and_then(|t| t.as_array())
        .expect("No tools in response");

    let tool_aliases_tool = tools
        .iter()
        .find(|t| t.get("name").and_then(|v| v.as_str()) == Some("tool_aliases"))
        .expect("tool_aliases MCP tool not found");

    assert_eq!(
        tool_aliases_tool
            .get("annotations")
            .and_then(|a| a.get("readOnly"))
            .and_then(|v| v.as_bool()),
        Some(true)
    );
}
