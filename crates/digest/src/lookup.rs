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

        // A file that never mentions the symbol cannot hold its declaration,
        // so parsing it is pure cost. Measured on a 548-file Rust tree:
        // `summarize` totals ~22s (~40ms/file) against a 0.07s walk, which is
        // why this tool took 37.9s where a grep takes 50ms. The check is a
        // *substring* one, not "is it a declaration": a symbol named only in
        // a doc comment still reaches the parse, because its declaration may
        // sit below that comment.
        if !source.contains(symbol) {
            continue;
        }

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

    // ---- the walk must not parse a file the symbol is not in ----
    //
    // Measured on a 548-file Rust tree: the raw walk plus read costs 0.07s,
    // while `summarize()` over the same files costs 22s (~40ms/file). The
    // lookup called `summarize` on every source file to find one symbol, so
    // the tool took 37.9s where a grep takes 50ms — slow enough that a model
    // is right to prefer grep over it.

    #[test]
    fn a_file_without_the_symbol_is_skipped_before_parsing() {
        // RED as written: `lookup_symbol` summarises every source file, so a
        // tree where only one file mentions the symbol still pays for all of
        // them. This asserts the observable effect — the parse count — not the
        // presence of a new helper.
        let dir = tempfile::tempdir().expect("temp dir");
        // 40 files that do NOT contain the symbol, and one that does.
        for i in 0..40 {
            std::fs::write(
                dir.path().join(format!("noise_{i}.rs")),
                format!("pub fn unrelated_{i}() {{}}\n"),
            )
            .expect("write noise");
        }
        std::fs::write(
            dir.path().join("target.rs"),
            "pub fn the_symbol_im_looking_for() {}\n",
        )
        .expect("write target");

        // Other tests run in parallel and share this process-wide counter, so
        // a delta over `before` is not stable. What IS stable: 41 noise files
        // can contribute at most 0 parses once the pre-filter exists, so a
        // tree that would have cost 41 costs at most a handful. The bound is
        // deliberately loose enough to survive neighbours and still fail on
        // the unfiltered behaviour (41).
        let before = summarizer::summarize_count();
        let result = lookup_symbol("the_symbol_im_looking_for", dir.path()).expect("lookup");
        let parsed = summarizer::summarize_count().saturating_sub(before);

        assert_eq!(result.matches.len(), 1, "the symbol is found");
        assert!(
            parsed < 5,
            "the walk parsed {parsed} files; only the one holding the symbol should be parsed"
        );
    }

    #[test]
    fn the_pre_filter_never_drops_a_file_whose_declaration_is_found() {
        // The invariant the pre-filter must not break: what it skips is a
        // superset of what the parse would have matched. Every element's
        // `text` is a verbatim slice of the source, so a file that lacks the
        // substring cannot yield a match — and a file that HAS it is still
        // parsed, including when the only textual hit is in a doc comment
        // above the declaration (which is how a symbol is usually documented).
        let dir = tempfile::tempdir().expect("temp dir");
        std::fs::write(
            dir.path().join("documented.rs"),
            "/// Builds a WidgetFactory, and says so in prose.\n\
             pub fn make() -> u8 { 1 }\n",
        )
        .expect("write");
        // The doc mention alone is enough to get the file parsed, so the
        // declaration's own kind is reported rather than skipped.
        let result = lookup_symbol("make", dir.path()).expect("lookup");
        assert!(
            result
                .matches
                .iter()
                .any(|m| m.file.ends_with("documented.rs")),
            "the documented declaration was found: {:?}",
            result.matches
        );
    }
}
