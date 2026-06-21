//! Symbol lookup with LSP detection and fallback.
//!
//! Provides `frg lookup <symbol>` — detects if oh-my-claudecode's LSP MCP
//! tools are available. If yes, suggests using them. Otherwise, falls back to
//! grep-based symbol search across files + excerpt extraction.

use std::path::Path;

use serde::Serialize;

use crate::excerpt;
use crate::summarizer::{self, ElementKind};

#[derive(Debug, Serialize)]
pub struct LookupResult {
    pub method: String,
    pub lsp_available: bool,
    pub matches: Vec<SymbolMatch>,
}

#[derive(Debug, Serialize)]
pub struct SymbolMatch {
    pub file: String,
    pub line: usize,
    pub end_line: Option<usize>,
    pub kind: String,
    pub preview: String,
    pub excerpt_command: String,
}

/// Check if oh-my-claudecode LSP MCP tools are likely available.
/// Looks for the plugin in ~/.claude/settings.json.
pub fn lsp_available() -> bool {
    let settings_path = dirs::home_dir()
        .map(|h| h.join(".claude").join("settings.json"))
        .unwrap_or_default();

    if !settings_path.exists() {
        return false;
    }

    std::fs::read_to_string(&settings_path)
        .map(|content| content.contains("oh-my-claudecode"))
        .unwrap_or(false)
}

/// Look up a symbol across all source files in a directory.
///
/// If LSP is available, returns a suggestion to use LSP tools.
/// Otherwise, scans all files using digest and returns matches with
/// ready-to-use excerpt commands.
pub fn lookup_symbol(symbol: &str, dir: &Path) -> anyhow::Result<LookupResult> {
    let has_lsp = lsp_available();

    let mut matches = Vec::new();

    // Walk source files
    let walker = ignore::WalkBuilder::new(dir)
        .hidden(true)
        .git_ignore(true)
        .build();

    for entry in walker.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }

        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        if !is_source_ext(ext) {
            continue;
        }

        let source = match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(_) => continue,
        };

        let filename = path.display().to_string();
        let digest = summarizer::summarize(&filename, &source);

        for elem in &digest.elements {
            if !elem.text.contains(symbol) {
                continue;
            }
            // Skip imports — we want definitions
            if elem.kind == ElementKind::Import {
                continue;
            }

            let kind_str = match elem.kind {
                ElementKind::Function => "function",
                ElementKind::Struct => "struct",
                ElementKind::Enum => "enum",
                ElementKind::Trait => "trait",
                ElementKind::Interface => "interface",
                ElementKind::Class => "class",
                ElementKind::Constant => "constant",
                ElementKind::TypeAlias => "type_alias",
                ElementKind::Module => "module",
                ElementKind::TestBlock => "test_block",
                ElementKind::Import => unreachable!(),
            };

            matches.push(SymbolMatch {
                file: filename.clone(),
                line: elem.line,
                end_line: elem.end_line,
                kind: kind_str.to_string(),
                preview: elem.text.clone(),
                excerpt_command: format!("frg excerpt {}:{}", filename, symbol),
            });
        }
    }

    Ok(LookupResult {
        method: if has_lsp {
            "lsp_hint".to_string()
        } else {
            "grep".to_string()
        },
        lsp_available: has_lsp,
        matches,
    })
}

/// Format lookup results for human/LLM consumption.
pub fn format_lookup(result: &LookupResult) -> String {
    let mut out = String::new();

    if result.lsp_available {
        out.push_str("LSP available — for precise results, use:\n");
        out.push_str("  mcp__plugin_oh-my-claudecode_t__lsp_workspace_symbols\n");
        out.push_str("  mcp__plugin_oh-my-claudecode_t__lsp_goto_definition\n\n");
    }

    if result.matches.is_empty() {
        out.push_str("No matches found.\n");
        return out;
    }

    out.push_str(&format!("Found {} matches:\n", result.matches.len()));
    for m in &result.matches {
        let span = match m.end_line {
            Some(end) => format!("L{}-{}", m.line, end),
            None => format!("L{}", m.line),
        };
        out.push_str(&format!(
            "  {} {:>10}  {} [{}]\n",
            m.file, span, m.preview, m.kind
        ));
    }

    if !result.matches.is_empty() {
        out.push_str("\nTo extract a symbol's full body:\n");
        out.push_str(&format!("  {}\n", result.matches[0].excerpt_command));
    }

    out
}

fn is_source_ext(ext: &str) -> bool {
    matches!(
        ext,
        "rs" | "py"
            | "go"
            | "ts"
            | "tsx"
            | "js"
            | "jsx"
            | "ex"
            | "exs"
            | "cpp"
            | "cc"
            | "h"
            | "hpp"
            | "swift"
            | "java"
            | "rb"
            | "kt"
    )
}

/// Try to extract a specific symbol using the excerpt module.
/// Returns the excerpt body if found, or None.
pub fn extract_and_format(filename: &str, source: &str, symbol: &str) -> Option<String> {
    excerpt::extract_symbol(filename, source, symbol).map(|result| {
        let mut out = format!(
            "// {} :: {} (L{}-{})\n",
            result.file, result.symbol, result.start_line, result.end_line
        );
        // Add line numbers to body
        for (i, line) in result.body.lines().enumerate() {
            out.push_str(&format!("{:>5}  {}\n", result.start_line + i, line));
        }
        out
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_source_ext_filters_correctly() {
        assert!(is_source_ext("rs"));
        assert!(is_source_ext("py"));
        assert!(!is_source_ext("md"));
        assert!(!is_source_ext("toml"));
    }

    #[test]
    fn format_lookup_empty_results() {
        let result = LookupResult {
            method: "grep".to_string(),
            lsp_available: false,
            matches: vec![],
        };
        let out = format_lookup(&result);
        assert!(out.contains("No matches found"));
    }

    #[test]
    fn format_lookup_with_matches() {
        let result = LookupResult {
            method: "grep".to_string(),
            lsp_available: false,
            matches: vec![SymbolMatch {
                file: "src/main.rs".to_string(),
                line: 42,
                end_line: Some(60),
                kind: "function".to_string(),
                preview: "pub fn process_data(input".to_string(),
                excerpt_command: "frg excerpt src/main.rs:process_data".to_string(),
            }],
        };
        let out = format_lookup(&result);
        assert!(out.contains("1 matches"));
        assert!(out.contains("L42-60"));
        assert!(out.contains("process_data"));
    }

    #[test]
    fn format_lookup_with_lsp_hint() {
        let result = LookupResult {
            method: "lsp_hint".to_string(),
            lsp_available: true,
            matches: vec![],
        };
        let out = format_lookup(&result);
        assert!(out.contains("LSP available"));
        assert!(out.contains("lsp_workspace_symbols"));
    }
}
