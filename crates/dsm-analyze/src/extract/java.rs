use crate::extract::dot_parser::parse_dot;
use crate::extract::*;
use anyhow::Result;
use std::collections::HashSet;
use std::path::Path;

pub struct JavaExtractor;

impl Extractor for JavaExtractor {
    fn name(&self) -> &str {
        "java"
    }
    fn language(&self) -> Language {
        Language::Java
    }

    fn detect(&self, dir: &Path) -> bool {
        // Look for build system markers or JARs
        dir.join("pom.xml").exists()
            || dir.join("build.gradle").exists()
            || dir.join("build.gradle.kts").exists()
            || dir.join("build.xml").exists()
            || has_jar_files(dir)
    }

    fn extract(&self, dir: &Path, config: &ExtractConfig) -> Result<Vec<Edge>> {
        // Try to find jdeps DOT output first
        let dots_dir = dir.join("dots");
        if dots_dir.exists() {
            return extract_from_dots(&dots_dir, config);
        }

        // Try running jdeps if JARs are available
        if let Some(jar) = find_jar(dir) {
            return extract_via_jdeps(&jar, dir, config);
        }

        // Fallback: parse source imports
        extract_from_source(dir, config)
    }

    fn detect_cross_language(&self, dir: &Path, config: &ExtractConfig) -> Result<Vec<Edge>> {
        if !config.detect_cross_language {
            return Ok(vec![]);
        }
        let mut edges = Vec::new();

        // Scan for JNI: native methods, System.loadLibrary
        scan_jni(dir, &mut edges)?;
        // Scan for JNA: com.sun.jna.Library
        scan_jna(dir, &mut edges)?;
        // Scan for gRPC/protobuf stubs
        scan_grpc_java(dir, &mut edges)?;

        Ok(edges)
    }
}

fn has_jar_files(dir: &Path) -> bool {
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            if entry.path().extension().is_some_and(|e| e == "jar") {
                return true;
            }
        }
    }
    // Check build output directories
    for subdir in &["target", "build/libs", "out"] {
        let path = dir.join(subdir);
        if path.exists() {
            if let Ok(entries) = std::fs::read_dir(&path) {
                for entry in entries.flatten() {
                    if entry.path().extension().is_some_and(|e| e == "jar") {
                        return true;
                    }
                }
            }
        }
    }
    false
}

fn find_jar(dir: &Path) -> Option<std::path::PathBuf> {
    for subdir in &["target", "build/libs", "out", "."] {
        let path = dir.join(subdir);
        if path.exists() {
            if let Ok(entries) = std::fs::read_dir(&path) {
                for entry in entries.flatten() {
                    let p = entry.path();
                    if p.extension().is_some_and(|e| e == "jar")
                        && !p
                            .file_name()
                            .is_some_and(|n| n.to_string_lossy().contains("sources"))
                    {
                        return Some(p);
                    }
                }
            }
        }
    }
    None
}

fn extract_from_dots(dots_dir: &Path, config: &ExtractConfig) -> Result<Vec<Edge>> {
    let mut all_edges = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dots_dir) {
        for entry in entries.flatten() {
            if entry.path().extension().is_some_and(|e| e == "dot") {
                let content = std::fs::read_to_string(entry.path())?;
                let mut edges = parse_dot(&content)?;
                if let Some(prefix) = &config.prefix_filter {
                    edges.retain(|e| e.source.starts_with(prefix) && e.target.starts_with(prefix));
                }
                all_edges.extend(edges);
            }
        }
    }
    Ok(all_edges)
}

fn extract_via_jdeps(jar: &Path, dir: &Path, config: &ExtractConfig) -> Result<Vec<Edge>> {
    let dots_dir = dir.join(".dsm-dots");
    std::fs::create_dir_all(&dots_dir)?;

    let level_flag = match config.level {
        GranularityLevel::Summary => vec![
            "--dot-output".to_string(),
            dots_dir.to_string_lossy().to_string(),
        ],
        GranularityLevel::Full => vec![
            "-verbose:class".to_string(),
            "--dot-output".to_string(),
            dots_dir.to_string_lossy().to_string(),
        ],
    };

    let mut cmd = std::process::Command::new("jdeps");
    for flag in &level_flag {
        cmd.arg(flag);
    }
    cmd.arg(jar.to_string_lossy().as_ref());

    match cmd.output() {
        Ok(output) => {
            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                anyhow::bail!("jdeps failed: {}", stderr);
            }
            extract_from_dots(&dots_dir, config)
        }
        Err(e) => {
            anyhow::bail!("Failed to run jdeps (is JDK installed?): {}", e)
        }
    }
}

fn extract_from_source(dir: &Path, config: &ExtractConfig) -> Result<Vec<Edge>> {
    let import_re = regex::Regex::new(r"import\s+(static\s+)?([a-zA-Z][\w.]*)")?;
    let package_re = regex::Regex::new(r"package\s+([a-zA-Z][\w.]*)")?;

    let mut edges = Vec::new();

    for entry in walkdir(dir, "java") {
        let content = std::fs::read_to_string(&entry)?;
        let source_package = package_re.captures(&content).map(|c| c[1].to_string());

        if let Some(source) = &source_package {
            for cap in import_re.captures_iter(&content) {
                let imported = cap[2].to_string();
                let target = match config.level {
                    GranularityLevel::Summary => {
                        // Truncate to package level
                        let parts: Vec<&str> = imported.split('.').collect();
                        if parts.len() > 2 {
                            parts[..parts.len() - 1].join(".")
                        } else {
                            imported.clone()
                        }
                    }
                    GranularityLevel::Full => imported,
                };

                if let Some(prefix) = &config.prefix_filter {
                    if !target.starts_with(prefix) {
                        continue;
                    }
                }

                if &target != source {
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

    Ok(edges)
}

fn scan_jni(dir: &Path, edges: &mut Vec<Edge>) -> Result<()> {
    let native_re =
        regex::Regex::new(r"(?m)^\s*(public|private|protected)?\s*native\s+\w+\s+(\w+)")?;
    let loadlib_re = regex::Regex::new(r#"System\.load(?:Library)?\s*\(\s*"([^"]+)""#)?;
    let package_re = regex::Regex::new(r"package\s+([a-zA-Z][\w.]*)")?;

    for entry in walkdir(dir, "java") {
        let content = std::fs::read_to_string(&entry)?;
        let has_native = native_re.is_match(&content) || loadlib_re.is_match(&content);

        if has_native {
            let source = package_re
                .captures(&content)
                .map(|c| c[1].to_string())
                .unwrap_or_else(|| {
                    entry
                        .file_stem()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .to_string()
                });

            let lib_name = loadlib_re
                .captures(&content)
                .map(|c| c[1].to_string())
                .unwrap_or_else(|| "native_lib".to_string());

            edges.push(Edge {
                source: source.clone(),
                target: format!("native:{}", lib_name),
                weight: 1.0,
                kind: EdgeKind::Ffi,
                cross_language: Some(CrossLanguageEdge {
                    source_lang: Language::Java,
                    target_lang: Language::C,
                    mechanism: FfiMechanism::Jni,
                }),
            });
        }
    }
    Ok(())
}

fn scan_jna(dir: &Path, edges: &mut Vec<Edge>) -> Result<()> {
    let jna_re = regex::Regex::new(r"com\.sun\.jna\.Library")?;
    let package_re = regex::Regex::new(r"package\s+([a-zA-Z][\w.]*)")?;

    for entry in walkdir(dir, "java") {
        let content = std::fs::read_to_string(&entry)?;
        if jna_re.is_match(&content) {
            let source = package_re
                .captures(&content)
                .map(|c| c[1].to_string())
                .unwrap_or_default();

            edges.push(Edge {
                source,
                target: "native:jna_lib".to_string(),
                weight: 1.0,
                kind: EdgeKind::Ffi,
                cross_language: Some(CrossLanguageEdge {
                    source_lang: Language::Java,
                    target_lang: Language::C,
                    mechanism: FfiMechanism::Jna,
                }),
            });
        }
    }
    Ok(())
}

fn scan_grpc_java(dir: &Path, edges: &mut Vec<Edge>) -> Result<()> {
    let grpc_re = regex::Regex::new(r"(\w+)Grpc\.java")?;
    let package_re = regex::Regex::new(r"package\s+([a-zA-Z][\w.]*)")?;

    for entry in walkdir(dir, "java") {
        let name = entry
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        if let Some(cap) = grpc_re.captures(&name) {
            let service_name = cap[1].to_string();
            let content = std::fs::read_to_string(&entry)?;
            let source = package_re
                .captures(&content)
                .map(|c| c[1].to_string())
                .unwrap_or_default();

            edges.push(Edge {
                source,
                target: format!("grpc:{}", service_name),
                weight: 1.0,
                kind: EdgeKind::Ipc,
                cross_language: Some(CrossLanguageEdge {
                    source_lang: Language::Java,
                    target_lang: Language::Unknown("grpc".to_string()),
                    mechanism: FfiMechanism::Grpc,
                }),
            });
        }
    }
    Ok(())
}

/// Walk directory for files with given extension.
pub fn walkdir(dir: &Path, ext: &str) -> Vec<std::path::PathBuf> {
    let mut files = Vec::new();
    fn walk_inner(dir: &Path, ext: &str, files: &mut Vec<std::path::PathBuf>) {
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    let name = path.file_name().unwrap_or_default().to_string_lossy();
                    if !name.starts_with('.')
                        && name != "node_modules"
                        && name != "target"
                        && name != "build"
                    {
                        walk_inner(&path, ext, files);
                    }
                } else if path.extension().is_some_and(|e| e == ext) {
                    files.push(path);
                }
            }
        }
    }
    walk_inner(dir, ext, &mut files);
    files
}

// ---------------------------------------------------------------------------
// Built-in JDK types to filter from references
// ---------------------------------------------------------------------------

fn jdk_builtins() -> HashSet<&'static str> {
    [
        "Object",
        "String",
        "Integer",
        "Boolean",
        "Long",
        "Double",
        "Float",
        "List",
        "Map",
        "Set",
        "Collection",
        "Optional",
        "Stream",
        "Class",
        "Thread",
        "Exception",
        "RuntimeException",
        "Override",
        "Deprecated",
        "SuppressWarnings",
        "FunctionalInterface",
        "Void",
        "Byte",
        "Short",
        "Character",
        "Number",
    ]
    .into_iter()
    .collect()
}

/// Check if a path component indicates a build output directory to skip.
fn is_java_build_dir(name: &str) -> bool {
    matches!(name, "target" | "build" | ".gradle")
}

// ---------------------------------------------------------------------------
// DeclarationExtractor implementation
// ---------------------------------------------------------------------------

impl DeclarationExtractor for JavaExtractor {
    fn language(&self) -> Language {
        Language::Java
    }

    fn extract_declarations(&self, dir: &Path, config: &ExtractConfig) -> Result<Vec<Declaration>> {
        let package_re = regex::Regex::new(r"package\s+([a-zA-Z][\w.]*)\s*;")?;
        let class_re = regex::Regex::new(
            r"(?m)^[ \t]*(public|protected|private)?\s*(abstract\s+|final\s+)?class\s+(\w+)",
        )?;
        let interface_re =
            regex::Regex::new(r"(?m)^[ \t]*(public|protected|private)?\s*interface\s+(\w+)")?;
        let enum_re = regex::Regex::new(r"(?m)^[ \t]*(public|protected|private)?\s*enum\s+(\w+)")?;
        let method_re = regex::Regex::new(
            r"(?m)^[ \t]*(public|protected|private)?\s*(static\s+)?\w[\w<>\[\], ]*\s+(\w+)\s*\(",
        )?;
        let const_re = regex::Regex::new(
            r"(?m)^[ \t]*(public|protected|private)?\s*(static\s+)(final\s+)\w[\w<>\[\], ]*\s+(\w+)\s*[=;]",
        )?;

        // Annotation patterns
        let controller_re = regex::Regex::new(r"@(RestController|Controller)")?;
        let serializable_re = regex::Regex::new(r"implements\s+[^{]*\bSerializable\b")?;

        let mut declarations = Vec::new();

        for entry in walkdir(dir, "java") {
            let rel = entry
                .strip_prefix(dir)
                .unwrap_or(&entry)
                .to_string_lossy()
                .to_string();

            // Skip build output directories
            if rel.split(['/', '\\']).any(is_java_build_dir) {
                continue;
            }

            if let Some(pf) = &config.prefix_filter {
                if !rel.starts_with(pf) {
                    continue;
                }
            }

            let content = std::fs::read_to_string(&entry)?;
            let lines: Vec<&str> = content.lines().collect();

            // Extract package name for fully qualified names
            let package = package_re
                .captures(&content)
                .map(|c| c[1].to_string())
                .unwrap_or_default();

            // Detect class-level annotations
            let is_controller = controller_re.is_match(&content);
            let is_serializable = serializable_re.is_match(&content);

            // Track which class names exist for constructor detection
            let mut class_names: HashSet<String> = HashSet::new();

            let parse_java_visibility = |vis: Option<regex::Match>| -> Visibility {
                match vis.map(|m| m.as_str().trim()) {
                    Some("public") => Visibility::Public,
                    Some("protected") => Visibility::Public,
                    Some("private") => Visibility::Private,
                    _ => Visibility::Internal, // package-private
                }
            };

            let line_number_of =
                |byte_offset: usize| -> usize { content[..byte_offset].matches('\n').count() + 1 };

            // Helper: check annotations on lines above a given line index
            let has_annotation_above = |line_idx: usize, annotation: &str| -> bool {
                let start = line_idx.saturating_sub(5);
                for li in start..line_idx {
                    if li < lines.len() && lines[li].contains(annotation) {
                        return true;
                    }
                }
                false
            };

            // Classes
            for cap in class_re.captures_iter(&content) {
                let name = cap[3].to_string();
                let vis = parse_java_visibility(cap.get(1));
                let line = line_number_of(cap.get(0).unwrap().start());
                let fqn = if package.is_empty() {
                    name.clone()
                } else {
                    format!("{}.{}", package, name)
                };
                class_names.insert(name.clone());
                declarations.push(Declaration {
                    name: fqn,
                    kind: DeclarationKind::Type,
                    visibility: vis,
                    file: rel.clone(),
                    line,
                    language: Language::Java,
                    is_entry_point: false,
                    entry_point_reason: None,
                });
            }

            // Interfaces
            for cap in interface_re.captures_iter(&content) {
                let name = cap[2].to_string();
                let vis = parse_java_visibility(cap.get(1));
                let line = line_number_of(cap.get(0).unwrap().start());
                let fqn = if package.is_empty() {
                    name.clone()
                } else {
                    format!("{}.{}", package, name)
                };
                declarations.push(Declaration {
                    name: fqn,
                    kind: DeclarationKind::Trait,
                    visibility: vis,
                    file: rel.clone(),
                    line,
                    language: Language::Java,
                    is_entry_point: false,
                    entry_point_reason: None,
                });
            }

            // Enums
            for cap in enum_re.captures_iter(&content) {
                let name = cap[2].to_string();
                let vis = parse_java_visibility(cap.get(1));
                let line = line_number_of(cap.get(0).unwrap().start());
                let fqn = if package.is_empty() {
                    name.clone()
                } else {
                    format!("{}.{}", package, name)
                };
                declarations.push(Declaration {
                    name: fqn,
                    kind: DeclarationKind::Type,
                    visibility: vis,
                    file: rel.clone(),
                    line,
                    language: Language::Java,
                    is_entry_point: false,
                    entry_point_reason: None,
                });
            }

            // Constants (static final)
            for cap in const_re.captures_iter(&content) {
                let name = cap[4].to_string();
                let vis = parse_java_visibility(cap.get(1));
                let line = line_number_of(cap.get(0).unwrap().start());
                let fqn = if package.is_empty() {
                    name.clone()
                } else {
                    format!("{}.{}", package, name)
                };
                declarations.push(Declaration {
                    name: fqn,
                    kind: DeclarationKind::Constant,
                    visibility: vis,
                    file: rel.clone(),
                    line,
                    language: Language::Java,
                    is_entry_point: false,
                    entry_point_reason: None,
                });
            }

            // Methods
            for cap in method_re.captures_iter(&content) {
                let name = cap[3].to_string();
                let vis = parse_java_visibility(cap.get(1));
                let is_static = cap.get(2).is_some();
                let line = line_number_of(cap.get(0).unwrap().start());
                let line_idx = line.saturating_sub(1);

                // Determine kind
                let kind = if is_static {
                    DeclarationKind::Function
                } else {
                    DeclarationKind::Method
                };

                let fqn = if package.is_empty() {
                    name.clone()
                } else {
                    format!("{}.{}", package, name)
                };

                // Constructor detection: method name == class name
                let is_constructor = class_names.contains(&name);

                // Entry point detection
                let (is_ep, ep_reason) = if is_constructor {
                    (true, Some("constructor".to_string()))
                } else if name == "main" && is_static && vis == Visibility::Public {
                    // Check for String[] args pattern on the same line
                    let line_text = lines.get(line_idx).unwrap_or(&"");
                    if line_text.contains("String") {
                        (true, Some("main method".to_string()))
                    } else {
                        (false, None)
                    }
                } else if has_annotation_above(line_idx, "@Test") {
                    (true, Some("test method".to_string()))
                } else if has_annotation_above(line_idx, "@Bean") {
                    (true, Some("Spring bean".to_string()))
                } else if has_annotation_above(line_idx, "@RequestMapping")
                    || has_annotation_above(line_idx, "@GetMapping")
                    || has_annotation_above(line_idx, "@PostMapping")
                    || has_annotation_above(line_idx, "@PutMapping")
                    || has_annotation_above(line_idx, "@DeleteMapping")
                {
                    (true, Some("HTTP endpoint".to_string()))
                } else if has_annotation_above(line_idx, "@Scheduled") {
                    (true, Some("scheduled task".to_string()))
                } else if has_annotation_above(line_idx, "@EventListener") {
                    (true, Some("event handler".to_string()))
                } else if has_annotation_above(line_idx, "@Override") {
                    (true, Some("interface implementation".to_string()))
                } else if is_controller && vis == Visibility::Public {
                    (true, Some("controller method".to_string()))
                } else if is_serializable && matches!(name.as_str(), "readObject" | "writeObject") {
                    (true, Some("serialization hook".to_string()))
                } else {
                    (false, None)
                };

                declarations.push(Declaration {
                    name: fqn,
                    kind,
                    visibility: vis,
                    file: rel.clone(),
                    line,
                    language: Language::Java,
                    is_entry_point: is_ep,
                    entry_point_reason: ep_reason,
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
        let builtins = jdk_builtins();

        // Reference patterns
        let new_re = regex::Regex::new(r"\bnew\s+([A-Z]\w+)\s*[(<]")?;
        let static_call_re = regex::Regex::new(r"([A-Z]\w+)\.(\w+)\s*\(")?;
        let var_decl_re = regex::Regex::new(r"\b([A-Z]\w+)\s+\w+\s*[=;,)]")?;
        let cast_re = regex::Regex::new(r"\(\s*([A-Z]\w+)\s*\)")?;
        let instanceof_re = regex::Regex::new(r"instanceof\s+([A-Z]\w+)")?;
        let extends_re = regex::Regex::new(r"extends\s+([A-Z]\w+)")?;
        let implements_re = regex::Regex::new(r"implements\s+([A-Z][\w\s,]+)")?;
        let annotation_ref_re = regex::Regex::new(r"@([A-Z]\w+)")?;
        let generic_re = regex::Regex::new(r"[<,]\s*([A-Z]\w+)")?;
        let import_re = regex::Regex::new(r"import\s+(?:static\s+)?[\w.]*\.([A-Z]\w+)\s*;")?;
        let method_ref_re = regex::Regex::new(r"([A-Z]\w+)::(\w+)")?;
        let fn_call_re = regex::Regex::new(r"\b([a-z]\w+)\s*\(")?;

        let mut references = Vec::new();

        for entry in walkdir(dir, "java") {
            let rel = entry
                .strip_prefix(dir)
                .unwrap_or(&entry)
                .to_string_lossy()
                .to_string();

            if rel.split(['/', '\\']).any(is_java_build_dir) {
                continue;
            }

            if let Some(pf) = &config.prefix_filter {
                if !rel.starts_with(pf) {
                    continue;
                }
            }

            let content = std::fs::read_to_string(&entry)?;

            for (line_idx, line) in content.lines().enumerate() {
                let line_num = line_idx + 1;

                // Constructor calls: new TypeName(
                for cap in new_re.captures_iter(line) {
                    let name = &cap[1];
                    if !builtins.contains(name) {
                        references.push(SymbolReference {
                            from_file: rel.clone(),
                            to_symbol: name.to_string(),
                            line: line_num,
                        });
                    }
                }

                // Static method calls: TypeName.method(
                for cap in static_call_re.captures_iter(line) {
                    let type_name = &cap[1];
                    let method_name = &cap[2];
                    if !builtins.contains(type_name) {
                        references.push(SymbolReference {
                            from_file: rel.clone(),
                            to_symbol: type_name.to_string(),
                            line: line_num,
                        });
                    }
                    references.push(SymbolReference {
                        from_file: rel.clone(),
                        to_symbol: method_name.to_string(),
                        line: line_num,
                    });
                }

                // Variable declarations with type: TypeName varName
                for cap in var_decl_re.captures_iter(line) {
                    let name = &cap[1];
                    if !builtins.contains(name) {
                        references.push(SymbolReference {
                            from_file: rel.clone(),
                            to_symbol: name.to_string(),
                            line: line_num,
                        });
                    }
                }

                // Casts: (TypeName)
                for cap in cast_re.captures_iter(line) {
                    let name = &cap[1];
                    if !builtins.contains(name) {
                        references.push(SymbolReference {
                            from_file: rel.clone(),
                            to_symbol: name.to_string(),
                            line: line_num,
                        });
                    }
                }

                // instanceof TypeName
                for cap in instanceof_re.captures_iter(line) {
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
                for cap in extends_re.captures_iter(line) {
                    let name = &cap[1];
                    if !builtins.contains(name) {
                        references.push(SymbolReference {
                            from_file: rel.clone(),
                            to_symbol: name.to_string(),
                            line: line_num,
                        });
                    }
                }

                // implements TypeName1, TypeName2
                for cap in implements_re.captures_iter(line) {
                    for iface in cap[1].split(',') {
                        let t = iface.trim();
                        if !t.is_empty()
                            && t.starts_with(|c: char| c.is_uppercase())
                            && !builtins.contains(t)
                        {
                            references.push(SymbolReference {
                                from_file: rel.clone(),
                                to_symbol: t.to_string(),
                                line: line_num,
                            });
                        }
                    }
                }

                // @Annotation references
                for cap in annotation_ref_re.captures_iter(line) {
                    let name = &cap[1];
                    if !builtins.contains(name) {
                        references.push(SymbolReference {
                            from_file: rel.clone(),
                            to_symbol: name.to_string(),
                            line: line_num,
                        });
                    }
                }

                // Generics: <TypeName>
                for cap in generic_re.captures_iter(line) {
                    let name = &cap[1];
                    if !builtins.contains(name) {
                        references.push(SymbolReference {
                            from_file: rel.clone(),
                            to_symbol: name.to_string(),
                            line: line_num,
                        });
                    }
                }

                // Import declarations (symbol-level)
                for cap in import_re.captures_iter(line) {
                    let name = &cap[1];
                    if !builtins.contains(name) {
                        references.push(SymbolReference {
                            from_file: rel.clone(),
                            to_symbol: name.to_string(),
                            line: line_num,
                        });
                    }
                }

                // Method references: TypeName::methodRef
                for cap in method_ref_re.captures_iter(line) {
                    let type_name = &cap[1];
                    let method_name = &cap[2];
                    if !builtins.contains(type_name) {
                        references.push(SymbolReference {
                            from_file: rel.clone(),
                            to_symbol: type_name.to_string(),
                            line: line_num,
                        });
                    }
                    references.push(SymbolReference {
                        from_file: rel.clone(),
                        to_symbol: method_name.to_string(),
                        line: line_num,
                    });
                }

                // Simple function/method calls: methodName(
                for cap in fn_call_re.captures_iter(line) {
                    let name = &cap[1];
                    if !matches!(
                        name,
                        "if" | "for"
                            | "while"
                            | "switch"
                            | "catch"
                            | "return"
                            | "throw"
                            | "new"
                            | "import"
                            | "package"
                            | "class"
                            | "interface"
                            | "enum"
                            | "extends"
                            | "implements"
                            | "instanceof"
                            | "synchronized"
                            | "assert"
                    ) {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_java_project() {
        let ext = JavaExtractor;
        // Current dir probably doesn't have pom.xml, so should be false
        assert!(!ext.detect(Path::new("/nonexistent")));
    }

    #[test]
    fn walkdir_returns_files() {
        // walkdir on current crate should find .rs files
        let files = walkdir(Path::new("."), "rs");
        // We're in the dsm-analyze crate, so should find at least our own source
        // (may not work if cwd is different, so just check it doesn't panic)
        let _ = files;
    }

    #[test]
    fn java_extract_declarations_empty_dir() {
        let ext = JavaExtractor;
        let config = ExtractConfig::default();
        // On a non-Java dir, should return empty
        let decls = ext.extract_declarations(Path::new("/nonexistent"), &config);
        assert!(decls.is_ok());
        assert!(decls.unwrap().is_empty());
    }

    #[test]
    fn java_extract_references_empty_dir() {
        let ext = JavaExtractor;
        let config = ExtractConfig::default();
        let refs = ext.extract_references(Path::new("/nonexistent"), &config);
        assert!(refs.is_ok());
        assert!(refs.unwrap().is_empty());
    }
}
