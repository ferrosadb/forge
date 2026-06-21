use crate::extract::*;
use anyhow::Result;
use std::path::Path;

pub struct TypeScriptExtractor;

impl Extractor for TypeScriptExtractor {
    fn name(&self) -> &str {
        "typescript"
    }
    fn language(&self) -> Language {
        Language::TypeScript
    }

    fn detect(&self, dir: &Path) -> bool {
        dir.join("tsconfig.json").exists() || dir.join("package.json").exists()
    }

    fn extract(&self, dir: &Path, config: &ExtractConfig) -> Result<Vec<Edge>> {
        let import_re =
            regex::Regex::new(r#"(?m)(?:import|export)\s+(?:.*?\s+from\s+)?['"]([^'"]+)['"]"#)?;
        let require_re = regex::Regex::new(r#"require\s*\(\s*['"]([^'"]+)['"]\s*\)"#)?;

        let mut edges = Vec::new();
        let extensions = ["ts", "tsx", "js", "jsx"];

        for ext in &extensions {
            for entry in super::java::walkdir(dir, ext) {
                // Skip node_modules
                if entry.to_string_lossy().contains("node_modules") {
                    continue;
                }

                let content = std::fs::read_to_string(&entry)?;
                let source = ts_module_from_path(&entry, dir, config);

                for cap in import_re.captures_iter(&content) {
                    let raw = &cap[1];
                    if let Some(target) = resolve_ts_import(raw, &entry, dir, config) {
                        if target != source {
                            edges.push(Edge {
                                source: source.clone(),
                                target,
                                weight: 1.0,
                                kind: EdgeKind::Import,
                                cross_language: None,
                            });
                        }
                    }
                }

                for cap in require_re.captures_iter(&content) {
                    let raw = &cap[1];
                    if let Some(target) = resolve_ts_import(raw, &entry, dir, config) {
                        if target != source {
                            edges.push(Edge {
                                source: source.clone(),
                                target,
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

    fn detect_cross_language(&self, dir: &Path, config: &ExtractConfig) -> Result<Vec<Edge>> {
        if !config.detect_cross_language {
            return Ok(vec![]);
        }
        let mut edges = Vec::new();
        scan_native_addons(dir, &mut edges)?;
        scan_wasm_ts(dir, &mut edges)?;
        scan_grpc_ts(dir, &mut edges)?;
        Ok(edges)
    }
}

// ---------------------------------------------------------------------------
// DeclarationExtractor implementation
// ---------------------------------------------------------------------------

impl DeclarationExtractor for TypeScriptExtractor {
    fn language(&self) -> Language {
        Language::TypeScript
    }

    fn extract_declarations(&self, dir: &Path, config: &ExtractConfig) -> Result<Vec<Declaration>> {
        let fn_re = regex::Regex::new(r#"(?m)^[ \t]*(export\s+)?(?:async\s+)?function\s+(\w+)"#)?;
        let class_re = regex::Regex::new(r#"(?m)^[ \t]*(export\s+)?class\s+(\w+)"#)?;
        let type_re = regex::Regex::new(r#"(?m)^[ \t]*(export\s+)?type\s+(\w+)"#)?;
        let interface_re = regex::Regex::new(r#"(?m)^[ \t]*(export\s+)?interface\s+(\w+)"#)?;
        let const_re = regex::Regex::new(r#"(?m)^[ \t]*(export\s+)?(?:const|let|var)\s+(\w+)"#)?;
        let enum_re = regex::Regex::new(r#"(?m)^[ \t]*(export\s+)?enum\s+(\w+)"#)?;
        let mod_re = regex::Regex::new(r#"(?m)^[ \t]*(export\s+)?namespace\s+(\w+)"#)?;

        // Entry-point patterns
        let attr_test_re = regex::Regex::new(r#"@(test|Test|pytest)"#)?;
        let attr_main_re = regex::Regex::new(r#"@(main|Main|entrypoint)"#)?;
        let describe_re = regex::Regex::new(r#"describe\s*\("#)?;
        let it_re = regex::Regex::new(r#"(?m)^ *[\'\"]?(it|test|spec)\(["']"#)?;

        let mut declarations = Vec::new();

        for entry in super::java::walkdir(dir, "ts")
            .into_iter()
            .chain(super::java::walkdir(dir, "tsx"))
        {
            let rel = entry
                .strip_prefix(dir)
                .unwrap_or(&entry)
                .to_string_lossy()
                .to_string();

            // Skip node_modules and build directories
            if rel.contains("node_modules") || rel.starts_with("dist/") || rel.starts_with("build/")
            {
                continue;
            }

            // Apply prefix filter on file path
            if let Some(pf) = &config.prefix_filter {
                let module = ts_module_from_path(&entry, dir, config);
                if !module.starts_with(pf) {
                    continue;
                }
            }

            let content = std::fs::read_to_string(&entry)?;
            let lines: Vec<&str> = content.lines().collect();

            // Helper to check if a declaration has an entry-point marker above it
            let check_entry_point = |line_idx: usize, name: &str| -> (bool, Option<String>) {
                // Check up to 5 lines above for decorators/tests
                let start = line_idx.saturating_sub(5);
                for i in start..line_idx {
                    if i < lines.len() {
                        let line = lines[i];
                        if describe_re.is_match(line) || it_re.is_match(line) {
                            return (true, Some("test function".to_string()));
                        }
                        if attr_test_re.is_match(line) {
                            return (true, Some("test decorator".to_string()));
                        }
                        if attr_main_re.is_match(line) {
                            return (true, Some("entry point".to_string()));
                        }
                    }
                }

                // Check for main function or test file convention
                if name == "main" || name == "Main" {
                    return (true, Some("main function".to_string()));
                }

                // Check if this is a test file
                let file_lower = rel.to_lowercase();
                if file_lower.contains(".test.")
                    || file_lower.contains(".spec.")
                    || file_lower.ends_with(".test.ts")
                    || file_lower.ends_with(".spec.ts")
                {
                    return (true, Some("test file".to_string()));
                }

                (false, None)
            };

            let parse_visibility = |vis_match: Option<regex::Match>| -> Visibility {
                if vis_match.is_some() {
                    Visibility::Public
                } else {
                    Visibility::Internal
                }
            };

            let line_number_of =
                |byte_offset: usize| -> usize { content[..byte_offset].matches('\n').count() + 1 };

            // Functions
            for cap in fn_re.captures_iter(&content) {
                let name = cap[2].to_string();
                let vis = parse_visibility(cap.get(1));
                let line = line_number_of(cap.get(0).unwrap().start());
                let (is_ep, ep_reason) = check_entry_point(line.saturating_sub(1), &name);
                declarations.push(Declaration {
                    name,
                    kind: DeclarationKind::Function,
                    visibility: vis,
                    file: rel.clone(),
                    line,
                    language: Language::TypeScript,
                    is_entry_point: is_ep,
                    entry_point_reason: ep_reason,
                });
            }

            // Classes
            for cap in class_re.captures_iter(&content) {
                let name = cap[2].to_string();
                let vis = parse_visibility(cap.get(1));
                let line = line_number_of(cap.get(0).unwrap().start());
                declarations.push(Declaration {
                    name,
                    kind: DeclarationKind::Type,
                    visibility: vis,
                    file: rel.clone(),
                    line,
                    language: Language::TypeScript,
                    is_entry_point: false,
                    entry_point_reason: None,
                });
            }

            // Types
            for cap in type_re.captures_iter(&content) {
                let name = cap[2].to_string();
                let vis = parse_visibility(cap.get(1));
                let line = line_number_of(cap.get(0).unwrap().start());
                declarations.push(Declaration {
                    name,
                    kind: DeclarationKind::Type,
                    visibility: vis,
                    file: rel.clone(),
                    line,
                    language: Language::TypeScript,
                    is_entry_point: false,
                    entry_point_reason: None,
                });
            }

            // Interfaces
            for cap in interface_re.captures_iter(&content) {
                let name = cap[2].to_string();
                let vis = parse_visibility(cap.get(1));
                let line = line_number_of(cap.get(0).unwrap().start());
                declarations.push(Declaration {
                    name,
                    kind: DeclarationKind::Trait,
                    visibility: vis,
                    file: rel.clone(),
                    line,
                    language: Language::TypeScript,
                    is_entry_point: false,
                    entry_point_reason: None,
                });
            }

            // Constants/Variables
            for cap in const_re.captures_iter(&content) {
                let name = cap[2].to_string();
                let vis = parse_visibility(cap.get(1));
                let line = line_number_of(cap.get(0).unwrap().start());
                declarations.push(Declaration {
                    name,
                    kind: DeclarationKind::Constant,
                    visibility: vis,
                    file: rel.clone(),
                    line,
                    language: Language::TypeScript,
                    is_entry_point: false,
                    entry_point_reason: None,
                });
            }

            // Enums
            for cap in enum_re.captures_iter(&content) {
                let name = cap[2].to_string();
                let vis = parse_visibility(cap.get(1));
                let line = line_number_of(cap.get(0).unwrap().start());
                declarations.push(Declaration {
                    name,
                    kind: DeclarationKind::Type,
                    visibility: vis,
                    file: rel.clone(),
                    line,
                    language: Language::TypeScript,
                    is_entry_point: false,
                    entry_point_reason: None,
                });
            }

            // Namespaces
            for cap in mod_re.captures_iter(&content) {
                let name = cap[2].to_string();
                let vis = parse_visibility(cap.get(1));
                let line = line_number_of(cap.get(0).unwrap().start());
                declarations.push(Declaration {
                    name,
                    kind: DeclarationKind::Module,
                    visibility: vis,
                    file: rel.clone(),
                    line,
                    language: Language::TypeScript,
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
        // TypeScript keyword builtins to filter
        let builtins = [
            "any",
            "boolean",
            "number",
            "string",
            "symbol",
            "undefined",
            "null",
            "void",
            "never",
            "unknown",
            "object",
            "Function",
            "Array",
            "Promise",
            "Record",
            "Partial",
            "Readonly",
            "Pick",
            "Omit",
            "Exclude",
            "Extract",
            "NonNullable",
            "Parameters",
            "ConstructorParameters",
            "ReturnType",
            "InstanceType",
            "ThisParameterType",
            "OmitThisParameter",
        ]
        .iter()
        .cloned()
        .collect::<std::collections::HashSet<&str>>();

        let type_re = regex::Regex::new(r"\b([A-Z]\w*)\s*[<:,]")?;
        let new_re = regex::Regex::new(r"\bnew\s+([A-Z]\w*)\s*[(<]")?;
        let call_re = regex::Regex::new(
            r"([A-Z]\w*)\.(subscribe|pipe|map|filter|of|from|asObservable|getValue|setValue)",
        )?;
        let impl_re = regex::Regex::new(r"implements\s+([A-Z]\w*)")?;
        let ext_re = regex::Regex::new(r"extends\s+([A-Z]\w*)")?;
        let type_of_re = regex::Regex::new(r"typeof\s+([A-Z]\w*)")?;
        let inst_re = regex::Regex::new(r"instanceof\s+([A-Z]\w*)")?;

        let mut references = Vec::new();

        for entry in super::java::walkdir(dir, "ts")
            .into_iter()
            .chain(super::java::walkdir(dir, "tsx"))
        {
            let rel = entry
                .strip_prefix(dir)
                .unwrap_or(&entry)
                .to_string_lossy()
                .to_string();

            if rel.contains("node_modules") || rel.starts_with("dist/") || rel.starts_with("build/")
            {
                continue;
            }

            if let Some(pf) = &config.prefix_filter {
                let module = ts_module_from_path(&entry, dir, config);
                if !module.starts_with(pf) {
                    continue;
                }
            }

            let content = std::fs::read_to_string(&entry)?;

            for (line_idx, line) in content.lines().enumerate() {
                let line_num = line_idx + 1;

                // Type annotations: TypeName< or TypeName:
                for cap in type_re.captures_iter(line) {
                    let name = &cap[1];
                    if !builtins.contains(name)
                        && name.chars().next().is_some_and(|c| c.is_uppercase())
                    {
                        references.push(SymbolReference {
                            from_file: rel.clone(),
                            to_symbol: name.to_string(),
                            line: line_num,
                        });
                    }
                }

                // Constructor calls: new TypeName(
                for cap in new_re.captures_iter(line) {
                    let name = &cap[1];
                    if !builtins.contains(name)
                        && !name.chars().next().is_some_and(|c| c.is_uppercase())
                    {
                        // Allow lowercase for factory patterns
                    } else if !builtins.contains(name) {
                        references.push(SymbolReference {
                            from_file: rel.clone(),
                            to_symbol: name.to_string(),
                            line: line_num,
                        });
                    }
                }

                // Method chaining (RxJS, etc.)
                for cap in call_re.captures_iter(line) {
                    let name = &cap[1];
                    if !builtins.contains(name) {
                        references.push(SymbolReference {
                            from_file: rel.clone(),
                            to_symbol: name.to_string(),
                            line: line_num,
                        });
                    }
                }

                // implements TypeName
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

                // extends TypeName
                for cap in ext_re.captures_iter(line) {
                    let name = &cap[1];
                    if !builtins.contains(name) {
                        references.push(SymbolReference {
                            from_file: rel.clone(),
                            to_symbol: name.to_string(),
                            line: line_num,
                        });
                    }
                }

                // typeof TypeName
                for cap in type_of_re.captures_iter(line) {
                    let name = &cap[1];
                    if !builtins.contains(name)
                        && name.chars().next().is_some_and(|c| c.is_uppercase())
                    {
                        references.push(SymbolReference {
                            from_file: rel.clone(),
                            to_symbol: name.to_string(),
                            line: line_num,
                        });
                    }
                }

                // instanceof TypeName
                for cap in inst_re.captures_iter(line) {
                    let name = &cap[1];
                    if !builtins.contains(name) {
                        references.push(SymbolReference {
                            from_file: rel.clone(),
                            to_symbol: name.to_string(),
                            line: line_num,
                        });
                    }
                }
            }
        }

        Ok(references)
    }
}

fn ts_module_from_path(file: &Path, root: &Path, config: &ExtractConfig) -> String {
    let relative = file.strip_prefix(root).unwrap_or(file);
    let path_str = relative.to_string_lossy().replace('\\', "/");
    let without_ext = path_str
        .strip_suffix(".ts")
        .or_else(|| path_str.strip_suffix(".tsx"))
        .or_else(|| path_str.strip_suffix(".js"))
        .or_else(|| path_str.strip_suffix(".jsx"))
        .unwrap_or(&path_str);

    match config.level {
        GranularityLevel::Summary => {
            // Directory level
            let parts: Vec<&str> = without_ext.split('/').collect();
            if parts.len() > 1 {
                parts[..parts.len() - 1].join("/")
            } else {
                without_ext.to_string()
            }
        }
        GranularityLevel::Full => without_ext.to_string(),
    }
}

fn resolve_ts_import(
    raw: &str,
    source_file: &Path,
    root: &Path,
    config: &ExtractConfig,
) -> Option<String> {
    // Skip node_modules imports (npm packages)
    if !raw.starts_with('.') && !raw.starts_with('/') && !raw.starts_with('@') {
        // npm package — skip unless it starts with project prefix
        if let Some(prefix) = &config.prefix_filter {
            if !raw.starts_with(prefix) {
                return None;
            }
        } else {
            return None; // skip external packages by default
        }
    }

    // Relative import
    if raw.starts_with('.') {
        let source_dir = source_file.parent()?;
        let resolved = source_dir.join(raw);
        let normalized = resolved.to_string_lossy().replace('\\', "/");

        match config.level {
            GranularityLevel::Summary => {
                // Truncate to directory
                let parts: Vec<&str> = normalized.split('/').collect();
                if parts.len() > 1 {
                    Some(parts[..parts.len() - 1].join("/"))
                } else {
                    Some(normalized)
                }
            }
            GranularityLevel::Full => {
                let relative = Path::new(&normalized)
                    .strip_prefix(root)
                    .unwrap_or(Path::new(&normalized));
                Some(relative.to_string_lossy().to_string())
            }
        }
    } else if raw.starts_with('@') {
        // Scoped package or path alias
        if let Some(prefix) = &config.prefix_filter {
            if raw.starts_with(prefix) {
                return Some(raw.to_string());
            }
        }
        None
    } else {
        None
    }
}

fn scan_native_addons(dir: &Path, edges: &mut Vec<Edge>) -> Result<()> {
    let pkg_json = dir.join("package.json");
    if pkg_json.exists() {
        let content = std::fs::read_to_string(&pkg_json)?;
        if content.contains("node-addon-api")
            || content.contains("napi-rs")
            || content.contains("node-gyp")
        {
            let mechanism = FfiMechanism::NativeAddon;
            let target_lang = if content.contains("napi-rs") {
                Language::Rust
            } else {
                Language::Cpp
            };
            edges.push(Edge {
                source: "node:project".to_string(),
                target: "native:addon".to_string(),
                weight: 1.0,
                kind: EdgeKind::Ffi,
                cross_language: Some(CrossLanguageEdge {
                    source_lang: Language::TypeScript,
                    target_lang,
                    mechanism,
                }),
            });
        }
    }
    Ok(())
}

fn scan_wasm_ts(dir: &Path, edges: &mut Vec<Edge>) -> Result<()> {
    let wasm_re = regex::Regex::new(r#"WebAssembly\.instantiate|\.wasm['"]"#)?;

    for ext in &["ts", "tsx", "js", "jsx"] {
        for entry in super::java::walkdir(dir, ext) {
            if entry.to_string_lossy().contains("node_modules") {
                continue;
            }
            let content = std::fs::read_to_string(&entry)?;
            if wasm_re.is_match(&content) {
                let source = entry.to_string_lossy().to_string();
                edges.push(Edge {
                    source,
                    target: "wasm:module".to_string(),
                    weight: 1.0,
                    kind: EdgeKind::Ffi,
                    cross_language: Some(CrossLanguageEdge {
                        source_lang: Language::TypeScript,
                        target_lang: Language::Unknown("wasm".to_string()),
                        mechanism: FfiMechanism::Wasm,
                    }),
                });
                break; // One per project is enough
            }
        }
    }
    Ok(())
}

fn scan_grpc_ts(dir: &Path, edges: &mut Vec<Edge>) -> Result<()> {
    let pkg_json = dir.join("package.json");
    if pkg_json.exists() {
        let content = std::fs::read_to_string(&pkg_json)?;
        if content.contains("@grpc/grpc-js") || content.contains("grpc-tools") {
            edges.push(Edge {
                source: "node:project".to_string(),
                target: "grpc:service".to_string(),
                weight: 1.0,
                kind: EdgeKind::Ipc,
                cross_language: Some(CrossLanguageEdge {
                    source_lang: Language::TypeScript,
                    target_lang: Language::Unknown("grpc".to_string()),
                    mechanism: FfiMechanism::Grpc,
                }),
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_ts_project() {
        let ext = TypeScriptExtractor;
        assert!(!ext.detect(Path::new("/nonexistent")));
    }

    #[test]
    fn ts_module_from_path_summary() {
        let config = ExtractConfig {
            level: GranularityLevel::Summary,
            ..Default::default()
        };
        assert_eq!(
            ts_module_from_path(
                Path::new("src/components/Button.tsx"),
                Path::new("."),
                &config
            ),
            "src/components"
        );
    }

    #[test]
    fn ts_module_from_path_full() {
        let config = ExtractConfig {
            level: GranularityLevel::Full,
            ..Default::default()
        };
        assert_eq!(
            ts_module_from_path(
                Path::new("src/components/Button.tsx"),
                Path::new("."),
                &config
            ),
            "src/components/Button"
        );
    }
}
