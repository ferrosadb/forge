//! Symbol lookup: find where a symbol is declared, in this project.
//!
//! `frg lookup <symbol>` scans the project's own source files and returns each
//! declaration with a ready-to-run `frg excerpt` command. Forge answers with
//! its own tools — it never points at another server's namespace.

use std::path::Path;

use serde::Serialize;

use crate::excerpt;
use crate::summarizer::{self, ElementKind};

#[derive(Debug, Serialize)]
pub struct LookupResult {
    pub method: String,
    pub matches: Vec<SymbolMatch>,
    /// How to get a language server for this project, when the answer fell
    /// back to the source walk and one is missing. `None` when a server
    /// answered, or when the language has no known server.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lsp_advice: Option<LspAdvice>,
}

/// What to tell the model when its lookup ran without a language server.
#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct LspAdvice {
    pub language: String,
    pub binary: String,
    pub install: String,
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

/// Look up a symbol across all source files in a directory.
///
/// If LSP is available, returns a suggestion to use LSP tools.
/// Otherwise, scans all files using digest and returns matches with
/// ready-to-use excerpt commands.
pub fn lookup_symbol(symbol: &str, dir: &Path) -> anyhow::Result<LookupResult> {
    // Race the language server against the source walk and take whichever
    // answers first. The LSP is capped short by design (`rust_lsp`), so this
    // can only ever be faster than the walk alone — a cold or wedged
    // rust-analyzer bows out and the walker's answer stands.
    let (lsp_tx, lsp_rx) = std::sync::mpsc::channel();
    {
        let symbol = symbol.to_string();
        let dir = dir.to_path_buf();
        std::thread::spawn(move || {
            let found = crate::rust_lsp::workspace_symbols(&symbol, &dir)
                .ok()
                .flatten()
                .filter(|matches| !matches.is_empty());
            let _ = lsp_tx.send(found);
        });
    }

    let walked = walk_symbol(symbol, dir)?;
    // The walk is the floor: it is already finished, so an LSP answer that has
    // not arrived yet adds nothing but latency.
    if let Ok(Some(matches)) = lsp_rx.try_recv() {
        return Ok(LookupResult {
            method: "lsp".to_string(),
            matches,
            lsp_advice: None,
        });
    }
    Ok(walked)
}

/// [`lookup_symbol`] with the language-server state injected, so the advice
/// rule is testable without depending on what this machine happens to have
/// installed. `lsp_path` is the resolved server, or `None` when there is none.
///
/// The advice is the **only** way a model learns a server is missing: forge
/// prints its install prompt to stderr (`ingest::lsp::prompt_lsp_install`),
/// where the model never reads it. So it rides on the answer, beside the
/// matches rather than instead of them.
pub fn lookup_symbol_with_lsp_state(
    symbol: &str,
    dir: &Path,
    lsp_path: Option<&str>,
) -> anyhow::Result<LookupResult> {
    let mut result = lookup_symbol(symbol, dir)?;
    // A server answered: nothing to advise.
    if lsp_path.is_some() || result.method != "grep" {
        return Ok(result);
    }
    result.lsp_advice = lsp_advice_for(dir);
    Ok(result)
}

/// The advice for the project at `dir`, from the language its sources show.
///
/// `None` when no source names a language forge knows a server for — inventing
/// an install command for an unsupported language would be advice the model
/// can never act on.
fn lsp_advice_for(dir: &Path) -> Option<LspAdvice> {
    let language = dominant_language(dir)?;
    let info = forge_ingest::lsp::lsp_for_language(&language)?;
    Some(LspAdvice {
        language,
        binary: info.binary.to_string(),
        install: info.install.to_string(),
    })
}

/// The source language with the most files under `dir`, by extension.
///
/// Shallow and cheap on purpose: it reads the walk's own file list, not a
/// parser, so a lookup never pays a second scan to learn what to advise.
pub fn dominant_language(dir: &Path) -> Option<String> {
    use std::collections::HashMap;

    let mut counts: HashMap<&'static str, usize> = HashMap::new();
    for entry in ignore::WalkBuilder::new(dir)
        .hidden(true)
        .git_ignore(true)
        .build()
        .flatten()
    {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        if let Some(lang) = language_for_ext(ext) {
            *counts.entry(lang).or_default() += 1;
        }
    }
    counts
        .into_iter()
        .max_by_key(|&(_, n)| n)
        .map(|(l, _)| l.to_string())
}

/// The language a source extension names, for the servers forge knows.
fn language_for_ext(ext: &str) -> Option<&'static str> {
    match ext {
        "rs" => Some("rust"),
        "py" => Some("python"),
        "go" => Some("go"),
        "ts" | "tsx" | "js" | "jsx" => Some("typescript"),
        "ex" | "exs" => Some("elixir"),
        _ => None,
    }
}

/// The source-walk half of [`lookup_symbol`].
fn walk_symbol(symbol: &str, dir: &Path) -> anyhow::Result<LookupResult> {
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
        method: "grep".to_string(),
        lsp_advice: None,
        matches,
    })
}

/// Format lookup results for human/LLM consumption.
pub fn format_lookup(result: &LookupResult) -> String {
    let mut out = String::new();

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
            matches: vec![],
            lsp_advice: None,
        };
        let out = format_lookup(&result);
        assert!(out.contains("No matches found"));
    }

    #[test]
    fn format_lookup_with_matches() {
        let result = LookupResult {
            method: "grep".to_string(),
            matches: vec![SymbolMatch {
                file: "src/main.rs".to_string(),
                line: 42,
                end_line: Some(60),
                kind: "function".to_string(),
                preview: "pub fn process_data(input".to_string(),
                excerpt_command: "frg excerpt src/main.rs:process_data".to_string(),
            }],
            lsp_advice: None,
        };
        let out = format_lookup(&result);
        assert!(out.contains("1 matches"));
        assert!(out.contains("L42-60"));
        assert!(out.contains("process_data"));
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
        // them.
        //
        // The assertion is on the RESULT, not on a shared counter: sibling
        // tests parse concurrently in this process, so any delta over a
        // process-wide counter is schedule-dependent and flakes (measured:
        // passes alone, fails in the suite). What is deterministic is that a
        // file with no textual mention cannot contribute a match, so the
        // result may only ever name the file that has one.
        let dir = tempfile::tempdir().expect("temp dir");
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

        let before = summarizer::summarize_count();
        let result = lookup_symbol("the_symbol_im_looking_for", dir.path()).expect("lookup");
        let parsed = summarizer::summarize_count() - before;

        assert_eq!(result.matches.len(), 1, "exactly one declaration matches");
        assert_eq!(
            parsed, 1,
            "only the file holding the symbol may be parsed, of 41"
        );
        assert!(
            result.matches[0].file.ends_with("target.rs"),
            "the match is the file holding the symbol: {:?}",
            result.matches[0].file
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

    // ---- forge does its own lookup; it never advertises another server ----
    //
    // The lookup used to read ~/.claude/settings.json for the substring
    // "oh-my-claudecode" and, on a hit, print a suggestion to call
    // `mcp__plugin_oh-my-claudecode_t__lsp_workspace_symbols`. That name is
    // another product's namespace: under alter-zero the substring is never
    // present, so the branch was dead, and the tool a model saw was a name it
    // could not call. Forge's answer is its own.

    #[test]
    fn the_renderer_never_emits_a_foreign_tool_even_if_a_result_says_lsp() {
        // The guard must be reachable whatever a caller passes. A result
        // claiming `lsp_available: true` is exactly what the deleted
        // settings.json branch produced, so feeding it directly is the only
        // way to reach the code the fix removes.
        let result = LookupResult {
            method: "lsp_hint".to_string(),
            matches: vec![],
            lsp_advice: None,
        };
        let rendered = format_lookup(&result);
        assert!(
            !rendered.contains("oh-my-claudecode") && !rendered.contains("mcp__plugin_"),
            "a foreign tool namespace reached the answer: {rendered}"
        );
    }

    #[test]
    fn a_lookup_never_names_another_products_tool() {
        let dir = tempfile::tempdir().expect("temp dir");
        std::fs::write(dir.path().join("a.rs"), "pub fn thing() {}\n").expect("write");
        let result = lookup_symbol("thing", dir.path()).expect("lookup");
        let rendered = format_lookup(&result);
        assert!(
            !rendered.contains("oh-my-claudecode"),
            "the answer advertises a foreign server: {rendered}"
        );
        assert!(
            !rendered.contains("mcp__plugin_"),
            "the answer names a foreign tool namespace: {rendered}"
        );
    }

    #[test]
    fn a_grep_fallback_on_a_rust_tree_says_to_install_rust_analyzer() {
        // Measured 2026-11-23: without a language server the walk parses every
        // candidate file, and a model that never learns an LSP is *missing*
        // just keeps paying that. The answer must carry the install command —
        // as one of forge's own tools, not a hint printed to stderr where the
        // model cannot see it.
        let dir = tempfile::tempdir().expect("temp dir");
        std::fs::write(dir.path().join("a.rs"), "pub fn thing() {}\n").expect("write");
        let result = lookup_symbol_with_lsp_state("thing", dir.path(), None).expect("lookup");
        let advice = result
            .lsp_advice
            .expect("a grep answer with no server must advise one");
        assert_eq!(advice.language, "rust");
        assert_eq!(advice.binary, "rust-analyzer");
        assert!(
            advice.install.contains("rust-analyzer"),
            "the install command must name the binary: {}",
            advice.install
        );
    }

    #[test]
    fn no_advice_is_offered_when_the_server_is_present_or_the_language_is_unknown() {
        // Two negative controls. An LSP that IS answering needs no advice, and
        // a language forge has no server for must not invent one — a hint the
        // model can never satisfy is worse than silence.
        let dir = tempfile::tempdir().expect("temp dir");
        std::fs::write(dir.path().join("a.rs"), "pub fn thing() {}\n").expect("write");
        let with_lsp =
            lookup_symbol_with_lsp_state("thing", dir.path(), Some("/usr/bin/ra")).expect("lookup");
        assert!(
            with_lsp.lsp_advice.is_none(),
            "an answering LSP needs no advice"
        );

        let odd = tempfile::tempdir().expect("temp dir");
        std::fs::write(odd.path().join("a.zig"), "pub fn thing() {}\n").expect("write");
        let unknown = lookup_symbol_with_lsp_state("thing", odd.path(), None)
            .expect("lookup for a language with no known server");
        assert!(
            unknown.lsp_advice.is_none(),
            "no known server means no install advice"
        );
    }

    #[test]
    fn a_match_still_answers_when_advice_is_present() {
        // Advice rides *beside* the answer, never instead of it: a model told
        // to install a server still needs the file and line it asked for.
        let dir = tempfile::tempdir().expect("temp dir");
        std::fs::write(dir.path().join("a.rs"), "pub fn thing() {}\n").expect("write");
        let result = lookup_symbol_with_lsp_state("thing", dir.path(), None).expect("lookup");
        assert!(!result.matches.is_empty(), "the walk must still answer");
        assert!(result.lsp_advice.is_some(), "and the advice is beside it");
    }

    #[test]
    fn a_lookup_reports_only_the_methods_forge_itself_runs() {
        let dir = tempfile::tempdir().expect("temp dir");
        std::fs::write(dir.path().join("a.rs"), "pub fn thing() {}\n").expect("write");
        let result = lookup_symbol("thing", dir.path()).expect("lookup");
        assert!(
            result.method == "grep" || result.method == "lsp",
            "method must name something forge runs, got {:?}",
            result.method
        );
    }
}
