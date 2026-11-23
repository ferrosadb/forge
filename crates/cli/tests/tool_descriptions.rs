//! Every MCP tool description must meet the house standard.
//!
//! A model chooses between 41 tools by their descriptions alone, so a stub
//! beside a well-written neighbour loses that choice every time. Measured
//! before this test existed: description length ran min 24 / median 467 /
//! max 771 characters, and the four shortest were all task-board tools —
//! `task_comment` said only "Add a comment to a task." while `find_definition`
//! got 400 characters of guidance.
//!
//! The standard is not "be long". It is: say **what it does**, **when to reach
//! for it**, and **what it returns** — the same three things the good ones say.

use serde_json::Value;
use std::process::Command;

/// The shortest description the house standard accepts.
///
/// Derived from the corpus: the tools that read well all clear 120 characters.
/// A shorter one cannot carry what/when/returns.
const MIN_CHARS: usize = 120;

fn tools() -> Vec<Value> {
    let bin = env!("CARGO_BIN_EXE_frg");
    let init = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"test","version":"0"}}}"#;
    let list = r#"{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}"#;
    let out = Command::new(bin)
        .arg("--mcp")
        .env_clear()
        .env("HOME", std::env::var("HOME").unwrap_or_default())
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            use std::io::Write;
            let stdin = child.stdin.as_mut().expect("stdin");
            stdin.write_all(format!("{init}\n{list}\n").as_bytes())?;
            child.wait_with_output()
        })
        .expect("frg --mcp");
    let stdout = String::from_utf8_lossy(&out.stdout);
    for line in stdout.lines() {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if let Some(tools) = value["result"]["tools"].as_array() {
            return tools.clone();
        }
    }
    panic!("no tools/list response in: {stdout}");
}

#[test]
fn no_tool_description_is_a_stub() {
    let tools = tools();
    assert!(
        tools.len() > 30,
        "expected the full tool set, got {}",
        tools.len()
    );

    let mut stubs = Vec::new();
    for tool in &tools {
        let name = tool["name"].as_str().unwrap_or_default();
        let description = tool["description"].as_str().unwrap_or_default();
        if description.trim().chars().count() < MIN_CHARS {
            stubs.push(format!("{name} ({} chars)", description.chars().count()));
        }
    }
    assert!(
        stubs.is_empty(),
        "these descriptions are below the {MIN_CHARS}-char standard and will \
         lose a model's tool choice to a better-described neighbour: {stubs:?}"
    );
}

#[test]
fn no_tool_description_names_another_server() {
    // Forge answers with its own tools. A description that advertises a
    // foreign namespace teaches the model a name it cannot call.
    let tools = tools();
    let mut offenders = Vec::new();
    for tool in &tools {
        let name = tool["name"].as_str().unwrap_or_default();
        let description = tool["description"].as_str().unwrap_or_default();
        for foreign in ["oh-my-claudecode", "mcp__plugin_"] {
            if description.contains(foreign) {
                offenders.push(format!("{name} names {foreign}"));
            }
        }
    }
    assert!(offenders.is_empty(), "{offenders:?}");
}
