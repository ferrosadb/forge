//! Extracts a structured outline from a source file.
//!
//! Supports Rust, Python, Go, TypeScript/JS, Elixir, Java, C++, and C#.

use regex::Regex;
use serde::Serialize;

// ---------------------------------------------------------------------------
// Output types
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
pub struct FileOutline {
    pub file: String,
    pub language: String,
    pub line_count: usize,
    pub modules: Vec<ModuleDecl>,
    pub public_functions: Vec<FuncDecl>,
    pub private_functions: Vec<FuncDecl>,
    pub types: Vec<TypeDecl>,
    pub imports: Vec<ImportDecl>,
    pub constants: Vec<ConstDecl>,
}

#[derive(Debug, Serialize)]
pub struct ModuleDecl {
    pub name: String,
    pub line: usize,
}

#[derive(Debug, Serialize)]
pub struct FuncDecl {
    pub name: String,
    pub args: String,
    pub line: usize,
    pub end_line: Option<usize>,
}

#[derive(Debug, Serialize)]
pub struct TypeDecl {
    pub name: String,
    pub kind: String,
    pub line: usize,
}

#[derive(Debug, Serialize)]
pub struct ImportDecl {
    pub target: String,
    pub line: usize,
}

#[derive(Debug, Serialize)]
pub struct ConstDecl {
    pub name: String,
    pub line: usize,
}

// ---------------------------------------------------------------------------
// Language detection
// ---------------------------------------------------------------------------

fn detect_language(filename: &str) -> &'static str {
    let ext = filename.rsplit('.').next().unwrap_or("");
    match ext {
        "rs" => "rust",
        "py" => "python",
        "go" => "go",
        "ts" | "tsx" | "js" | "jsx" => "typescript",
        "ex" | "exs" => "elixir",
        "java" => "java",
        "cpp" | "cc" | "h" | "hpp" => "cpp",
        "cs" => "csharp",
        _ => "unknown",
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Capture the argument string from the first `(...)` on a line.
/// Returns an empty string if no parens are found.
fn extract_args(line: &str) -> String {
    if let Some(open) = line.find('(') {
        // Find the matching close paren (simple: first ')' after open)
        if let Some(close) = line[open..].find(')') {
            return line[open + 1..open + close].trim().to_string();
        }
        // No closing paren on this line; return everything after '('
        return line[open + 1..].trim().to_string();
    }
    String::new()
}

/// Walk lines forward tracking brace depth; return the line number (1-based)
/// where the depth returns to 0 after we enter the first `{`.
fn find_brace_end(lines: &[&str], start: usize) -> Option<usize> {
    let mut depth: i32 = 0;
    let mut entered = false;
    for (i, line) in lines[start..].iter().enumerate() {
        for ch in line.chars() {
            if ch == '{' {
                depth += 1;
                entered = true;
            } else if ch == '}' {
                depth -= 1;
            }
        }
        if entered && depth <= 0 {
            return Some(start + i + 1); // 1-based
        }
    }
    None
}

/// For Python/Elixir: find end line by looking for the next line at the same
/// or lower indentation level (that isn't blank or a comment).
fn find_indent_end(lines: &[&str], start: usize, def_indent: usize) -> Option<usize> {
    for (i, line) in lines[start + 1..].iter().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let indent = line.len() - line.trim_start().len();
        if indent <= def_indent {
            // The previous non-blank line is the end
            return Some(start + i + 1); // 1-based line of the triggering line - 1
        }
    }
    // Reached EOF
    Some(lines.len())
}

/// Return the indentation level (number of leading spaces/tabs) of a line.
fn indent_of(line: &str) -> usize {
    line.len() - line.trim_start().len()
}

// ---------------------------------------------------------------------------
// Language-specific extractors
// ---------------------------------------------------------------------------

fn extract_rust(
    lines: &[&str],
    modules: &mut Vec<ModuleDecl>,
    public_functions: &mut Vec<FuncDecl>,
    private_functions: &mut Vec<FuncDecl>,
    types: &mut Vec<TypeDecl>,
    imports: &mut Vec<ImportDecl>,
    constants: &mut Vec<ConstDecl>,
) {
    // Precompile all patterns once.
    let re_mod = Regex::new(r"^\s*(pub\s+)?mod\s+(\w+)").unwrap();
    let re_pub_fn = Regex::new(r"^\s*pub\s+(async\s+)?fn\s+(\w+)\s*(\([^)]*\))?").unwrap();
    let re_priv_fn = Regex::new(r"^\s*fn\s+(\w+)\s*(\([^)]*\))?").unwrap();
    let re_type = Regex::new(r"^\s*(pub\s+)?(struct|enum|trait|type)\s+(\w+)").unwrap();
    let re_use = Regex::new(r"^\s*use\s+(.+);").unwrap();
    let re_const = Regex::new(r"^\s*(pub\s+)?(const|static)\s+(\w+)").unwrap();

    // Track if we are inside a test module so we can skip.
    let mut in_test_block = false;
    let mut test_depth: i32 = 0;

    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        let lineno = i + 1;
        let trimmed = line.trim();

        // Detect #[cfg(test)] followed by mod tests
        if trimmed.starts_with("#[cfg(test)]") {
            // Peek ahead for `mod`
            if let Some(next) = lines.get(i + 1) {
                if next.trim().starts_with("mod ") {
                    in_test_block = true;
                    test_depth = 0;
                }
            }
        }

        if in_test_block {
            for ch in line.chars() {
                if ch == '{' {
                    test_depth += 1;
                } else if ch == '}' {
                    test_depth -= 1;
                }
            }
            if test_depth <= 0 && trimmed.contains('}') {
                in_test_block = false;
            }
            i += 1;
            continue;
        }

        if let Some(caps) = re_use.captures(trimmed) {
            imports.push(ImportDecl {
                target: caps[1].trim().to_string(),
                line: lineno,
            });
        } else if let Some(caps) = re_const.captures(line) {
            constants.push(ConstDecl {
                name: caps[3].to_string(),
                line: lineno,
            });
        } else if let Some(caps) = re_pub_fn.captures(line) {
            let name = caps[2].to_string();
            let args = extract_args(line);
            let end_line = find_brace_end(lines, i);
            public_functions.push(FuncDecl {
                name,
                args,
                line: lineno,
                end_line,
            });
        } else if let Some(caps) = re_priv_fn.captures(line) {
            let name = caps[1].to_string();
            let args = extract_args(line);
            let end_line = find_brace_end(lines, i);
            private_functions.push(FuncDecl {
                name,
                args,
                line: lineno,
                end_line,
            });
        } else if let Some(caps) = re_type.captures(line) {
            let kind = caps[2].to_string();
            let name = caps[3].to_string();
            types.push(TypeDecl {
                name,
                kind,
                line: lineno,
            });
        } else if let Some(caps) = re_mod.captures(line) {
            modules.push(ModuleDecl {
                name: caps[2].to_string(),
                line: lineno,
            });
        }

        i += 1;
    }
}

fn extract_elixir(
    lines: &[&str],
    modules: &mut Vec<ModuleDecl>,
    public_functions: &mut Vec<FuncDecl>,
    private_functions: &mut Vec<FuncDecl>,
    types: &mut Vec<TypeDecl>,
    imports: &mut Vec<ImportDecl>,
    constants: &mut Vec<ConstDecl>,
) {
    let re_defmodule = Regex::new(r"^\s*defmodule\s+([\w.]+)").unwrap();
    let re_def = Regex::new(r"^\s*def\s+(\w+)\s*(\()?").unwrap();
    let re_defp = Regex::new(r"^\s*defp\s+(\w+)\s*(\()?").unwrap();
    let re_use = Regex::new(r"^\s*(use|import|alias)\s+(.+)").unwrap();
    // @type, @typep, @opaque — extract the type name
    let re_type = Regex::new(r"^\s*@(type|typep|opaque)\s+(\w+)").unwrap();
    // @callback — extract the callback function name
    let re_callback = Regex::new(r"^\s*@callback\s+(\w+)").unwrap();
    // @spec — extract the function name from the spec
    let re_spec = Regex::new(r"^\s*@spec\s+(\w+)").unwrap();
    // Module attributes excluding doc/spec/type/callback/etc.
    let skip_attrs: &[&str] = &[
        "doc",
        "moduledoc",
        "spec",
        "type",
        "typep",
        "callback",
        "impl",
        "behaviour",
        "derive",
        "enforce_keys",
        "typedoc",
        "opaque",
    ];
    let re_attr = Regex::new(r"^\s*@(\w+)").unwrap();

    for (i, line) in lines.iter().enumerate() {
        let lineno = i + 1;
        let trimmed = line.trim();

        if trimmed.starts_with('#') {
            continue;
        }

        if let Some(caps) = re_defmodule.captures(trimmed) {
            modules.push(ModuleDecl {
                name: caps[1].to_string(),
                line: lineno,
            });
        } else if let Some(caps) = re_def.captures(trimmed) {
            let name = caps[1].to_string();
            let args = extract_args(trimmed);
            let end_line = find_indent_end(lines, i, indent_of(line));
            public_functions.push(FuncDecl {
                name,
                args,
                line: lineno,
                end_line,
            });
        } else if let Some(caps) = re_defp.captures(trimmed) {
            let name = caps[1].to_string();
            let args = extract_args(trimmed);
            let end_line = find_indent_end(lines, i, indent_of(line));
            private_functions.push(FuncDecl {
                name,
                args,
                line: lineno,
                end_line,
            });
        } else if let Some(caps) = re_type.captures(trimmed) {
            let kind = caps[1].to_string(); // "type", "typep", or "opaque"
            let name = caps[2].to_string();
            types.push(TypeDecl {
                name,
                kind: format!("@{}", kind),
                line: lineno,
            });
        } else if let Some(caps) = re_callback.captures(trimmed) {
            types.push(TypeDecl {
                name: caps[1].to_string(),
                kind: "@callback".to_string(),
                line: lineno,
            });
        } else if let Some(caps) = re_spec.captures(trimmed) {
            types.push(TypeDecl {
                name: caps[1].to_string(),
                kind: "@spec".to_string(),
                line: lineno,
            });
        } else if let Some(caps) = re_use.captures(trimmed) {
            imports.push(ImportDecl {
                target: caps[2].trim_end_matches([',', ';']).trim().to_string(),
                line: lineno,
            });
        } else if let Some(caps) = re_attr.captures(trimmed) {
            let attr_name = caps[1].to_string();
            if !skip_attrs.contains(&attr_name.as_str()) {
                constants.push(ConstDecl {
                    name: format!("@{}", attr_name),
                    line: lineno,
                });
            }
        }
    }
}

fn extract_python(
    lines: &[&str],
    modules: &mut Vec<ModuleDecl>,
    public_functions: &mut Vec<FuncDecl>,
    private_functions: &mut Vec<FuncDecl>,
    imports: &mut Vec<ImportDecl>,
    constants: &mut Vec<ConstDecl>,
) {
    let re_class = Regex::new(r"^\s*class\s+(\w+)").unwrap();
    let re_def = Regex::new(r"^\s*def\s+(\w+)\s*\(").unwrap();
    let re_import = Regex::new(r"^\s*(import\s+\S+|from\s+\S+\s+import\s+.+)").unwrap();
    let re_const = Regex::new(r"^\s*([A-Z][A-Z0-9_]+)\s*=").unwrap();

    // Track test classes to skip their methods
    let mut in_test_class = false;
    let mut test_class_indent: Option<usize> = None;

    for (i, line) in lines.iter().enumerate() {
        let lineno = i + 1;
        let trimmed = line.trim();

        if trimmed.starts_with('#') || trimmed.is_empty() {
            continue;
        }

        let current_indent = indent_of(line);

        // Check if we've left the test class
        if in_test_class {
            if let Some(ti) = test_class_indent {
                if current_indent <= ti && !trimmed.is_empty() {
                    in_test_class = false;
                    test_class_indent = None;
                }
            }
        }

        if let Some(caps) = re_class.captures(line) {
            let name = caps[1].to_string();
            if name.starts_with("Test") || name.ends_with("Test") || name.ends_with("Tests") {
                in_test_class = true;
                test_class_indent = Some(current_indent);
            }
            modules.push(ModuleDecl { name, line: lineno });
        } else if let Some(caps) = re_def.captures(line) {
            if in_test_class {
                continue;
            }
            let name = caps[1].to_string();
            let args = extract_args(line);
            let end_line = find_indent_end(lines, i, current_indent);
            if name.starts_with('_') {
                private_functions.push(FuncDecl {
                    name,
                    args,
                    line: lineno,
                    end_line,
                });
            } else {
                public_functions.push(FuncDecl {
                    name,
                    args,
                    line: lineno,
                    end_line,
                });
            }
        } else if let Some(caps) = re_import.captures(line) {
            imports.push(ImportDecl {
                target: caps[1].trim().to_string(),
                line: lineno,
            });
        } else if let Some(caps) = re_const.captures(line) {
            constants.push(ConstDecl {
                name: caps[1].to_string(),
                line: lineno,
            });
        }
    }
}

fn extract_go(
    lines: &[&str],
    modules: &mut Vec<ModuleDecl>,
    public_functions: &mut Vec<FuncDecl>,
    private_functions: &mut Vec<FuncDecl>,
    types: &mut Vec<TypeDecl>,
    imports: &mut Vec<ImportDecl>,
    constants: &mut Vec<ConstDecl>,
) {
    let re_package = Regex::new(r"^\s*package\s+(\w+)").unwrap();
    let re_func = Regex::new(r"^\s*func\s+(?:\([^)]*\)\s*)?(\w+)\s*\(").unwrap();
    let re_type = Regex::new(r"^\s*type\s+(\w+)\s+(struct|interface)").unwrap();
    let re_import_single = Regex::new(r#"^\s*import\s+"([^"]+)""#).unwrap();
    let re_import_item = Regex::new(r#"^\s*"([^"]+)""#).unwrap();
    let re_const = Regex::new(r"^\s*(const|var)\s+(\w+)").unwrap();

    let mut in_import_block = false;
    let mut in_test_func = false;
    let mut test_depth: i32 = 0;

    for (i, line) in lines.iter().enumerate() {
        let lineno = i + 1;
        let trimmed = line.trim();

        // Handle multi-line import blocks
        if trimmed.starts_with("import (") || trimmed == "import(" {
            in_import_block = true;
            continue;
        }
        if in_import_block {
            if trimmed == ")" {
                in_import_block = false;
                continue;
            }
            if let Some(caps) = re_import_item.captures(trimmed) {
                imports.push(ImportDecl {
                    target: caps[1].to_string(),
                    line: lineno,
                });
            }
            continue;
        }

        // Track test functions
        if in_test_func {
            for ch in line.chars() {
                if ch == '{' {
                    test_depth += 1;
                } else if ch == '}' {
                    test_depth -= 1;
                }
            }
            if test_depth <= 0 {
                in_test_func = false;
            }
            continue;
        }

        if let Some(caps) = re_package.captures(trimmed) {
            modules.push(ModuleDecl {
                name: caps[1].to_string(),
                line: lineno,
            });
        } else if let Some(caps) = re_import_single.captures(trimmed) {
            imports.push(ImportDecl {
                target: caps[1].to_string(),
                line: lineno,
            });
        } else if let Some(caps) = re_func.captures(line) {
            let name = caps[1].to_string();
            // Skip test functions
            if name.starts_with("Test")
                || name.starts_with("Benchmark")
                || name.starts_with("Example")
            {
                in_test_func = true;
                test_depth = 0;
                for ch in line.chars() {
                    if ch == '{' {
                        test_depth += 1;
                    } else if ch == '}' {
                        test_depth -= 1;
                    }
                }
                continue;
            }
            let args = extract_args(line);
            let end_line = find_brace_end(lines, i);
            let first_char = name.chars().next().unwrap_or('a');
            if first_char.is_uppercase() {
                public_functions.push(FuncDecl {
                    name,
                    args,
                    line: lineno,
                    end_line,
                });
            } else {
                private_functions.push(FuncDecl {
                    name,
                    args,
                    line: lineno,
                    end_line,
                });
            }
        } else if let Some(caps) = re_type.captures(trimmed) {
            types.push(TypeDecl {
                name: caps[1].to_string(),
                kind: caps[2].to_string(),
                line: lineno,
            });
        } else if let Some(caps) = re_const.captures(trimmed) {
            constants.push(ConstDecl {
                name: caps[2].to_string(),
                line: lineno,
            });
        }
    }
}

fn extract_typescript(
    lines: &[&str],
    modules: &mut Vec<ModuleDecl>,
    public_functions: &mut Vec<FuncDecl>,
    private_functions: &mut Vec<FuncDecl>,
    types: &mut Vec<TypeDecl>,
    imports: &mut Vec<ImportDecl>,
    constants: &mut Vec<ConstDecl>,
) {
    let re_class = Regex::new(r"^\s*(export\s+)?(abstract\s+)?class\s+(\w+)").unwrap();
    let re_interface = Regex::new(r"^\s*(export\s+)?interface\s+(\w+)").unwrap();
    let re_export_fn = Regex::new(r"^\s*export\s+(async\s+)?function\s+(\w+)\s*\(").unwrap();
    let re_fn = Regex::new(r"^\s*(async\s+)?function\s+(\w+)\s*\(").unwrap();
    let re_export_type = Regex::new(r"^\s*export\s+type\s+(\w+)").unwrap();
    let re_import = Regex::new(r"^\s*import\s+").unwrap();
    let re_import_from = Regex::new(r#"from\s+['"]([^'"]+)['"]"#).unwrap();
    let re_export_const = Regex::new(r"^\s*(export\s+)?const\s+(\w+)").unwrap();

    for (i, line) in lines.iter().enumerate() {
        let lineno = i + 1;
        let trimmed = line.trim();

        if trimmed.starts_with("//") {
            continue;
        }

        if re_import.is_match(trimmed) {
            // Extract the 'from' module if present
            let target = if let Some(caps) = re_import_from.captures(trimmed) {
                caps[1].to_string()
            } else {
                trimmed.to_string()
            };
            imports.push(ImportDecl {
                target,
                line: lineno,
            });
        } else if let Some(caps) = re_export_fn.captures(line) {
            let name = caps[2].to_string();
            let args = extract_args(line);
            let end_line = find_brace_end(lines, i);
            public_functions.push(FuncDecl {
                name,
                args,
                line: lineno,
                end_line,
            });
        } else if let Some(caps) = re_fn.captures(line) {
            let name = caps[2].to_string();
            let args = extract_args(line);
            let end_line = find_brace_end(lines, i);
            private_functions.push(FuncDecl {
                name,
                args,
                line: lineno,
                end_line,
            });
        } else if let Some(caps) = re_export_type.captures(trimmed) {
            types.push(TypeDecl {
                name: caps[1].to_string(),
                kind: "type".to_string(),
                line: lineno,
            });
        } else if let Some(caps) = re_interface.captures(trimmed) {
            let exported = caps.get(1).is_some();
            let name = caps[2].to_string();
            types.push(TypeDecl {
                name: name.clone(),
                kind: "interface".to_string(),
                line: lineno,
            });
            if exported {
                modules.push(ModuleDecl { name, line: lineno });
            }
        } else if let Some(caps) = re_class.captures(trimmed) {
            modules.push(ModuleDecl {
                name: caps[3].to_string(),
                line: lineno,
            });
        } else if let Some(caps) = re_export_const.captures(trimmed) {
            constants.push(ConstDecl {
                name: caps[2].to_string(),
                line: lineno,
            });
        }
    }
}

fn extract_java(
    lines: &[&str],
    modules: &mut Vec<ModuleDecl>,
    public_functions: &mut Vec<FuncDecl>,
    private_functions: &mut Vec<FuncDecl>,
    imports: &mut Vec<ImportDecl>,
    constants: &mut Vec<ConstDecl>,
) {
    let re_class = Regex::new(r"^\s*(public\s+|private\s+|protected\s+|abstract\s+|final\s+)*(class|interface|enum)\s+(\w+)").unwrap();
    let re_import = Regex::new(r"^\s*import\s+(static\s+)?(.+);").unwrap();
    let re_pub_method = Regex::new(r"^\s*public\s+(?:(?:static|final|abstract|synchronized)\s+)*(?:\w[\w<>\[\],\s]*\s+)?(\w+)\s*\(").unwrap();
    let re_priv_method = Regex::new(r"^\s*private\s+(?:(?:static|final|abstract|synchronized)\s+)*(?:\w[\w<>\[\],\s]*\s+)?(\w+)\s*\(").unwrap();
    let re_const =
        Regex::new(r"^\s*(?:public\s+|private\s+|protected\s+)?static\s+final\s+\w+\s+(\w+)")
            .unwrap();

    for (i, line) in lines.iter().enumerate() {
        let lineno = i + 1;
        let trimmed = line.trim();

        if trimmed.starts_with("//") || trimmed.starts_with('*') || trimmed.starts_with("/*") {
            continue;
        }

        if let Some(caps) = re_import.captures(trimmed) {
            imports.push(ImportDecl {
                target: caps[2].trim().to_string(),
                line: lineno,
            });
        } else if let Some(caps) = re_class.captures(trimmed) {
            let kind = caps[2].to_string();
            let name = caps[3].to_string();
            modules.push(ModuleDecl {
                name: name.clone(),
                line: lineno,
            });
            // Also add to types? Per spec, classes/interfaces/enums go in modules for Java.
            // We don't add to types here since they are already in modules.
            let _ = kind;
        } else if let Some(caps) = re_const.captures(trimmed) {
            constants.push(ConstDecl {
                name: caps[1].to_string(),
                line: lineno,
            });
        } else if let Some(caps) = re_pub_method.captures(line) {
            let name = caps[1].to_string();
            // Skip constructors that match class names (heuristic: skip if it looks like a constructor)
            let args = extract_args(line);
            let end_line = find_brace_end(lines, i);
            public_functions.push(FuncDecl {
                name,
                args,
                line: lineno,
                end_line,
            });
        } else if let Some(caps) = re_priv_method.captures(line) {
            let name = caps[1].to_string();
            let args = extract_args(line);
            let end_line = find_brace_end(lines, i);
            private_functions.push(FuncDecl {
                name,
                args,
                line: lineno,
                end_line,
            });
        }
    }
}

fn extract_cpp(
    lines: &[&str],
    modules: &mut Vec<ModuleDecl>,
    public_functions: &mut Vec<FuncDecl>,
    private_functions: &mut Vec<FuncDecl>,
    types: &mut Vec<TypeDecl>,
    imports: &mut Vec<ImportDecl>,
    constants: &mut Vec<ConstDecl>,
) {
    let re_include = Regex::new(r#"^\s*#include\s+[<"]([^>"]+)[>"]"#).unwrap();
    let re_class = Regex::new(r"^\s*(class|struct)\s+(\w+)").unwrap();
    let re_enum = Regex::new(r"^\s*enum\s+(?:class\s+)?(\w+)").unwrap();
    let re_namespace = Regex::new(r"^\s*namespace\s+(\w+)").unwrap();
    let re_func = Regex::new(r"^\s*(?:(?:inline|static|virtual|explicit|constexpr|override)\s+)*(?:[\w:*&<>\[\]]+\s+)+(\w+)\s*\(").unwrap();
    let re_const =
        Regex::new(r"^\s*(?:constexpr|const|static\s+const(?:expr)?)\s+\w+\s+(\w+)").unwrap();

    let mut visibility = "public"; // default for structs; track for classes
    let mut class_depth: i32 = 0;
    let mut in_class = false;

    for (i, line) in lines.iter().enumerate() {
        let lineno = i + 1;
        let trimmed = line.trim();

        if trimmed.starts_with("//") || trimmed.starts_with('*') || trimmed.starts_with("/*") {
            continue;
        }

        // Track class body for visibility
        if in_class {
            for ch in line.chars() {
                if ch == '{' {
                    class_depth += 1;
                } else if ch == '}' {
                    class_depth -= 1;
                }
            }
            if class_depth <= 0 {
                in_class = false;
                visibility = "public";
            }
            if trimmed == "public:" || trimmed.starts_with("public:") {
                visibility = "public";
            } else if trimmed == "private:" || trimmed.starts_with("private:") {
                visibility = "private";
            } else if trimmed == "protected:" || trimmed.starts_with("protected:") {
                visibility = "protected";
            }
        }

        if let Some(caps) = re_include.captures(trimmed) {
            imports.push(ImportDecl {
                target: caps[1].to_string(),
                line: lineno,
            });
        } else if let Some(caps) = re_namespace.captures(trimmed) {
            modules.push(ModuleDecl {
                name: caps[1].to_string(),
                line: lineno,
            });
        } else if let Some(caps) = re_class.captures(trimmed) {
            let kind = caps[1].to_string();
            let name = caps[2].to_string();
            types.push(TypeDecl {
                name: name.clone(),
                kind,
                line: lineno,
            });
            modules.push(ModuleDecl { name, line: lineno });
            in_class = true;
            class_depth = 0;
            visibility = "public";
        } else if let Some(caps) = re_enum.captures(trimmed) {
            types.push(TypeDecl {
                name: caps[1].to_string(),
                kind: "enum".to_string(),
                line: lineno,
            });
        } else if let Some(caps) = re_const.captures(trimmed) {
            constants.push(ConstDecl {
                name: caps[1].to_string(),
                line: lineno,
            });
        } else if let Some(caps) = re_func.captures(line) {
            // Skip preprocessor lines
            if trimmed.starts_with('#') {
                continue;
            }
            let name = caps[1].to_string();
            // Skip common false positives
            if matches!(
                name.as_str(),
                "if" | "for" | "while" | "switch" | "catch" | "return"
            ) {
                continue;
            }
            let args = extract_args(line);
            let end_line = find_brace_end(lines, i);
            if visibility == "private" {
                private_functions.push(FuncDecl {
                    name,
                    args,
                    line: lineno,
                    end_line,
                });
            } else {
                public_functions.push(FuncDecl {
                    name,
                    args,
                    line: lineno,
                    end_line,
                });
            }
        }
    }
}

fn extract_csharp(
    lines: &[&str],
    modules: &mut Vec<ModuleDecl>,
    public_functions: &mut Vec<FuncDecl>,
    private_functions: &mut Vec<FuncDecl>,
    types: &mut Vec<TypeDecl>,
    imports: &mut Vec<ImportDecl>,
    constants: &mut Vec<ConstDecl>,
) {
    let re_namespace = Regex::new(r"^\s*namespace\s+([\w.]+)").unwrap();
    let re_using = Regex::new(r"^\s*using\s+(?:static\s+)?(.+);").unwrap();
    let re_type_decl = Regex::new(
        r"^\s*(?:public|private|internal|protected)\s+(?:(?:static|sealed|abstract|partial)\s+)*(class|struct|interface|record|enum)\s+(\w+)",
    )
    .unwrap();
    let re_pub_method = Regex::new(
        r"^\s*public\s+(?:(?:static|virtual|override|abstract|async|sealed|new)\s+)*(?:[\w<>\[\]?,\s]+\s+)(\w+)\s*\(",
    )
    .unwrap();
    let re_priv_method = Regex::new(
        r"^\s*(?:private|internal|protected)\s+(?:(?:static|virtual|override|abstract|async|sealed|new)\s+)*(?:[\w<>\[\]?,\s]+\s+)(\w+)\s*\(",
    )
    .unwrap();
    let re_property = Regex::new(
        r"^\s*(?:public|private|internal|protected)\s+(?:(?:static|virtual|override|abstract|new)\s+)*[\w<>\[\]?,]+\s+(\w+)\s*\{",
    )
    .unwrap();
    let re_const =
        Regex::new(r"^\s*(?:public\s+|private\s+|internal\s+|protected\s+)?(?:const|static\s+readonly)\s+\w+\s+(\w+)")
            .unwrap();

    for (i, line) in lines.iter().enumerate() {
        let lineno = i + 1;
        let trimmed = line.trim();

        if trimmed.starts_with("//") || trimmed.starts_with('*') || trimmed.starts_with("/*") {
            continue;
        }

        if let Some(caps) = re_using.captures(trimmed) {
            imports.push(ImportDecl {
                target: caps[1].trim().to_string(),
                line: lineno,
            });
        } else if let Some(caps) = re_namespace.captures(trimmed) {
            modules.push(ModuleDecl {
                name: caps[1].to_string(),
                line: lineno,
            });
        } else if let Some(caps) = re_type_decl.captures(trimmed) {
            let kind = caps[1].to_string();
            let name = caps[2].to_string();
            types.push(TypeDecl {
                name: name.clone(),
                kind,
                line: lineno,
            });
            modules.push(ModuleDecl { name, line: lineno });
        } else if let Some(caps) = re_const.captures(trimmed) {
            constants.push(ConstDecl {
                name: caps[1].to_string(),
                line: lineno,
            });
        } else if let Some(caps) = re_pub_method.captures(line) {
            let name = caps[1].to_string();
            // Skip property-like patterns
            if re_property.is_match(line) {
                continue;
            }
            let args = extract_args(line);
            let end_line = find_brace_end(lines, i);
            public_functions.push(FuncDecl {
                name,
                args,
                line: lineno,
                end_line,
            });
        } else if let Some(caps) = re_priv_method.captures(line) {
            let name = caps[1].to_string();
            if re_property.is_match(line) {
                continue;
            }
            let args = extract_args(line);
            let end_line = find_brace_end(lines, i);
            private_functions.push(FuncDecl {
                name,
                args,
                line: lineno,
                end_line,
            });
        }
    }
}

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

pub fn outline(filename: &str, source: &str) -> FileOutline {
    let language = detect_language(filename).to_string();
    let lines: Vec<&str> = source.lines().collect();
    let line_count = lines.len();

    let mut modules = Vec::new();
    let mut public_functions = Vec::new();
    let mut private_functions = Vec::new();
    let mut types = Vec::new();
    let mut imports = Vec::new();
    let mut constants = Vec::new();

    match language.as_str() {
        "rust" => extract_rust(
            &lines,
            &mut modules,
            &mut public_functions,
            &mut private_functions,
            &mut types,
            &mut imports,
            &mut constants,
        ),
        "elixir" => extract_elixir(
            &lines,
            &mut modules,
            &mut public_functions,
            &mut private_functions,
            &mut types,
            &mut imports,
            &mut constants,
        ),
        "python" => extract_python(
            &lines,
            &mut modules,
            &mut public_functions,
            &mut private_functions,
            &mut imports,
            &mut constants,
        ),
        "go" => extract_go(
            &lines,
            &mut modules,
            &mut public_functions,
            &mut private_functions,
            &mut types,
            &mut imports,
            &mut constants,
        ),
        "typescript" => extract_typescript(
            &lines,
            &mut modules,
            &mut public_functions,
            &mut private_functions,
            &mut types,
            &mut imports,
            &mut constants,
        ),
        "java" => extract_java(
            &lines,
            &mut modules,
            &mut public_functions,
            &mut private_functions,
            &mut imports,
            &mut constants,
        ),
        "cpp" => extract_cpp(
            &lines,
            &mut modules,
            &mut public_functions,
            &mut private_functions,
            &mut types,
            &mut imports,
            &mut constants,
        ),
        "csharp" => extract_csharp(
            &lines,
            &mut modules,
            &mut public_functions,
            &mut private_functions,
            &mut types,
            &mut imports,
            &mut constants,
        ),
        _ => {}
    }

    FileOutline {
        file: filename.to_string(),
        language,
        line_count,
        modules,
        public_functions,
        private_functions,
        types,
        imports,
        constants,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_file_returns_empty_outline() {
        let result = outline("foo.rs", "");
        assert_eq!(result.line_count, 0);
        assert!(result.modules.is_empty());
        assert!(result.public_functions.is_empty());
        assert!(result.private_functions.is_empty());
        assert!(result.types.is_empty());
        assert!(result.imports.is_empty());
        assert!(result.constants.is_empty());
        assert_eq!(result.language, "rust");
    }

    #[test]
    fn rust_extracts_pub_fn_struct_use_const() {
        let src = r#"
use std::collections::HashMap;
use serde::Serialize;

pub const MAX_SIZE: usize = 100;
static VERSION: &str = "1.0";

pub struct MyStruct {
    pub field: u32,
}

pub enum Color { Red, Green, Blue }

pub trait Drawable {
    fn draw(&self);
}

pub fn hello(name: &str) -> String {
    format!("Hello, {}", name)
}

pub async fn fetch(url: &str) -> Result<String, ()> {
    Ok(url.to_string())
}

fn internal_helper(x: i32) -> i32 {
    x * 2
}

#[cfg(test)]
mod tests {
    #[test]
    fn it_works() {}
}
"#;
        let result = outline("src/main.rs", src);
        assert_eq!(result.language, "rust");

        let import_targets: Vec<&str> = result.imports.iter().map(|i| i.target.as_str()).collect();
        assert!(import_targets.contains(&"std::collections::HashMap"));
        assert!(import_targets.contains(&"serde::Serialize"));

        let const_names: Vec<&str> = result.constants.iter().map(|c| c.name.as_str()).collect();
        assert!(const_names.contains(&"MAX_SIZE"));
        assert!(const_names.contains(&"VERSION"));

        let type_names: Vec<&str> = result.types.iter().map(|t| t.name.as_str()).collect();
        assert!(type_names.contains(&"MyStruct"));
        assert!(type_names.contains(&"Color"));
        assert!(type_names.contains(&"Drawable"));

        let pub_fn_names: Vec<&str> = result
            .public_functions
            .iter()
            .map(|f| f.name.as_str())
            .collect();
        assert!(pub_fn_names.contains(&"hello"));
        assert!(pub_fn_names.contains(&"fetch"));

        let priv_fn_names: Vec<&str> = result
            .private_functions
            .iter()
            .map(|f| f.name.as_str())
            .collect();
        assert!(priv_fn_names.contains(&"internal_helper"));

        // Test block functions should not appear
        assert!(!pub_fn_names.contains(&"it_works"));
    }

    #[test]
    fn rust_fn_end_line_tracked() {
        let src = "pub fn simple() {\n    42\n}\n";
        let result = outline("a.rs", src);
        assert_eq!(result.public_functions.len(), 1);
        assert_eq!(result.public_functions[0].end_line, Some(3));
    }

    #[test]
    fn elixir_extracts_defmodule_def_defp_use_alias_attr() {
        let src = r#"
defmodule MyApp.Server do
  use GenServer
  alias MyApp.Repo
  import Ecto.Query

  @name :my_server
  @doc "Start the server"
  @moduledoc false

  def start_link(opts) do
    GenServer.start_link(__MODULE__, opts)
  end

  def handle_call(:ping, _from, state) do
    {:reply, :pong, state}
  end

  defp init_state(opts) do
    %{}
  end
end
"#;
        let result = outline("lib/my_app/server.ex", src);
        assert_eq!(result.language, "elixir");

        let mod_names: Vec<&str> = result.modules.iter().map(|m| m.name.as_str()).collect();
        assert!(mod_names.contains(&"MyApp.Server"));

        let import_targets: Vec<&str> = result.imports.iter().map(|i| i.target.as_str()).collect();
        assert!(import_targets.contains(&"GenServer"));
        assert!(import_targets.contains(&"MyApp.Repo"));
        assert!(import_targets.contains(&"Ecto.Query"));

        // @name should be a constant, @doc and @moduledoc should not
        let const_names: Vec<&str> = result.constants.iter().map(|c| c.name.as_str()).collect();
        assert!(const_names.contains(&"@name"));
        assert!(!const_names.contains(&"@doc"));
        assert!(!const_names.contains(&"@moduledoc"));

        let pub_fn_names: Vec<&str> = result
            .public_functions
            .iter()
            .map(|f| f.name.as_str())
            .collect();
        assert!(pub_fn_names.contains(&"start_link"));
        assert!(pub_fn_names.contains(&"handle_call"));

        let priv_fn_names: Vec<&str> = result
            .private_functions
            .iter()
            .map(|f| f.name.as_str())
            .collect();
        assert!(priv_fn_names.contains(&"init_state"));
    }

    #[test]
    fn elixir_extracts_type_spec_callback() {
        let src = r#"
defmodule MyApp.Behaviour do
  @type state :: map()
  @typep internal :: atom()
  @opaque secret :: binary()

  @callback init(opts :: keyword()) :: {:ok, state()}
  @callback handle(event :: term(), state()) :: state()

  @spec process(state()) :: :ok
  def process(state) do
    :ok
  end
end
"#;
        let result = outline("lib/my_app/behaviour.ex", src);

        let type_entries: Vec<(&str, &str)> = result
            .types
            .iter()
            .map(|t| (t.name.as_str(), t.kind.as_str()))
            .collect();
        assert!(type_entries.contains(&("state", "@type")));
        assert!(type_entries.contains(&("internal", "@typep")));
        assert!(type_entries.contains(&("secret", "@opaque")));
        assert!(type_entries.contains(&("init", "@callback")));
        assert!(type_entries.contains(&("handle", "@callback")));
        assert!(type_entries.contains(&("process", "@spec")));

        // @type/@spec/@callback should NOT appear as constants
        let const_names: Vec<&str> = result.constants.iter().map(|c| c.name.as_str()).collect();
        assert!(!const_names.contains(&"@type"));
        assert!(!const_names.contains(&"@callback"));
        assert!(!const_names.contains(&"@spec"));
    }

    #[test]
    fn python_extracts_class_def_import_const() {
        let src = r#"
import os
import sys
from pathlib import Path

MAX_RETRIES = 3
DEFAULT_TIMEOUT = 30

class MyService:
    def process(self, data):
        pass

    def _validate(self, data):
        pass

def public_helper(x):
    return x

def _private_impl(x):
    return x * 2

class TestMyService:
    def test_process(self):
        pass
"#;
        let result = outline("service.py", src);
        assert_eq!(result.language, "python");

        let import_targets: Vec<&str> = result.imports.iter().map(|i| i.target.as_str()).collect();
        assert!(import_targets.iter().any(|t| t.contains("os")));
        assert!(import_targets.iter().any(|t| t.contains("sys")));
        assert!(import_targets.iter().any(|t| t.contains("Path")));

        let const_names: Vec<&str> = result.constants.iter().map(|c| c.name.as_str()).collect();
        assert!(const_names.contains(&"MAX_RETRIES"));
        assert!(const_names.contains(&"DEFAULT_TIMEOUT"));

        let mod_names: Vec<&str> = result.modules.iter().map(|m| m.name.as_str()).collect();
        assert!(mod_names.contains(&"MyService"));

        let pub_fn_names: Vec<&str> = result
            .public_functions
            .iter()
            .map(|f| f.name.as_str())
            .collect();
        assert!(pub_fn_names.contains(&"process"));
        assert!(pub_fn_names.contains(&"public_helper"));

        let priv_fn_names: Vec<&str> = result
            .private_functions
            .iter()
            .map(|f| f.name.as_str())
            .collect();
        assert!(priv_fn_names.contains(&"_validate"));
        assert!(priv_fn_names.contains(&"_private_impl"));
    }

    #[test]
    fn go_extracts_func_type_import() {
        let src = r#"
package main

import (
    "fmt"
    "os"
)

import "net/http"

type Server struct {
    port int
}

type Handler interface {
    ServeHTTP(w http.ResponseWriter, r *http.Request)
}

const MaxConn = 100
var DefaultTimeout = 30

func NewServer(port int) *Server {
    return &Server{port: port}
}

func (s *Server) Start() error {
    return nil
}

func helper(x int) int {
    return x
}

func TestSomething(t *testing.T) {
    // test body
}
"#;
        let result = outline("main.go", src);
        assert_eq!(result.language, "go");

        let mod_names: Vec<&str> = result.modules.iter().map(|m| m.name.as_str()).collect();
        assert!(mod_names.contains(&"main"));

        let import_targets: Vec<&str> = result.imports.iter().map(|i| i.target.as_str()).collect();
        assert!(import_targets.contains(&"fmt"));
        assert!(import_targets.contains(&"os"));
        assert!(import_targets.contains(&"net/http"));

        let type_names: Vec<&str> = result.types.iter().map(|t| t.name.as_str()).collect();
        assert!(type_names.contains(&"Server"));
        assert!(type_names.contains(&"Handler"));

        let const_names: Vec<&str> = result.constants.iter().map(|c| c.name.as_str()).collect();
        assert!(const_names.contains(&"MaxConn"));

        let pub_fn_names: Vec<&str> = result
            .public_functions
            .iter()
            .map(|f| f.name.as_str())
            .collect();
        assert!(pub_fn_names.contains(&"NewServer"));
        assert!(pub_fn_names.contains(&"Start"));

        let priv_fn_names: Vec<&str> = result
            .private_functions
            .iter()
            .map(|f| f.name.as_str())
            .collect();
        assert!(priv_fn_names.contains(&"helper"));

        // Test functions should be skipped
        assert!(!pub_fn_names.contains(&"TestSomething"));
    }

    #[test]
    fn typescript_extracts_export_function_interface_import() {
        let src = r#"
import { useState } from 'react';
import type { FC } from 'react';

export interface User {
    id: number;
    name: string;
}

export type UserId = number;

export class UserService {
    getName(): string { return ""; }
}

export function fetchUser(id: number): Promise<User> {
    return Promise.resolve({ id, name: "" });
}

export async function createUser(name: string): Promise<User> {
    return { id: 1, name };
}

function internalHelper(x: number): number {
    return x;
}

export const MAX_USERS = 1000;
const localConst = 42;
"#;
        let result = outline("user.ts", src);
        assert_eq!(result.language, "typescript");

        let import_targets: Vec<&str> = result.imports.iter().map(|i| i.target.as_str()).collect();
        assert!(import_targets.contains(&"react"));

        let type_names: Vec<&str> = result.types.iter().map(|t| t.name.as_str()).collect();
        assert!(type_names.contains(&"User"));
        assert!(type_names.contains(&"UserId"));

        let pub_fn_names: Vec<&str> = result
            .public_functions
            .iter()
            .map(|f| f.name.as_str())
            .collect();
        assert!(pub_fn_names.contains(&"fetchUser"));
        assert!(pub_fn_names.contains(&"createUser"));

        let priv_fn_names: Vec<&str> = result
            .private_functions
            .iter()
            .map(|f| f.name.as_str())
            .collect();
        assert!(priv_fn_names.contains(&"internalHelper"));

        let const_names: Vec<&str> = result.constants.iter().map(|c| c.name.as_str()).collect();
        assert!(const_names.contains(&"MAX_USERS"));
    }

    #[test]
    fn language_detection_covers_all_extensions() {
        assert_eq!(detect_language("foo.rs"), "rust");
        assert_eq!(detect_language("foo.py"), "python");
        assert_eq!(detect_language("foo.go"), "go");
        assert_eq!(detect_language("foo.ts"), "typescript");
        assert_eq!(detect_language("foo.tsx"), "typescript");
        assert_eq!(detect_language("foo.js"), "typescript");
        assert_eq!(detect_language("foo.jsx"), "typescript");
        assert_eq!(detect_language("foo.ex"), "elixir");
        assert_eq!(detect_language("foo.exs"), "elixir");
        assert_eq!(detect_language("foo.java"), "java");
        assert_eq!(detect_language("foo.cpp"), "cpp");
        assert_eq!(detect_language("foo.cc"), "cpp");
        assert_eq!(detect_language("foo.h"), "cpp");
        assert_eq!(detect_language("foo.hpp"), "cpp");
        assert_eq!(detect_language("foo.cs"), "csharp");
        assert_eq!(detect_language("foo.unknown"), "unknown");
    }

    #[test]
    fn csharp_extracts_namespace_class_method_using() {
        let src = r#"
using System;
using System.Collections.Generic;

namespace MyApp.Services
{
    public class UserService
    {
        public const int MaxUsers = 100;

        public async Task<User> GetUserAsync(int id)
        {
            return await _repo.FindAsync(id);
        }

        private void ValidateInput(string input)
        {
            if (string.IsNullOrEmpty(input))
                throw new ArgumentException();
        }

        public string Name { get; set; }
    }

    public interface IUserService
    {
        Task<User> GetUserAsync(int id);
    }

    public record UserDto(string Name, int Age);

    internal enum Status { Active, Inactive }
}
"#;
        let result = outline("Services/UserService.cs", src);
        assert_eq!(result.language, "csharp");

        let mod_names: Vec<&str> = result.modules.iter().map(|m| m.name.as_str()).collect();
        assert!(mod_names.contains(&"MyApp.Services"));
        assert!(mod_names.contains(&"UserService"));
        assert!(mod_names.contains(&"IUserService"));
        assert!(mod_names.contains(&"UserDto"));

        let import_targets: Vec<&str> = result.imports.iter().map(|i| i.target.as_str()).collect();
        assert!(import_targets.contains(&"System"));
        assert!(import_targets.contains(&"System.Collections.Generic"));

        let type_entries: Vec<(&str, &str)> = result
            .types
            .iter()
            .map(|t| (t.name.as_str(), t.kind.as_str()))
            .collect();
        assert!(type_entries.contains(&("UserService", "class")));
        assert!(type_entries.contains(&("IUserService", "interface")));
        assert!(type_entries.contains(&("UserDto", "record")));
        assert!(type_entries.contains(&("Status", "enum")));

        let pub_fn_names: Vec<&str> = result
            .public_functions
            .iter()
            .map(|f| f.name.as_str())
            .collect();
        assert!(pub_fn_names.contains(&"GetUserAsync"));

        let priv_fn_names: Vec<&str> = result
            .private_functions
            .iter()
            .map(|f| f.name.as_str())
            .collect();
        assert!(priv_fn_names.contains(&"ValidateInput"));

        let const_names: Vec<&str> = result.constants.iter().map(|c| c.name.as_str()).collect();
        assert!(const_names.contains(&"MaxUsers"));
    }

    #[test]
    fn outline_serializes_to_json() {
        let src = "pub fn add(a: i32, b: i32) -> i32 { a + b }\n";
        let result = outline("math.rs", src);
        let json = serde_json::to_string(&result).unwrap();
        assert!(json.contains("\"add\""));
        assert!(json.contains("\"rust\""));
    }
}
