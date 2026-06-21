//! Documentation coverage scanner.
//!
//! Uses regex to find public function/struct/type declarations
//! and checks for preceding doc comments.

use regex::Regex;
use serde::Serialize;

#[derive(Debug, Serialize, PartialEq)]
pub struct DocReport {
    pub file: String,
    pub total_public: usize,
    pub documented: usize,
    pub undocumented: Vec<UndocumentedItem>,
    pub coverage_pct: f64,
}

#[derive(Debug, Serialize, PartialEq, Clone)]
pub struct UndocumentedItem {
    pub name: String,
    pub kind: String,
    pub line: usize,
}

/// Scan a source file for documentation coverage.
pub fn scan(filename: &str, source: &str) -> DocReport {
    let lines: Vec<&str> = source.lines().collect();
    let lang = detect_language(filename);

    let declarations = find_public_declarations(&lines, lang);
    let total_public = declarations.len();

    let mut documented = 0;
    let mut undocumented = Vec::new();

    for decl in &declarations {
        if has_doc_comment(&lines, decl.line, lang) {
            documented += 1;
        } else {
            undocumented.push(UndocumentedItem {
                name: decl.name.clone(),
                kind: decl.kind.clone(),
                line: decl.line,
            });
        }
    }

    let coverage_pct = if total_public > 0 {
        (documented as f64 / total_public as f64) * 100.0
    } else {
        100.0
    };

    DocReport {
        file: filename.to_string(),
        total_public,
        documented,
        undocumented,
        coverage_pct,
    }
}

#[derive(Debug, Clone, Copy)]
enum Language {
    Rust,
    Python,
    Go,
    TypeScript,
    Elixir,
    CSharp,
    Other,
}

fn detect_language(filename: &str) -> Language {
    if filename.ends_with(".rs") {
        Language::Rust
    } else if filename.ends_with(".py") {
        Language::Python
    } else if filename.ends_with(".go") {
        Language::Go
    } else if filename.ends_with(".ts")
        || filename.ends_with(".tsx")
        || filename.ends_with(".js")
        || filename.ends_with(".jsx")
    {
        Language::TypeScript
    } else if filename.ends_with(".ex") || filename.ends_with(".exs") {
        Language::Elixir
    } else if filename.ends_with(".cs") {
        Language::CSharp
    } else {
        Language::Other
    }
}

struct Declaration {
    name: String,
    kind: String,
    line: usize, // 1-indexed
}

fn find_public_declarations(lines: &[&str], lang: Language) -> Vec<Declaration> {
    let mut decls = Vec::new();

    let patterns: Vec<(Regex, &str)> = match lang {
        Language::Rust => vec![
            (
                Regex::new(r"^\s*pub\s+(?:async\s+)?fn\s+(\w+)").unwrap(),
                "function",
            ),
            (Regex::new(r"^\s*pub\s+struct\s+(\w+)").unwrap(), "struct"),
            (Regex::new(r"^\s*pub\s+enum\s+(\w+)").unwrap(), "enum"),
            (Regex::new(r"^\s*pub\s+trait\s+(\w+)").unwrap(), "trait"),
            (Regex::new(r"^\s*pub\s+type\s+(\w+)").unwrap(), "type"),
        ],
        Language::Python => vec![
            (
                Regex::new(r"^(?:async\s+)?def\s+([a-zA-Z]\w*)").unwrap(),
                "function",
            ),
            (Regex::new(r"^class\s+(\w+)").unwrap(), "class"),
        ],
        Language::Go => vec![
            (
                Regex::new(r"^func\s+(?:\([^)]+\)\s+)?([A-Z]\w*)").unwrap(),
                "function",
            ),
            (
                Regex::new(r"^type\s+([A-Z]\w*)\s+struct").unwrap(),
                "struct",
            ),
            (
                Regex::new(r"^type\s+([A-Z]\w*)\s+interface").unwrap(),
                "interface",
            ),
        ],
        Language::TypeScript => vec![
            (
                Regex::new(r"^\s*(?:export\s+)?(?:async\s+)?function\s+(\w+)").unwrap(),
                "function",
            ),
            (
                Regex::new(r"^\s*(?:export\s+)?class\s+(\w+)").unwrap(),
                "class",
            ),
            (
                Regex::new(r"^\s*(?:export\s+)?interface\s+(\w+)").unwrap(),
                "interface",
            ),
            (
                Regex::new(r"^\s*(?:export\s+)?type\s+(\w+)").unwrap(),
                "type",
            ),
        ],
        Language::Elixir => vec![
            (Regex::new(r"^\s*def\s+(\w+)").unwrap(), "function"),
            (Regex::new(r"^\s*defmodule\s+([\w.]+)").unwrap(), "module"),
        ],
        Language::CSharp => vec![
            (
                Regex::new(r"^\s*public\s+(?:(?:static|sealed|abstract|partial)\s+)*(?:class|struct|interface|record|enum)\s+(\w+)").unwrap(),
                "type",
            ),
            (
                Regex::new(r"^\s*public\s+(?:(?:static|virtual|override|abstract|async|sealed|new)\s+)*(?:[\w<>\[\]?,\s]+\s+)(\w+)\s*\(").unwrap(),
                "method",
            ),
        ],
        Language::Other => vec![(
            Regex::new(r"^\s*(?:pub\s+)?(?:async\s+)?(?:fn|func|def|function)\s+(\w+)").unwrap(),
            "function",
        )],
    };

    for (i, line) in lines.iter().enumerate() {
        // Skip private Python functions
        if matches!(lang, Language::Python) {
            let trimmed = line.trim();
            if trimmed.starts_with("def _") && !trimmed.starts_with("def __init__") {
                continue;
            }
        }

        for (re, kind) in &patterns {
            if let Some(cap) = re.captures(line) {
                decls.push(Declaration {
                    name: cap[1].to_string(),
                    kind: kind.to_string(),
                    line: i + 1,
                });
                break;
            }
        }
    }

    decls
}

fn has_doc_comment(lines: &[&str], decl_line: usize, lang: Language) -> bool {
    // Python docstrings are inside the function — check line after def first
    if matches!(lang, Language::Python) && decl_line < lines.len() {
        let next = lines[decl_line].trim(); // 0-indexed = decl_line (line after def)
        if next.starts_with("\"\"\"") || next.starts_with("'''") {
            return true;
        }
    }

    if decl_line <= 1 {
        return false;
    }

    // Look at lines immediately above the declaration
    let mut i = decl_line - 2; // 0-indexed line above
    loop {
        if i >= lines.len() {
            return false;
        }
        let trimmed = lines[i].trim();

        match lang {
            Language::Rust => {
                if trimmed.starts_with("///") || trimmed.starts_with("//!") {
                    return true;
                }
                if trimmed.starts_with("#[") || trimmed.is_empty() {
                    if i == 0 {
                        return false;
                    }
                    i -= 1;
                    continue;
                }
            }
            Language::Python => {
                // Check for # comment above
                if trimmed.starts_with('#') && !trimmed.starts_with("#!") {
                    return true;
                }
            }
            Language::Go => {
                if trimmed.starts_with("//") {
                    return true;
                }
            }
            Language::TypeScript => {
                if trimmed.starts_with("/**")
                    || trimmed.starts_with("*")
                    || trimmed.starts_with("//")
                {
                    return true;
                }
            }
            Language::Elixir => {
                if trimmed.starts_with("@doc")
                    || trimmed.starts_with("@moduledoc")
                    || trimmed.starts_with("@spec")
                {
                    return true;
                }
                if trimmed.is_empty() {
                    if i == 0 {
                        return false;
                    }
                    i -= 1;
                    continue;
                }
            }
            Language::CSharp => {
                if trimmed.starts_with("///") {
                    return true;
                }
                // Skip attributes like [Obsolete], [HttpGet], etc.
                if trimmed.starts_with('[') || trimmed.is_empty() {
                    if i == 0 {
                        return false;
                    }
                    i -= 1;
                    continue;
                }
            }
            Language::Other => {
                if trimmed.starts_with("///")
                    || trimmed.starts_with("//")
                    || trimmed.starts_with("/**")
                {
                    return true;
                }
            }
        }

        return false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // === Test list (TDD) ===
    // [x] empty file: 100% coverage
    // [x] rust: documented pub fn detected
    // [x] rust: undocumented pub fn detected
    // [x] rust: private fn ignored
    // [x] python: documented def detected
    // [x] python: private _func ignored
    // [x] go: exported (capitalized) function detected
    // [x] typescript: export function detected
    // [x] elixir: def with @doc detected
    // [x] multiple declarations in one file

    #[test]
    fn empty_file() {
        let result = scan("test.rs", "");
        assert_eq!(result.total_public, 0);
        assert_eq!(result.coverage_pct, 100.0);
    }

    #[test]
    fn rust_documented_pub_fn() {
        let source = "/// Does a thing.\npub fn foo() {}\n";
        let result = scan("test.rs", source);
        assert_eq!(result.total_public, 1);
        assert_eq!(result.documented, 1);
        assert!(result.undocumented.is_empty());
    }

    #[test]
    fn rust_undocumented_pub_fn() {
        let source = "pub fn foo() {}\n";
        let result = scan("test.rs", source);
        assert_eq!(result.total_public, 1);
        assert_eq!(result.documented, 0);
        assert_eq!(result.undocumented.len(), 1);
        assert_eq!(result.undocumented[0].name, "foo");
    }

    #[test]
    fn rust_private_fn_ignored() {
        let source = "fn private_fn() {}\npub fn public_fn() {}\n";
        let result = scan("test.rs", source);
        assert_eq!(result.total_public, 1); // only pub fn
    }

    #[test]
    fn python_documented_def() {
        let source = "def process():\n    \"\"\"Process things.\"\"\"\n    pass\n";
        let result = scan("test.py", source);
        assert_eq!(result.total_public, 1);
        assert_eq!(result.documented, 1);
    }

    #[test]
    fn python_private_ignored() {
        let source = "def _private():\n    pass\ndef public():\n    pass\n";
        let result = scan("test.py", source);
        assert_eq!(result.total_public, 1); // _private skipped
    }

    #[test]
    fn go_exported_function() {
        let source = "// ProcessData handles data processing.\nfunc ProcessData() {}\n";
        let result = scan("test.go", source);
        assert_eq!(result.total_public, 1);
        assert_eq!(result.documented, 1);
    }

    #[test]
    fn typescript_export_function() {
        let source = "/** Fetch user data. */\nexport function fetchUser() {}\n";
        let result = scan("test.ts", source);
        assert_eq!(result.total_public, 1);
        assert_eq!(result.documented, 1);
    }

    #[test]
    fn elixir_def_with_doc() {
        let source = "@doc \"Processes data.\"\ndef process(data) do\n  data\nend\n";
        let result = scan("test.ex", source);
        assert_eq!(result.total_public, 1);
        assert_eq!(result.documented, 1);
    }

    #[test]
    fn csharp_documented_public_class() {
        let source = "/// <summary>A user service.</summary>\npublic class UserService {}\n";
        let result = scan("test.cs", source);
        assert_eq!(result.total_public, 1);
        assert_eq!(result.documented, 1);
        assert!(result.undocumented.is_empty());
    }

    #[test]
    fn csharp_undocumented_public_method() {
        let source = "public class Svc {}\npublic int GetCount(string key)\n{\n}\n";
        let result = scan("test.cs", source);
        assert_eq!(result.total_public, 2);
        assert_eq!(result.documented, 0);
        assert_eq!(result.undocumented.len(), 2);
    }

    #[test]
    fn csharp_xml_doc_with_attribute() {
        let source =
            "/// <summary>Gets a user.</summary>\n[HttpGet]\npublic User GetUser(int id) {}\n";
        let result = scan("test.cs", source);
        assert_eq!(result.total_public, 1);
        assert_eq!(result.documented, 1);
    }

    #[test]
    fn multiple_declarations() {
        let source = r#"/// Documented.
pub fn a() {}
pub fn b() {}
/// Also documented.
pub struct C {}
pub enum D {}
"#;
        let result = scan("test.rs", source);
        assert_eq!(result.total_public, 4);
        assert_eq!(result.documented, 2); // a and C
        assert_eq!(result.undocumented.len(), 2); // b and D
    }
}
