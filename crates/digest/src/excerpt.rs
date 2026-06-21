//! Symbol excerpt extraction.
//!
//! Given a filename and symbol name, extracts the full body of a single
//! function, struct, enum, etc. from source code. Uses the summarizer
//! to locate symbols, then extracts the complete body using brace-depth
//! or indentation tracking.

use crate::summarizer::{self, CodeElement, ElementKind};

/// Result of extracting a symbol from source code.
#[derive(Debug, PartialEq)]
pub struct ExcerptResult {
    pub file: String,
    pub symbol: String,
    pub start_line: usize,
    pub end_line: usize,
    pub body: String,
}

/// Extract a named symbol's full body from source code.
///
/// Uses the summarizer to find structural elements, then locates the one
/// whose `text` contains `symbol`. Extracts the complete body including
/// doc comments above the definition.
///
/// Returns `None` if the symbol is not found.
pub fn extract_symbol(filename: &str, source: &str, symbol: &str) -> Option<ExcerptResult> {
    let digest = summarizer::summarize(filename, source);
    let lines: Vec<&str> = source.lines().collect();

    // Find the element whose text contains the symbol name.
    // Prefer exact word-boundary matches (e.g. "process" matches "fn process("
    // but not "fn process_data(").
    let element = find_best_match(&digest.elements, symbol)?;

    let is_python = filename.ends_with(".py");
    let is_elixir = filename.ends_with(".ex") || filename.ends_with(".exs");

    // Determine start line (1-indexed from element, convert to 0-indexed)
    let elem_start_0 = element.line.saturating_sub(1);

    // Include doc comment lines above the definition
    let doc_start_0 = find_doc_start(&lines, elem_start_0, is_python);

    // Determine end line (0-indexed, exclusive).
    // For indentation-based languages (Python, Elixir), always use indentation
    // tracking because the summarizer's brace-based find_block_end falls through
    // to end-of-file for braceless code.
    let end_0 = if is_python || is_elixir {
        find_indentation_block_end(&lines, elem_start_0)
    } else if let Some(end_line) = element.end_line {
        end_line
    } else {
        find_brace_block_end(&lines, elem_start_0)
    };

    // Clamp to file bounds
    let start = doc_start_0;
    let end = end_0.min(lines.len());

    if start >= lines.len() {
        return None;
    }

    let body = lines[start..end].join("\n");

    Some(ExcerptResult {
        file: filename.to_string(),
        symbol: symbol.to_string(),
        start_line: start + 1, // 1-indexed
        end_line: end,         // 1-indexed (inclusive of last line)
        body,
    })
}

/// Find the best matching element for a symbol name.
///
/// Prioritizes exact word matches over substring matches.
/// For example, searching for "process" should match "fn process(" before
/// "fn process_data(".
fn find_best_match<'a>(elements: &'a [CodeElement], symbol: &str) -> Option<&'a CodeElement> {
    // Skip imports — they're not extractable bodies
    let candidates: Vec<&CodeElement> = elements
        .iter()
        .filter(|e| e.kind != ElementKind::Import && e.kind != ElementKind::TestBlock)
        .collect();

    // First pass: exact word boundary match using a simple heuristic.
    // The symbol should appear as a complete word in the text.
    for elem in &candidates {
        if has_word_match(&elem.text, symbol) {
            return Some(elem);
        }
    }

    // Second pass: substring match
    candidates
        .iter()
        .copied()
        .find(|elem| elem.text.contains(symbol))
}

/// Check if `text` contains `word` as a complete identifier
/// (bounded by non-identifier chars: not alphanumeric and not `_`).
fn has_word_match(text: &str, word: &str) -> bool {
    let is_ident_char = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    let bytes = text.as_bytes();
    let word_len = word.len();

    // Check all occurrences, not just the first
    let mut search_from = 0;
    while let Some(rel_pos) = text[search_from..].find(word) {
        let pos = search_from + rel_pos;
        let before_ok = pos == 0 || !is_ident_char(bytes[pos - 1]);
        let after_pos = pos + word_len;
        let after_ok = after_pos >= bytes.len() || !is_ident_char(bytes[after_pos]);
        if before_ok && after_ok {
            return true;
        }
        search_from = pos + 1;
    }
    false
}

/// Walk backwards from the definition line to find where doc comments start.
fn find_doc_start(lines: &[&str], def_line: usize, is_python: bool) -> usize {
    if def_line == 0 {
        return 0;
    }

    let mut start = def_line;
    for i in (0..def_line).rev() {
        let trimmed = lines[i].trim();
        if is_python {
            // Python: look for # comments or decorators above
            if trimmed.starts_with('#') || trimmed.starts_with('@') {
                start = i;
            } else {
                break;
            }
        } else {
            // Brace-based: look for /// doc comments, //! comments, /** comments,
            // or #[...] attributes (Rust)
            if trimmed.starts_with("///")
                || trimmed.starts_with("//!")
                || trimmed.starts_with("/**")
                || trimmed.starts_with("* ")
                || trimmed.starts_with("*/")
                || trimmed.starts_with("#[")
            {
                start = i;
            } else {
                break;
            }
        }
    }
    start
}

/// Find the end of a brace-delimited block starting at `start` (0-indexed).
/// Returns the line index (0-indexed, exclusive) after the closing brace.
fn find_brace_block_end(lines: &[&str], start: usize) -> usize {
    let mut depth = 0i32;
    let mut started = false;
    for (i, line) in lines.iter().enumerate().skip(start) {
        for ch in line.chars() {
            if ch == '{' {
                depth += 1;
                started = true;
            } else if ch == '}' {
                depth -= 1;
                if started && depth == 0 {
                    return i + 1;
                }
            }
        }
    }
    lines.len()
}

/// Find the end of an indentation-based block (Python, Elixir).
/// The block ends when a non-empty line has equal or less indentation than
/// the definition line.
fn find_indentation_block_end(lines: &[&str], start: usize) -> usize {
    if start >= lines.len() {
        return lines.len();
    }

    let base_indent = indent_level(lines[start]);

    for (i, line) in lines.iter().enumerate().skip(start + 1) {
        // Skip empty lines
        if line.trim().is_empty() {
            continue;
        }
        let indent = indent_level(line);
        if indent <= base_indent {
            return i;
        }
    }
    lines.len()
}

/// Count leading whitespace characters.
fn indent_level(line: &str) -> usize {
    line.len() - line.trim_start().len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rust_fn_extraction() {
        let source = r#"use std::io;

/// Process incoming data.
pub fn process(data: &[u8]) -> Result<()> {
    let x = data.len();
    if x > 0 {
        println!("got data");
    }
    Ok(())
}

pub fn other() {}
"#;
        let result = extract_symbol("test.rs", source, "process").unwrap();
        assert_eq!(result.symbol, "process");
        assert_eq!(result.file, "test.rs");
        // Should include the doc comment
        assert!(result.body.contains("/// Process incoming data."));
        // Should include the full function body
        assert!(result.body.contains("pub fn process"));
        assert!(result.body.contains("Ok(())"));
        assert!(result.body.contains("}"));
        // Should NOT include the other function
        assert!(!result.body.contains("pub fn other"));
    }

    #[test]
    fn rust_struct_extraction() {
        let source = r#"use serde::Serialize;

/// Application state.
#[derive(Debug)]
pub struct AppState {
    pub db: Pool,
    pub cache: Redis,
}

pub fn run() {}
"#;
        let result = extract_symbol("test.rs", source, "AppState").unwrap();
        assert_eq!(result.symbol, "AppState");
        assert!(result.body.contains("/// Application state."));
        assert!(result.body.contains("#[derive(Debug)]"));
        assert!(result.body.contains("pub struct AppState"));
        assert!(result.body.contains("pub cache: Redis,"));
        assert!(!result.body.contains("pub fn run"));
    }

    #[test]
    fn python_def_extraction() {
        let source = r#"import os

def process_data(data):
    result = []
    for item in data:
        result.append(item * 2)
    return result

def other():
    pass
"#;
        let result = extract_symbol("test.py", source, "process_data").unwrap();
        assert_eq!(result.symbol, "process_data");
        assert!(result.body.contains("def process_data(data):"));
        assert!(result.body.contains("return result"));
        // Should NOT include the other function
        assert!(!result.body.contains("def other"));
    }

    #[test]
    fn symbol_not_found_returns_none() {
        let source = "pub fn existing() {}\n";
        let result = extract_symbol("test.rs", source, "nonexistent");
        assert!(result.is_none());
    }

    #[test]
    fn exact_word_match_preferred() {
        let source = r#"pub fn process_data() {
    // long body
}

pub fn process() {
    // short body
}
"#;
        let result = extract_symbol("test.rs", source, "process").unwrap();
        assert!(result.body.contains("pub fn process()"));
        assert!(!result.body.contains("pub fn process_data"));
    }

    #[test]
    fn rust_enum_extraction() {
        let source = r#"pub enum Color {
    Red,
    Green,
    Blue,
}
"#;
        let result = extract_symbol("test.rs", source, "Color").unwrap();
        assert!(result.body.contains("pub enum Color"));
        assert!(result.body.contains("Blue,"));
    }

    #[test]
    fn python_with_decorator() {
        let source = r#"import flask

@app.route("/api")
def handle_request():
    return "ok"

def other():
    pass
"#;
        let result = extract_symbol("test.py", source, "handle_request").unwrap();
        assert!(result.body.contains("@app.route"));
        assert!(result.body.contains("def handle_request"));
        assert!(result.body.contains("return \"ok\""));
        assert!(!result.body.contains("def other"));
    }
}
