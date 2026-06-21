use crate::extract::*;
use anyhow::Result;
use std::collections::HashSet;
use std::path::Path;

/// Built-in Rust types and common trait names that should not be treated
/// as user-defined type references.
fn builtin_types() -> HashSet<&'static str> {
    [
        // Primitives / lang items
        "Self",
        // Standard library types
        "Option",
        "Result",
        "Vec",
        "String",
        "Box",
        "Arc",
        "Rc",
        "HashMap",
        "HashSet",
        "BTreeMap",
        "BTreeSet",
        "PhantomData",
        "Pin",
        "Cow",
        "Cell",
        "RefCell",
        "Mutex",
        "RwLock",
        // Auto-traits & common std traits
        "Send",
        "Sync",
        "Copy",
        "Clone",
        "Debug",
        "Default",
        "Display",
        "Drop",
        "Eq",
        "Hash",
        "Ord",
        "PartialEq",
        "PartialOrd",
        // Conversion traits
        "From",
        "Into",
        "Iterator",
        "IntoIterator",
        // Serde
        "Serialize",
        "Deserialize",
        // Error
        "Error",
    ]
    .into_iter()
    .collect()
}

pub struct RustExtractor;

impl Extractor for RustExtractor {
    fn name(&self) -> &str {
        "rust"
    }
    fn language(&self) -> Language {
        Language::Rust
    }

    fn detect(&self, dir: &Path) -> bool {
        dir.join("Cargo.toml").exists()
    }

    fn extract(&self, dir: &Path, config: &ExtractConfig) -> Result<Vec<Edge>> {
        match config.level {
            GranularityLevel::Summary => extract_crate_level(dir, config),
            GranularityLevel::Full => extract_module_level(dir, config),
        }
    }

    fn detect_cross_language(&self, dir: &Path, config: &ExtractConfig) -> Result<Vec<Edge>> {
        if !config.detect_cross_language {
            return Ok(vec![]);
        }
        let mut edges = Vec::new();
        scan_rust_ffi(dir, &mut edges)?;
        scan_pyo3(dir, &mut edges)?;
        scan_rustler(dir, &mut edges)?;
        scan_wasm_bindgen(dir, &mut edges)?;
        Ok(edges)
    }
}

/// Extract crate-level dependencies from `cargo metadata`.
fn extract_crate_level(dir: &Path, config: &ExtractConfig) -> Result<Vec<Edge>> {
    let output = std::process::Command::new("cargo")
        .args(["metadata", "--format-version", "1", "--no-deps"])
        .current_dir(dir)
        .output();

    match output {
        Ok(out) if out.status.success() => {
            let json: serde_json::Value = serde_json::from_slice(&out.stdout)?;
            parse_cargo_metadata(&json, config)
        }
        Ok(out) => {
            let stderr = String::from_utf8_lossy(&out.stderr);
            anyhow::bail!("cargo metadata failed: {}", stderr)
        }
        Err(e) => anyhow::bail!("Failed to run cargo: {}", e),
    }
}

fn parse_cargo_metadata(json: &serde_json::Value, config: &ExtractConfig) -> Result<Vec<Edge>> {
    let mut edges = Vec::new();
    let workspace_members: Vec<String> = json
        .get("workspace_members")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();

    let packages = json.get("packages").and_then(|v| v.as_array());
    if let Some(pkgs) = packages {
        for pkg in pkgs {
            let name = pkg.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let _id = pkg.get("id").and_then(|v| v.as_str()).unwrap_or("");

            // Only include workspace members
            if !workspace_members.iter().any(|m| m.contains(name)) {
                continue;
            }

            if let Some(deps) = pkg.get("dependencies").and_then(|v| v.as_array()) {
                for dep in deps {
                    let dep_name = dep.get("name").and_then(|v| v.as_str()).unwrap_or("");
                    if let Some(prefix) = &config.prefix_filter {
                        if !dep_name.starts_with(prefix) {
                            continue;
                        }
                    }
                    // Only include deps that are also workspace members
                    if workspace_members.iter().any(|m| m.contains(dep_name)) {
                        edges.push(Edge {
                            source: name.to_string(),
                            target: dep_name.to_string(),
                            weight: 1.0,
                            kind: EdgeKind::Import,
                            cross_language: None,
                        });
                    }
                }
            }
        }
    }

    Ok(edges)
}

/// Extract module-level dependencies by parsing `use` statements.
fn extract_module_level(dir: &Path, config: &ExtractConfig) -> Result<Vec<Edge>> {
    let use_re = regex::Regex::new(r"(?m)^\s*use\s+(crate|super|self)?::?([\w:]+)")?;
    let _mod_re = regex::Regex::new(r"(?m)^\s*(?:pub\s+)?mod\s+(\w+)")?;

    let mut edges = Vec::new();

    for entry in walkdir_rs(dir) {
        let content = std::fs::read_to_string(&entry)?;
        let source_module = path_to_module(&entry, dir);

        for cap in use_re.captures_iter(&content) {
            let prefix = cap.get(1).map(|m| m.as_str()).unwrap_or("");
            let path = &cap[2];

            let target = if prefix == "crate" {
                format!("crate::{}", path)
            } else if prefix == "super" {
                let parent = source_module
                    .rsplit_once("::")
                    .map(|(p, _)| p)
                    .unwrap_or("crate");
                format!("{}::{}", parent, path)
            } else {
                // External crate
                path.split("::").next().unwrap_or(path).to_string()
            };

            if let Some(pf) = &config.prefix_filter {
                if !target.starts_with(pf) {
                    continue;
                }
            }

            if target != source_module {
                edges.push(Edge {
                    source: source_module.clone(),
                    target,
                    weight: 1.0,
                    kind: EdgeKind::Import,
                    cross_language: None,
                });
            }
        }
    }

    Ok(edges)
}

fn path_to_module(file: &Path, root: &Path) -> String {
    let relative = file.strip_prefix(root).unwrap_or(file);
    let mut parts: Vec<&str> = relative
        .components()
        .filter_map(|c| c.as_os_str().to_str())
        .collect();
    // Remove "src" prefix if present
    if parts.first() == Some(&"src") {
        parts.remove(0);
    }
    // Remove file extension
    if let Some(last) = parts.last_mut() {
        if let Some(name) = last.strip_suffix(".rs") {
            *last = name;
        }
    }
    // Remove "mod" and "lib" as they represent the parent
    if parts.last() == Some(&"mod") || parts.last() == Some(&"lib") {
        parts.pop();
    }

    if parts.is_empty() {
        "crate".to_string()
    } else {
        format!("crate::{}", parts.join("::"))
    }
}

fn scan_rust_ffi(dir: &Path, edges: &mut Vec<Edge>) -> Result<()> {
    let extern_re = regex::Regex::new(r#"extern\s+"C""#)?;
    let link_re = regex::Regex::new(r#"#\[link\(name\s*=\s*"([^"]+)""#)?;

    for entry in walkdir_rs(dir) {
        let content = std::fs::read_to_string(&entry)?;
        if extern_re.is_match(&content) {
            let source = path_to_module(&entry, dir);
            let lib = link_re
                .captures(&content)
                .map(|c| c[1].to_string())
                .unwrap_or_else(|| "c_lib".to_string());

            edges.push(Edge {
                source,
                target: format!("native:{}", lib),
                weight: 1.0,
                kind: EdgeKind::Ffi,
                cross_language: Some(CrossLanguageEdge {
                    source_lang: Language::Rust,
                    target_lang: Language::C,
                    mechanism: FfiMechanism::RustFfi,
                }),
            });
        }
    }
    Ok(())
}

fn scan_pyo3(dir: &Path, edges: &mut Vec<Edge>) -> Result<()> {
    let cargo_toml = dir.join("Cargo.toml");
    if cargo_toml.exists() {
        let content = std::fs::read_to_string(&cargo_toml)?;
        if content.contains("pyo3") {
            edges.push(Edge {
                source: "crate".to_string(),
                target: "python:module".to_string(),
                weight: 1.0,
                kind: EdgeKind::Ffi,
                cross_language: Some(CrossLanguageEdge {
                    source_lang: Language::Rust,
                    target_lang: Language::Python,
                    mechanism: FfiMechanism::PyO3,
                }),
            });
        }
    }
    Ok(())
}

fn scan_rustler(dir: &Path, edges: &mut Vec<Edge>) -> Result<()> {
    let cargo_toml = dir.join("Cargo.toml");
    if cargo_toml.exists() {
        let content = std::fs::read_to_string(&cargo_toml)?;
        if content.contains("rustler") {
            edges.push(Edge {
                source: "crate".to_string(),
                target: "elixir:nif".to_string(),
                weight: 1.0,
                kind: EdgeKind::Ffi,
                cross_language: Some(CrossLanguageEdge {
                    source_lang: Language::Rust,
                    target_lang: Language::Elixir,
                    mechanism: FfiMechanism::Nif,
                }),
            });
        }
    }
    Ok(())
}

fn scan_wasm_bindgen(dir: &Path, edges: &mut Vec<Edge>) -> Result<()> {
    let cargo_toml = dir.join("Cargo.toml");
    if cargo_toml.exists() {
        let content = std::fs::read_to_string(&cargo_toml)?;
        if content.contains("wasm-bindgen") || content.contains("wasm-pack") {
            edges.push(Edge {
                source: "crate".to_string(),
                target: "wasm:module".to_string(),
                weight: 1.0,
                kind: EdgeKind::Ffi,
                cross_language: Some(CrossLanguageEdge {
                    source_lang: Language::Rust,
                    target_lang: Language::Unknown("wasm".to_string()),
                    mechanism: FfiMechanism::Wasm,
                }),
            });
        }
    }
    Ok(())
}

fn walkdir_rs(dir: &Path) -> Vec<std::path::PathBuf> {
    super::java::walkdir(dir, "rs")
}

// ---------------------------------------------------------------------------
// DeclarationExtractor implementation
// ---------------------------------------------------------------------------

impl DeclarationExtractor for RustExtractor {
    fn language(&self) -> Language {
        Language::Rust
    }

    fn extract_declarations(&self, dir: &Path, config: &ExtractConfig) -> Result<Vec<Declaration>> {
        let fn_re = regex::Regex::new(
            r"(?m)^[ \t]*(pub(?:\(crate\)|\(super\))?\s+)?(?:async\s+)?fn\s+(\w+)",
        )?;
        let type_re = regex::Regex::new(
            r"(?m)^[ \t]*(pub(?:\(crate\)|\(super\))?\s+)?(?:struct|enum|union|type)\s+(\w+)",
        )?;
        let trait_re =
            regex::Regex::new(r"(?m)^[ \t]*(pub(?:\(crate\)|\(super\))?\s+)?trait\s+(\w+)")?;
        let const_re = regex::Regex::new(
            r"(?m)^[ \t]*(pub(?:\(crate\)|\(super\))?\s+)?(?:const|static)\s+(\w+)",
        )?;
        let mod_re = regex::Regex::new(r"(?m)^[ \t]*(pub(?:\(crate\)|\(super\))?\s+)?mod\s+(\w+)")?;

        // Entry-point attribute patterns (checked on the line above a declaration)
        let attr_test_re = regex::Regex::new(r"#\[(tokio::)?test")?;
        let attr_main_re = regex::Regex::new(r"#\[(tokio|actix_web)::main\]")?;
        let attr_no_mangle_re = regex::Regex::new(r"#\[no_mangle\]")?;
        let attr_wasm_re = regex::Regex::new(r"#\[wasm_bindgen")?;

        let mut declarations = Vec::new();

        for entry in walkdir_rs(dir) {
            let rel = entry
                .strip_prefix(dir)
                .unwrap_or(&entry)
                .to_string_lossy()
                .to_string();

            // Skip target/ directory
            if rel.starts_with("target/") || rel.starts_with("target\\") {
                continue;
            }

            // Apply prefix filter on file path
            if let Some(pf) = &config.prefix_filter {
                let module = path_to_module(&entry, dir);
                if !module.starts_with(pf) {
                    continue;
                }
            }

            let content = std::fs::read_to_string(&entry)?;
            let lines: Vec<&str> = content.lines().collect();
            let module_prefix = path_to_module(&entry, dir);

            // Helper: check if a given line index has an entry-point attribute above it
            let check_entry_point = |line_idx: usize, name: &str| -> (bool, Option<String>) {
                // Check the line itself and the line above
                let check_lines: Vec<usize> = if line_idx > 0 {
                    vec![line_idx.saturating_sub(1), line_idx]
                } else {
                    vec![line_idx]
                };

                // Also check up to 3 lines above for stacked attributes
                let start = line_idx.saturating_sub(3);
                let attr_lines: Vec<usize> = (start..=line_idx).collect();

                for &li in &attr_lines {
                    if li < lines.len() {
                        let l = lines[li];
                        if attr_test_re.is_match(l) {
                            return (true, Some("test function".to_string()));
                        }
                        if attr_main_re.is_match(l) {
                            return (true, Some("async main".to_string()));
                        }
                        if attr_no_mangle_re.is_match(l) {
                            return (true, Some("FFI export".to_string()));
                        }
                        if attr_wasm_re.is_match(l) {
                            return (true, Some("WASM export".to_string()));
                        }
                    }
                }

                // Check for main function
                if name == "main" {
                    return (true, Some("main function".to_string()));
                }
                // Check for constructor
                if name == "new" {
                    return (true, Some("constructor".to_string()));
                }

                let _ = check_lines; // suppress unused warning
                (false, None)
            };

            let parse_visibility = |vis_match: Option<regex::Match>| -> Visibility {
                match vis_match.map(|m| m.as_str().trim()) {
                    None => Visibility::Private,
                    Some(s) if s.starts_with("pub(crate)") => Visibility::PubCrate,
                    Some(s) if s.starts_with("pub(super)") => Visibility::PubSuper,
                    Some(s) if s.starts_with("pub") => Visibility::Public,
                    _ => Visibility::Private,
                }
            };

            let line_number_of =
                |byte_offset: usize| -> usize { content[..byte_offset].matches('\n').count() + 1 };

            // Functions
            for cap in fn_re.captures_iter(&content) {
                let name = cap[2].to_string();
                let vis = parse_visibility(cap.get(1));
                let line = line_number_of(cap.get(0).unwrap().start());
                let fqn = format!("{}::{}", module_prefix, name);
                let (is_ep, ep_reason) = check_entry_point(line.saturating_sub(1), &name);
                declarations.push(Declaration {
                    name: fqn,
                    kind: DeclarationKind::Function,
                    visibility: vis,
                    file: rel.clone(),
                    line,
                    language: Language::Rust,
                    is_entry_point: is_ep,
                    entry_point_reason: ep_reason,
                });
            }

            // Types (struct, enum, union, type alias)
            for cap in type_re.captures_iter(&content) {
                let name = cap[2].to_string();
                let vis = parse_visibility(cap.get(1));
                let line = line_number_of(cap.get(0).unwrap().start());
                let fqn = format!("{}::{}", module_prefix, name);
                declarations.push(Declaration {
                    name: fqn,
                    kind: DeclarationKind::Type,
                    visibility: vis,
                    file: rel.clone(),
                    line,
                    language: Language::Rust,
                    is_entry_point: false,
                    entry_point_reason: None,
                });
            }

            // Traits
            for cap in trait_re.captures_iter(&content) {
                let name = cap[2].to_string();
                let vis = parse_visibility(cap.get(1));
                let line = line_number_of(cap.get(0).unwrap().start());
                let fqn = format!("{}::{}", module_prefix, name);
                declarations.push(Declaration {
                    name: fqn,
                    kind: DeclarationKind::Trait,
                    visibility: vis,
                    file: rel.clone(),
                    line,
                    language: Language::Rust,
                    is_entry_point: false,
                    entry_point_reason: None,
                });
            }

            // Constants / statics
            for cap in const_re.captures_iter(&content) {
                let name = cap[2].to_string();
                let vis = parse_visibility(cap.get(1));
                let line = line_number_of(cap.get(0).unwrap().start());
                let fqn = format!("{}::{}", module_prefix, name);
                declarations.push(Declaration {
                    name: fqn,
                    kind: DeclarationKind::Constant,
                    visibility: vis,
                    file: rel.clone(),
                    line,
                    language: Language::Rust,
                    is_entry_point: false,
                    entry_point_reason: None,
                });
            }

            // Modules
            for cap in mod_re.captures_iter(&content) {
                let name = cap[2].to_string();
                let vis = parse_visibility(cap.get(1));
                let line = line_number_of(cap.get(0).unwrap().start());
                let fqn = format!("{}::{}", module_prefix, name);
                declarations.push(Declaration {
                    name: fqn,
                    kind: DeclarationKind::Module,
                    visibility: vis,
                    file: rel.clone(),
                    line,
                    language: Language::Rust,
                    is_entry_point: false,
                    entry_point_reason: None,
                });
            }
        }

        Ok(declarations)
    }

    fn extract_references(
        &self,
        dir: &Path,
        config: &ExtractConfig,
    ) -> Result<Vec<SymbolReference>> {
        let builtins = builtin_types();

        // Existing patterns
        let qualified_re = regex::Regex::new(r"(\w+)::(\w+)")?;
        let fn_call_re = regex::Regex::new(r"\b(\w+)\(")?;
        let method_call_re = regex::Regex::new(r"\.(\w+)\(")?;

        // New type-level reference patterns
        // 1. Type annotations: `: TypeName`, `-> TypeName`, `: &TypeName`
        let type_annot_re = regex::Regex::new(r":\s*(&?\s*)?([A-Z]\w+)")?;
        // 2. Struct construction: `TypeName {` (exclude keywords)
        let struct_construct_re = regex::Regex::new(r"([A-Z]\w+)\s*\{")?;
        // 3. Generic parameters: `<TypeName>`, `<TypeName,`, `, TypeName>`
        let generic_re = regex::Regex::new(r"[<,]\s*(&?\s*)?([A-Z]\w+)")?;
        // 4. impl blocks: `impl TypeName`, `impl Trait for TypeName`
        let impl_re = regex::Regex::new(r"\bimpl\s+(?:\w+\s+for\s+)?([A-Z]\w+)")?;
        // 5. Derive macros: `#[derive(Trait1, Trait2)]`
        let derive_re = regex::Regex::new(r"#\[derive\(([^)]+)\)\]")?;

        // Keywords that look like struct construction but aren't
        let struct_keywords: HashSet<&str> =
            ["If", "Else", "Match", "Loop", "Fn", "Mod", "Use", "Where"]
                .into_iter()
                .collect();

        let mut references = Vec::new();

        for entry in walkdir_rs(dir) {
            let rel = entry
                .strip_prefix(dir)
                .unwrap_or(&entry)
                .to_string_lossy()
                .to_string();

            // Skip target/ directory
            if rel.starts_with("target/") || rel.starts_with("target\\") {
                continue;
            }

            if let Some(pf) = &config.prefix_filter {
                let module = path_to_module(&entry, dir);
                if !module.starts_with(pf) {
                    continue;
                }
            }

            let content = std::fs::read_to_string(&entry)?;

            for (line_idx, line) in content.lines().enumerate() {
                let line_num = line_idx + 1;

                // Qualified path references: foo::bar
                for cap in qualified_re.captures_iter(line) {
                    let full = format!("{}::{}", &cap[1], &cap[2]);
                    references.push(SymbolReference {
                        from_file: rel.clone(),
                        to_symbol: full,
                        line: line_num,
                    });
                    // Also record just the final segment
                    references.push(SymbolReference {
                        from_file: rel.clone(),
                        to_symbol: cap[2].to_string(),
                        line: line_num,
                    });
                    // Record the left side if it starts with uppercase (type reference)
                    let left = &cap[1];
                    if left.starts_with(|c: char| c.is_uppercase()) && !builtins.contains(left) {
                        references.push(SymbolReference {
                            from_file: rel.clone(),
                            to_symbol: left.to_string(),
                            line: line_num,
                        });
                    }
                }

                // Function calls: foo(
                for cap in fn_call_re.captures_iter(line) {
                    let name = &cap[1];
                    // Skip common keywords that look like function calls
                    if matches!(
                        name,
                        "if" | "match"
                            | "while"
                            | "for"
                            | "return"
                            | "let"
                            | "use"
                            | "mod"
                            | "pub"
                            | "fn"
                            | "impl"
                            | "struct"
                            | "enum"
                            | "trait"
                            | "type"
                            | "where"
                            | "Some"
                            | "None"
                            | "Ok"
                            | "Err"
                            | "vec"
                            | "format"
                            | "println"
                            | "eprintln"
                            | "write"
                            | "writeln"
                            | "assert"
                            | "assert_eq"
                            | "assert_ne"
                            | "cfg"
                            | "derive"
                            | "include"
                    ) {
                        continue;
                    }
                    references.push(SymbolReference {
                        from_file: rel.clone(),
                        to_symbol: name.to_string(),
                        line: line_num,
                    });
                }

                // Method calls: .foo(
                for cap in method_call_re.captures_iter(line) {
                    references.push(SymbolReference {
                        from_file: rel.clone(),
                        to_symbol: cap[1].to_string(),
                        line: line_num,
                    });
                }

                // --- New type-level reference patterns ---

                // Type annotations: `: TypeName`
                for cap in type_annot_re.captures_iter(line) {
                    let name = &cap[2];
                    if !builtins.contains(name) {
                        references.push(SymbolReference {
                            from_file: rel.clone(),
                            to_symbol: name.to_string(),
                            line: line_num,
                        });
                    }
                }

                // Struct construction: `TypeName {`
                for cap in struct_construct_re.captures_iter(line) {
                    let name = &cap[1];
                    if !builtins.contains(name) && !struct_keywords.contains(name) {
                        references.push(SymbolReference {
                            from_file: rel.clone(),
                            to_symbol: name.to_string(),
                            line: line_num,
                        });
                    }
                }

                // Generic parameters: `<TypeName>`
                for cap in generic_re.captures_iter(line) {
                    let name = &cap[2];
                    if !builtins.contains(name) {
                        references.push(SymbolReference {
                            from_file: rel.clone(),
                            to_symbol: name.to_string(),
                            line: line_num,
                        });
                    }
                }

                // impl blocks: `impl TypeName`
                for cap in impl_re.captures_iter(line) {
                    let name = &cap[1];
                    if !builtins.contains(name) {
                        references.push(SymbolReference {
                            from_file: rel.clone(),
                            to_symbol: name.to_string(),
                            line: line_num,
                        });
                    }
                }

                // Derive macros: `#[derive(Trait1, Trait2)]`
                for cap in derive_re.captures_iter(line) {
                    let traits_str = &cap[1];
                    for trait_name in traits_str.split(',') {
                        let t = trait_name.trim();
                        if !t.is_empty() && !builtins.contains(t) {
                            references.push(SymbolReference {
                                from_file: rel.clone(),
                                to_symbol: t.to_string(),
                                line: line_num,
                            });
                        }
                    }
                }
            }
        }

        Ok(references)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_rust_project() {
        let ext = RustExtractor;
        // Our own crate has Cargo.toml
        assert!(ext.detect(Path::new(".")));
    }

    #[test]
    fn path_to_module_conversion() {
        assert_eq!(
            path_to_module(Path::new("src/lib.rs"), Path::new(".")),
            "crate"
        );
        assert_eq!(
            path_to_module(Path::new("src/extract/mod.rs"), Path::new(".")),
            "crate::extract"
        );
        assert_eq!(
            path_to_module(Path::new("src/matrix.rs"), Path::new(".")),
            "crate::matrix"
        );
    }
}
