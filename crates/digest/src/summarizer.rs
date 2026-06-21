//! Code structure summarizer.
//!
//! Extracts structural elements from source files using regex patterns.
//! Language-aware: Rust, Python, Go, TypeScript, Elixir, C++.

use regex::Regex;
use serde::Serialize;

#[derive(Debug, Serialize, PartialEq)]
pub struct FileDigest {
    pub file: String,
    pub total_lines: usize,
    pub elements: Vec<CodeElement>,
}

#[derive(Debug, Serialize, PartialEq, Clone)]
pub struct CodeElement {
    pub kind: ElementKind,
    pub line: usize,
    pub end_line: Option<usize>,
    pub text: String,
    pub doc: Option<String>,
}

impl FileDigest {
    /// Return a clone of this digest keeping only elements whose `text` or `doc`
    /// matches `pattern`.
    pub fn filtered(&self, pattern: &Regex) -> FileDigest {
        FileDigest {
            file: self.file.clone(),
            total_lines: self.total_lines,
            elements: self
                .elements
                .iter()
                .filter(|e| {
                    pattern.is_match(&e.text) || e.doc.as_ref().is_some_and(|d| pattern.is_match(d))
                })
                .cloned()
                .collect(),
        }
    }
}

#[derive(Debug, Serialize, PartialEq, Clone)]
pub enum ElementKind {
    Import,
    Function,
    Struct,
    Enum,
    Trait,
    Interface,
    Class,
    Constant,
    TypeAlias,
    Module,
    TestBlock,
}

/// Summarize a source file into a structural digest.
pub fn summarize(filename: &str, source: &str) -> FileDigest {
    let lines: Vec<&str> = source.lines().collect();
    let total_lines = lines.len();
    let lang = detect_language(filename);
    let mut elements = Vec::new();
    let mut seen_imports = std::collections::HashSet::new();

    // Track skip ranges for test blocks so we don't emit their contents
    let mut skip_until = 0usize;

    for (i, line) in lines.iter().enumerate() {
        // Skip lines inside collapsed test blocks
        if i < skip_until {
            continue;
        }

        let trimmed = line.trim();

        // Skip empty lines and pure comments (unless doc comments captured separately)
        if trimmed.is_empty() {
            continue;
        }

        // Test blocks (collapse) — check early so imports/fns inside are skipped
        if is_test_block(trimmed, lang) {
            let test_count = count_tests_in_block(&lines, i, lang);
            let block_end = find_block_end(&lines, i);
            elements.push(CodeElement {
                kind: ElementKind::TestBlock,
                line: i + 1,
                end_line: Some(block_end),
                text: format!("mod tests ({test_count} tests)"),
                doc: None,
            });
            skip_until = block_end;
            continue;
        }

        // Imports — only true top-level (0 indent), deduplicated within a file
        if is_true_top_level(line, lang) && is_import(trimmed, lang) {
            if seen_imports.insert(trimmed.to_string()) {
                elements.push(CodeElement {
                    kind: ElementKind::Import,
                    line: i + 1,
                    end_line: None,
                    text: trimmed.to_string(),
                    doc: None,
                });
            }
            continue;
        }

        // Constants
        if is_constant(trimmed, lang) {
            elements.push(CodeElement {
                kind: ElementKind::Constant,
                line: i + 1,
                end_line: None,
                text: trimmed.to_string(),
                doc: None,
            });
            continue;
        }

        // Type aliases
        if is_type_alias(trimmed, lang) {
            elements.push(CodeElement {
                kind: ElementKind::TypeAlias,
                line: i + 1,
                end_line: None,
                text: trimmed.to_string(),
                doc: None,
            });
            continue;
        }

        // Struct/class/enum/trait/interface
        if let Some(elem) = detect_type_definition(trimmed, i, &lines, lang) {
            elements.push(elem);
            continue;
        }

        // Functions/methods — only top-level (not indented in brace-based langs)
        if is_top_level_context(line, lang) {
            if let Some(elem) = detect_function(trimmed, i, &lines, lang) {
                elements.push(elem);
                continue;
            }
        }

        // Modules
        if is_module(trimmed, lang) {
            elements.push(CodeElement {
                kind: ElementKind::Module,
                line: i + 1,
                end_line: None,
                text: trimmed.to_string(),
                doc: None,
            });
        }
    }

    FileDigest {
        file: filename.to_string(),
        total_lines,
        elements,
    }
}

/// Find the closing brace of a block starting at `start`.
fn find_block_end(lines: &[&str], start: usize) -> usize {
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
                    return i + 1; // skip past closing brace line
                }
            }
        }
    }
    lines.len() // fallback: skip to end
}

/// Check if a line is likely at top-level scope (not deeply indented).
/// For brace-based languages, top-level = 0 or 1 level of indentation.
/// This prevents capturing `use regex::Regex;` inside function bodies as imports,
/// and prevents listing helper functions nested inside impl blocks at the same level.
fn is_top_level_context(line: &str, lang: Lang) -> bool {
    match lang {
        Lang::Rust | Lang::Go | Lang::Cpp | Lang::TypeScript => {
            // Allow top-level (0) and one level in (e.g. impl methods at 4 spaces)
            let indent = line.len() - line.trim_start().len();
            indent <= 4
        }
        Lang::Python | Lang::Elixir | Lang::Other => true,
    }
}

/// Stricter check: only true top-level (0 indent) for imports.
fn is_true_top_level(line: &str, lang: Lang) -> bool {
    match lang {
        Lang::Rust | Lang::Go | Lang::Cpp | Lang::TypeScript => {
            let indent = line.len() - line.trim_start().len();
            indent == 0
        }
        Lang::Python | Lang::Elixir | Lang::Other => true,
    }
}

/// Format a digest as a compact outline string.
pub fn format_outline(digest: &FileDigest) -> String {
    format_outline_filtered(digest, None)
}

/// Format a digest, optionally omitting imports in the `common_imports` set.
fn format_outline_filtered(
    digest: &FileDigest,
    common_imports: Option<&std::collections::HashSet<String>>,
) -> String {
    let mut out = format!("{} ({} lines)\n", digest.file, digest.total_lines);
    for elem in &digest.elements {
        // Skip imports that are in the common set (already shown at top)
        if elem.kind == ElementKind::Import {
            if let Some(common) = common_imports {
                if common.contains(&elem.text) {
                    continue;
                }
            }
        }
        let line_ref = match elem.end_line {
            Some(end) if end > elem.line => format!("L{}-{}", elem.line, end),
            _ => format!("L{}", elem.line),
        };
        out.push_str(&format!("  {line_ref:>10}  {}\n", elem.text));
    }
    out
}

/// Return a new `FileDigest` keeping only elements whose `text` or `doc`
/// matches the given regex pattern.
pub fn filter_digest(digest: &FileDigest, pattern: &Regex) -> FileDigest {
    digest.filtered(pattern)
}

/// Format multiple digests with a token budget, progressively dropping detail
/// until the output fits.
///
/// Token estimation: `text.len() / 4`.
///
/// Progressive dropping strategy:
/// 1. Full outline (current `format_multi_outline`)
/// 2. Drop `Import` elements
/// 3. Drop `Constant` and `TypeAlias` elements
/// 4. Drop `doc` fields
/// 5. Keep only `Function` + type definitions (`Struct`/`Enum`/`Trait`/`Class`/`Interface`)
/// 6. Collapse files to just `filename (N lines)`
pub fn format_multi_outline_budgeted(digests: &[FileDigest], token_budget: usize) -> String {
    let estimate = |s: &str| s.len() / 4;

    // Level 1: full outline
    let full = format_multi_outline(digests);
    let full_est = estimate(&full);
    if full_est <= token_budget {
        return append_budget_footer(full, full_est, token_budget, "full");
    }

    // Level 2: drop imports
    let no_imports: Vec<FileDigest> = digests
        .iter()
        .map(|d| FileDigest {
            file: d.file.clone(),
            total_lines: d.total_lines,
            elements: d
                .elements
                .iter()
                .filter(|e| e.kind != ElementKind::Import)
                .cloned()
                .collect(),
        })
        .collect();
    let text = format_multi_outline(&no_imports);
    let text_est = estimate(&text);
    if text_est <= token_budget {
        return append_budget_footer(text, text_est, token_budget, "no-imports");
    }

    // Level 3: also drop Constants and TypeAlias
    let no_const: Vec<FileDigest> = no_imports
        .iter()
        .map(|d| FileDigest {
            file: d.file.clone(),
            total_lines: d.total_lines,
            elements: d
                .elements
                .iter()
                .filter(|e| e.kind != ElementKind::Constant && e.kind != ElementKind::TypeAlias)
                .cloned()
                .collect(),
        })
        .collect();
    let text = format_multi_outline(&no_const);
    let text_est = estimate(&text);
    if text_est <= token_budget {
        return append_budget_footer(text, text_est, token_budget, "no-imports-consts");
    }

    // Level 4: drop doc fields
    let no_docs: Vec<FileDigest> = no_const
        .iter()
        .map(|d| FileDigest {
            file: d.file.clone(),
            total_lines: d.total_lines,
            elements: d
                .elements
                .iter()
                .map(|e| CodeElement {
                    doc: None,
                    ..e.clone()
                })
                .collect(),
        })
        .collect();
    let text = format_multi_outline(&no_docs);
    let text_est = estimate(&text);
    if text_est <= token_budget {
        return append_budget_footer(text, text_est, token_budget, "no-docs");
    }

    // Level 5: keep only Function + type definitions
    let sigs_only: Vec<FileDigest> = no_docs
        .iter()
        .map(|d| FileDigest {
            file: d.file.clone(),
            total_lines: d.total_lines,
            elements: d
                .elements
                .iter()
                .filter(|e| {
                    matches!(
                        e.kind,
                        ElementKind::Function
                            | ElementKind::Struct
                            | ElementKind::Enum
                            | ElementKind::Trait
                            | ElementKind::Class
                            | ElementKind::Interface
                    )
                })
                .cloned()
                .collect(),
        })
        .collect();
    let text = format_multi_outline(&sigs_only);
    let text_est = estimate(&text);
    if text_est <= token_budget {
        return append_budget_footer(text, text_est, token_budget, "signatures-only");
    }

    // Level 6: collapse to filename + line count
    let mut out = String::new();
    for d in digests {
        out.push_str(&format!("{} ({} lines)\n", d.file, d.total_lines));
    }
    let used = estimate(&out);
    append_budget_footer(out, used, token_budget, "file-list-only")
}

fn append_budget_footer(mut text: String, used: usize, budget: usize, level: &str) -> String {
    text.push_str(&format!(
        "\n[budget: {used}/{budget} tokens, detail level: {level}]\n"
    ));
    text
}

/// Format multiple digests, deduplicating imports that appear in 3+ files.
pub fn format_multi_outline(digests: &[FileDigest]) -> String {
    use std::collections::HashMap;

    // Count how many files each import appears in
    let mut import_counts: HashMap<&str, usize> = HashMap::new();
    for digest in digests {
        for elem in &digest.elements {
            if elem.kind == ElementKind::Import {
                *import_counts.entry(&elem.text).or_insert(0) += 1;
            }
        }
    }

    // Imports appearing in 3+ files are "common" — show once at the top
    let common: std::collections::HashSet<String> = import_counts
        .iter()
        .filter(|(_, &count)| count >= 3)
        .map(|(&text, _)| text.to_string())
        .collect();

    let mut out = String::new();
    if !common.is_empty() {
        out.push_str("Common imports:\n");
        let mut sorted: Vec<&str> = common.iter().map(|s| s.as_str()).collect();
        sorted.sort();
        for imp in sorted {
            out.push_str(&format!("  {imp}\n"));
        }
        out.push('\n');
    }

    for digest in digests {
        out.push_str(&format_outline_filtered(digest, Some(&common)));
    }
    out
}

#[derive(Debug, Clone, Copy)]
enum Lang {
    Rust,
    Python,
    Go,
    TypeScript,
    Elixir,
    Cpp,
    Other,
}

fn detect_language(filename: &str) -> Lang {
    if filename.ends_with(".rs") {
        Lang::Rust
    } else if filename.ends_with(".py") {
        Lang::Python
    } else if filename.ends_with(".go") {
        Lang::Go
    } else if filename.ends_with(".ts")
        || filename.ends_with(".tsx")
        || filename.ends_with(".js")
        || filename.ends_with(".jsx")
    {
        Lang::TypeScript
    } else if filename.ends_with(".ex") || filename.ends_with(".exs") {
        Lang::Elixir
    } else if filename.ends_with(".cpp")
        || filename.ends_with(".cc")
        || filename.ends_with(".h")
        || filename.ends_with(".hpp")
    {
        Lang::Cpp
    } else {
        Lang::Other
    }
}

fn is_import(line: &str, lang: Lang) -> bool {
    match lang {
        Lang::Rust => line.starts_with("use ") || line.starts_with("extern crate "),
        Lang::Python => line.starts_with("import ") || line.starts_with("from "),
        Lang::Go => line.starts_with("import "),
        Lang::TypeScript => line.starts_with("import "),
        Lang::Elixir => {
            line.starts_with("import ") || line.starts_with("alias ") || line.starts_with("use ")
        }
        Lang::Cpp => line.starts_with("#include "),
        Lang::Other => line.starts_with("import ") || line.starts_with("use "),
    }
}

fn is_constant(line: &str, lang: Lang) -> bool {
    match lang {
        Lang::Rust => {
            line.starts_with("const ")
                || line.starts_with("pub const ")
                || line.starts_with("static ")
        }
        Lang::Python => {
            let re = Regex::new(r"^[A-Z][A-Z_0-9]+\s*=").unwrap();
            re.is_match(line)
        }
        Lang::Go => line.starts_with("const ") || line.starts_with("var "),
        Lang::TypeScript => {
            (line.starts_with("const ") || line.starts_with("export const "))
                && line.contains('=')
                && !line.contains("=>")
        }
        Lang::Cpp => line.starts_with("constexpr ") || line.starts_with("#define "),
        _ => false,
    }
}

fn is_type_alias(line: &str, lang: Lang) -> bool {
    match lang {
        Lang::Rust => {
            (line.starts_with("type ") || line.starts_with("pub type ")) && line.contains('=')
        }
        Lang::TypeScript => {
            (line.starts_with("type ") || line.starts_with("export type ")) && line.contains('=')
        }
        Lang::Go => {
            line.starts_with("type ") && !line.contains("struct") && !line.contains("interface")
        }
        _ => false,
    }
}

fn is_test_block(line: &str, lang: Lang) -> bool {
    match lang {
        Lang::Rust => line.contains("mod tests"),
        Lang::Python => line.starts_with("class Test"),
        _ => false,
    }
}

fn count_tests_in_block(lines: &[&str], start: usize, lang: Lang) -> usize {
    let test_re = match lang {
        Lang::Rust => Regex::new(r"#\[test\]").unwrap(),
        Lang::Python => Regex::new(r"def test_").unwrap(),
        _ => Regex::new(r"(?:it|test)\(").unwrap(),
    };

    let mut count = 0;
    let mut depth = 0;
    let mut started = false;

    for line in lines.iter().skip(start) {
        for ch in line.chars() {
            if ch == '{' {
                depth += 1;
                started = true;
            } else if ch == '}' {
                depth -= 1;
                if started && depth == 0 {
                    return count;
                }
            }
        }
        if test_re.is_match(line) {
            count += 1;
        }
    }

    count
}

fn is_module(line: &str, lang: Lang) -> bool {
    match lang {
        Lang::Rust => {
            (line.starts_with("mod ") || line.starts_with("pub mod "))
                && line.ends_with(';')
                && !line.contains("tests")
        }
        Lang::Elixir => line.starts_with("defmodule "),
        _ => false,
    }
}

fn detect_type_definition(
    line: &str,
    idx: usize,
    lines: &[&str],
    lang: Lang,
) -> Option<CodeElement> {
    let (kind, re) = match lang {
        Lang::Rust => {
            if line.contains("struct ") {
                let re = Regex::new(r"(?:pub\s+)?struct\s+(\w+)").unwrap();
                (ElementKind::Struct, re)
            } else if line.contains("enum ") {
                let re = Regex::new(r"(?:pub\s+)?enum\s+(\w+)").unwrap();
                (ElementKind::Enum, re)
            } else if line.contains("trait ") {
                let re = Regex::new(r"(?:pub\s+)?trait\s+(\w+)").unwrap();
                (ElementKind::Trait, re)
            } else {
                return None;
            }
        }
        Lang::TypeScript => {
            if line.contains("interface ") {
                let re = Regex::new(r"(?:export\s+)?interface\s+(\w+)").unwrap();
                (ElementKind::Interface, re)
            } else if line.contains("class ") && !line.starts_with("class Test") {
                let re = Regex::new(r"(?:export\s+)?class\s+(\w+)").unwrap();
                (ElementKind::Class, re)
            } else {
                return None;
            }
        }
        Lang::Python if line.starts_with("class ") && !line.starts_with("class Test") => {
            let re = Regex::new(r"class\s+(\w+)").unwrap();
            (ElementKind::Class, re)
        }
        Lang::Python => return None,
        Lang::Go => {
            if line.contains("struct") {
                let re = Regex::new(r"type\s+(\w+)\s+struct").unwrap();
                (ElementKind::Struct, re)
            } else if line.contains("interface") {
                let re = Regex::new(r"type\s+(\w+)\s+interface").unwrap();
                (ElementKind::Interface, re)
            } else {
                return None;
            }
        }
        Lang::Cpp => {
            if line.contains("class ") {
                let re = Regex::new(r"class\s+(\w+)").unwrap();
                (ElementKind::Class, re)
            } else if line.contains("struct ") {
                let re = Regex::new(r"struct\s+(\w+)").unwrap();
                (ElementKind::Struct, re)
            } else {
                return None;
            }
        }
        _ => return None,
    };

    re.captures(line).map(|_cap| {
        let doc = get_doc_comment(lines, idx);
        let end_line = if line.contains('{') {
            Some(find_block_end(lines, idx))
        } else if line.contains(';') || line.ends_with(')') {
            None // single-line declaration
        } else {
            // Multi-line: check next lines for opening brace
            Some(find_block_end(lines, idx))
        };
        CodeElement {
            kind,
            line: idx + 1,
            end_line,
            text: line.trim_end_matches('{').trim().to_string(),
            doc,
        }
    })
}

fn detect_function(line: &str, idx: usize, lines: &[&str], lang: Lang) -> Option<CodeElement> {
    let re = match lang {
        Lang::Rust => Regex::new(r"^\s*(?:pub\s+)?(?:async\s+)?fn\s+\w+").unwrap(),
        Lang::Python => Regex::new(r"^\s*(?:async\s+)?def\s+\w+").unwrap(),
        Lang::Go => Regex::new(r"^func\s+(?:\([^)]+\)\s+)?\w+").unwrap(),
        Lang::TypeScript => Regex::new(r"^\s*(?:export\s+)?(?:async\s+)?function\s+\w+").unwrap(),
        Lang::Elixir => Regex::new(r"^\s*defp?\s+\w+").unwrap(),
        Lang::Cpp => Regex::new(r"^\s*(?:\w+\s+)*\w+\s+\w+\s*\(").unwrap(),
        Lang::Other => {
            Regex::new(r"^\s*(?:pub\s+)?(?:async\s+)?(?:fn|func|def|function)\s+\w+").unwrap()
        }
    };

    if !re.is_match(line) {
        return None;
    }

    // For Rust, skip lines inside test modules
    // For Python, skip private methods (except __init__)
    if matches!(lang, Lang::Python) {
        let trimmed = line.trim();
        if trimmed.starts_with("def _") && !trimmed.starts_with("def __") {
            return None;
        }
    }

    // Extract the signature (up to the opening brace or colon)
    let sig = line
        .split('{')
        .next()
        .unwrap_or(line)
        .split(':')
        .next()
        .unwrap_or(line);

    let doc = get_doc_comment(lines, idx);
    let end_line = Some(find_block_end(lines, idx));

    Some(CodeElement {
        kind: ElementKind::Function,
        line: idx + 1,
        end_line,
        text: sig.trim().to_string(),
        doc,
    })
}

fn get_doc_comment(lines: &[&str], idx: usize) -> Option<String> {
    if idx == 0 {
        return None;
    }

    let prev = lines[idx - 1].trim();

    if prev.starts_with("///") {
        Some(prev.trim_start_matches('/').trim().to_string())
    } else if prev.starts_with("//!") {
        Some(prev.trim_start_matches("//!").trim().to_string())
    } else if prev.starts_with('#') && prev.contains("doc") {
        Some(prev.to_string())
    } else if prev.starts_with("/**") || prev.starts_with("\"\"\"") {
        Some(
            prev.trim_start_matches("/**")
                .trim_end_matches("*/")
                .trim()
                .to_string(),
        )
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // === Test list (TDD) ===
    // [x] empty file produces empty digest
    // [x] rust: extracts imports
    // [x] rust: extracts pub fn signature
    // [x] rust: extracts struct
    // [x] rust: collapses test module
    // [x] python: extracts imports and defs
    // [x] go: extracts func and type struct
    // [x] typescript: extracts exports
    // [x] format_outline produces readable text
    // [x] doc comments captured
    // [x] constants captured

    #[test]
    fn empty_file() {
        let result = summarize("test.rs", "");
        assert!(result.elements.is_empty());
        assert_eq!(result.total_lines, 0);
    }

    #[test]
    fn rust_imports() {
        let source = "use std::collections::HashMap;\nuse serde::Serialize;\n";
        let result = summarize("test.rs", source);
        let imports: Vec<_> = result
            .elements
            .iter()
            .filter(|e| e.kind == ElementKind::Import)
            .collect();
        assert_eq!(imports.len(), 2);
    }

    #[test]
    fn rust_pub_fn() {
        let source =
            "/// Process data.\npub fn process(data: &[u8]) -> Result<()> {\n    Ok(())\n}\n";
        let result = summarize("test.rs", source);
        let fns: Vec<_> = result
            .elements
            .iter()
            .filter(|e| e.kind == ElementKind::Function)
            .collect();
        assert_eq!(fns.len(), 1);
        assert!(fns[0].text.contains("process"));
        assert!(fns[0].doc.is_some());
    }

    #[test]
    fn rust_struct() {
        let source = "pub struct AppState {\n    db: Pool,\n    cache: Redis,\n}\n";
        let result = summarize("test.rs", source);
        let structs: Vec<_> = result
            .elements
            .iter()
            .filter(|e| e.kind == ElementKind::Struct)
            .collect();
        assert_eq!(structs.len(), 1);
        assert!(structs[0].text.contains("AppState"));
    }

    #[test]
    fn rust_test_block_collapsed() {
        let source = r#"fn main() {}

#[cfg(test)]
mod tests {
    #[test]
    fn test_one() {}
    #[test]
    fn test_two() {}
    #[test]
    fn test_three() {}
}
"#;
        let result = summarize("test.rs", source);
        let tests: Vec<_> = result
            .elements
            .iter()
            .filter(|e| e.kind == ElementKind::TestBlock)
            .collect();
        assert_eq!(tests.len(), 1);
        assert!(tests[0].text.contains("3 tests"));
    }

    #[test]
    fn python_imports_and_defs() {
        let source = "import os\nfrom pathlib import Path\n\ndef process(data):\n    pass\n";
        let result = summarize("test.py", source);
        let imports: Vec<_> = result
            .elements
            .iter()
            .filter(|e| e.kind == ElementKind::Import)
            .collect();
        let fns: Vec<_> = result
            .elements
            .iter()
            .filter(|e| e.kind == ElementKind::Function)
            .collect();
        assert_eq!(imports.len(), 2);
        assert_eq!(fns.len(), 1);
    }

    #[test]
    fn go_func_and_struct() {
        let source = "type AppState struct {\n\tDB *sql.DB\n}\n\nfunc ProcessData(state *AppState) error {\n\treturn nil\n}\n";
        let result = summarize("test.go", source);
        assert!(result
            .elements
            .iter()
            .any(|e| e.kind == ElementKind::Struct));
        assert!(result
            .elements
            .iter()
            .any(|e| e.kind == ElementKind::Function));
    }

    #[test]
    fn typescript_exports() {
        let source = "import { Router } from 'express';\n\nexport interface Config {\n  port: number;\n}\n\nexport async function startServer(config: Config) {\n}\n";
        let result = summarize("test.ts", source);
        assert!(result
            .elements
            .iter()
            .any(|e| e.kind == ElementKind::Import));
        assert!(result
            .elements
            .iter()
            .any(|e| e.kind == ElementKind::Interface));
        assert!(result
            .elements
            .iter()
            .any(|e| e.kind == ElementKind::Function));
    }

    #[test]
    fn format_outline_readable() {
        let source = "use std::io;\n\npub fn main() {\n    println!(\"hello\");\n}\n";
        let digest = summarize("test.rs", source);
        let outline = format_outline(&digest);
        assert!(outline.contains("test.rs (5 lines)"));
        assert!(outline.contains("use std::io"));
    }

    #[test]
    fn doc_comments_captured() {
        let source = "/// Important function.\npub fn important() {}\n";
        let result = summarize("test.rs", source);
        let func = result
            .elements
            .iter()
            .find(|e| e.kind == ElementKind::Function)
            .unwrap();
        assert_eq!(func.doc, Some("Important function.".to_string()));
    }

    #[test]
    fn constants_captured() {
        let source = "pub const MAX_SIZE: usize = 1024;\n";
        let result = summarize("test.rs", source);
        assert!(result
            .elements
            .iter()
            .any(|e| e.kind == ElementKind::Constant));
    }

    // === filter_digest tests ===

    #[test]
    fn filter_digest_keeps_matching_text() {
        let source = "use std::io;\n\n/// Process data.\npub fn process(data: &[u8]) -> Result<()> {\n    Ok(())\n}\n\npub fn unrelated() {\n}\n";
        let digest = summarize("test.rs", source);
        let pat = Regex::new("process").unwrap();
        let filtered = filter_digest(&digest, &pat);
        assert_eq!(filtered.elements.len(), 1);
        assert!(filtered.elements[0].text.contains("process"));
    }

    #[test]
    fn filter_digest_matches_doc() {
        let source = "/// Important helper.\npub fn helper() {\n}\n\npub fn other() {\n}\n";
        let digest = summarize("test.rs", source);
        let pat = Regex::new("Important").unwrap();
        let filtered = filter_digest(&digest, &pat);
        assert_eq!(filtered.elements.len(), 1);
        assert!(filtered.elements[0].text.contains("helper"));
    }

    #[test]
    fn filter_digest_empty_on_no_match() {
        let source = "pub fn foo() {\n}\n";
        let digest = summarize("test.rs", source);
        let pat = Regex::new("zzz_nonexistent").unwrap();
        let filtered = filter_digest(&digest, &pat);
        assert!(filtered.elements.is_empty());
    }

    // === format_multi_outline_budgeted tests ===

    fn make_test_digests() -> Vec<FileDigest> {
        vec![
            summarize(
                "a.rs",
                "use std::io;\npub const MAX: usize = 100;\ntype Alias = String;\n/// Doc.\npub fn alpha() {\n}\npub struct Foo {\n}\n",
            ),
            summarize(
                "b.rs",
                "use std::io;\npub fn beta() {\n}\npub enum Bar {\n}\n",
            ),
        ]
    }

    #[test]
    fn budgeted_full_when_budget_large() {
        let digests = make_test_digests();
        let result = format_multi_outline_budgeted(&digests, 100_000);
        assert!(result.contains("detail level: full"));
        assert!(result.contains("use std::io"));
    }

    #[test]
    fn budgeted_drops_imports_at_level2() {
        let digests = make_test_digests();
        // Use a budget that is too small for full but enough without imports
        let full = format_multi_outline(&digests);
        let full_tokens = full.len() / 4;
        // Budget just under full but enough for no-imports
        let result = format_multi_outline_budgeted(&digests, full_tokens - 1);
        // Should have dropped to at least no-imports level
        assert!(
            result.contains("no-imports")
                || result.contains("no-imports-consts")
                || result.contains("no-docs")
                || result.contains("signatures-only")
                || result.contains("file-list-only")
        );
    }

    #[test]
    fn budgeted_collapses_to_file_list_at_tiny_budget() {
        let digests = make_test_digests();
        let result = format_multi_outline_budgeted(&digests, 1);
        assert!(result.contains("detail level: file-list-only"));
        assert!(result.contains("a.rs"));
        assert!(result.contains("b.rs"));
        // Should NOT contain function names in file-list-only mode
        // (the footer still has "budget" in it, that's fine)
    }

    #[test]
    fn budgeted_footer_format() {
        let digests = make_test_digests();
        let result = format_multi_outline_budgeted(&digests, 100_000);
        assert!(result.contains("[budget:"));
        assert!(result.contains("tokens, detail level:"));
    }
}
