use crate::extract::*;
use anyhow::Result;
use std::path::Path;

pub struct GoExtractor;

impl Extractor for GoExtractor {
    fn name(&self) -> &str {
        "go"
    }
    fn language(&self) -> Language {
        Language::Go
    }

    fn detect(&self, dir: &Path) -> bool {
        dir.join("go.mod").exists()
    }

    fn extract(&self, dir: &Path, config: &ExtractConfig) -> Result<Vec<Edge>> {
        match config.level {
            GranularityLevel::Summary => extract_via_go_list(dir, config),
            GranularityLevel::Full => extract_from_source(dir, config),
        }
    }

    fn detect_cross_language(&self, dir: &Path, config: &ExtractConfig) -> Result<Vec<Edge>> {
        if !config.detect_cross_language {
            return Ok(vec![]);
        }
        let mut edges = Vec::new();
        scan_cgo(dir, &mut edges)?;
        scan_grpc_go(dir, &mut edges)?;
        Ok(edges)
    }
}

fn extract_via_go_list(dir: &Path, config: &ExtractConfig) -> Result<Vec<Edge>> {
    let output = std::process::Command::new("go")
        .args(["list", "-json", "./..."])
        .current_dir(dir)
        .output();

    match output {
        Ok(out) if out.status.success() => {
            parse_go_list_output(&String::from_utf8_lossy(&out.stdout), config)
        }
        Ok(out) => {
            let stderr = String::from_utf8_lossy(&out.stderr);
            anyhow::bail!("go list failed: {}", stderr)
        }
        Err(e) => anyhow::bail!("Failed to run go: {}", e),
    }
}

fn parse_go_list_output(output: &str, config: &ExtractConfig) -> Result<Vec<Edge>> {
    let mut edges = Vec::new();

    // Go list -json outputs concatenated JSON objects (not an array)
    // Parse them one by one
    let mut decoder = output.as_bytes();
    loop {
        let pkg: std::result::Result<serde_json::Value, _> = serde_json::from_reader(&mut decoder);
        match pkg {
            Ok(pkg) => {
                let import_path = pkg.get("ImportPath").and_then(|v| v.as_str()).unwrap_or("");

                if let Some(imports) = pkg.get("Imports").and_then(|v| v.as_array()) {
                    for imp in imports {
                        if let Some(imp_path) = imp.as_str() {
                            if let Some(prefix) = &config.prefix_filter {
                                if !imp_path.starts_with(prefix) {
                                    continue;
                                }
                            }
                            // Skip stdlib
                            if !imp_path.contains('.') {
                                continue;
                            }
                            edges.push(Edge {
                                source: import_path.to_string(),
                                target: imp_path.to_string(),
                                weight: 1.0,
                                kind: EdgeKind::Import,
                                cross_language: None,
                            });
                        }
                    }
                }
            }
            Err(_) => break,
        }
    }

    Ok(edges)
}

fn extract_from_source(dir: &Path, config: &ExtractConfig) -> Result<Vec<Edge>> {
    let import_re = regex::Regex::new(r#"(?m)^\s*"([^"]+)""#)?;
    let _package_re = regex::Regex::new(r"(?m)^package\s+(\w+)")?;

    let mut edges = Vec::new();

    for entry in super::java::walkdir(dir, "go") {
        let content = std::fs::read_to_string(&entry)?;
        let source = go_package_from_path(&entry, dir);

        // Find import blocks
        let mut in_import = false;
        for line in content.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with("import (") {
                in_import = true;
                continue;
            }
            if trimmed == ")" && in_import {
                in_import = false;
                continue;
            }
            if trimmed.starts_with("import \"") || in_import {
                if let Some(cap) = import_re.captures(trimmed) {
                    let target = cap[1].to_string();
                    if !target.contains('.') {
                        continue; // stdlib
                    }
                    if let Some(prefix) = &config.prefix_filter {
                        if !target.starts_with(prefix) {
                            continue;
                        }
                    }
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

fn go_package_from_path(file: &Path, root: &Path) -> String {
    let relative = file.strip_prefix(root).unwrap_or(file);
    let parent = relative.parent().unwrap_or(Path::new("."));
    parent.to_string_lossy().replace('\\', "/")
}

fn scan_cgo(dir: &Path, edges: &mut Vec<Edge>) -> Result<()> {
    let cgo_re = regex::Regex::new(r#"import\s+"C""#)?;
    let include_re = regex::Regex::new(r#"//\s*#include\s+[<"]([^>"]+)"#)?;

    for entry in super::java::walkdir(dir, "go") {
        let content = std::fs::read_to_string(&entry)?;
        if cgo_re.is_match(&content) {
            let source = go_package_from_path(&entry, dir);
            let header = include_re
                .captures(&content)
                .map(|c| c[1].to_string())
                .unwrap_or_else(|| "c_lib".to_string());

            edges.push(Edge {
                source,
                target: format!("native:{}", header),
                weight: 1.0,
                kind: EdgeKind::Ffi,
                cross_language: Some(CrossLanguageEdge {
                    source_lang: Language::Go,
                    target_lang: Language::C,
                    mechanism: FfiMechanism::Cgo,
                }),
            });
        }
    }
    Ok(())
}

fn scan_grpc_go(dir: &Path, edges: &mut Vec<Edge>) -> Result<()> {
    let grpc_re = regex::Regex::new(r"(\w+)_grpc\.pb\.go")?;

    for entry in super::java::walkdir(dir, "go") {
        let name = entry
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        if let Some(cap) = grpc_re.captures(&name) {
            let service = cap[1].to_string();
            let source = go_package_from_path(&entry, dir);
            edges.push(Edge {
                source,
                target: format!("grpc:{}", service),
                weight: 1.0,
                kind: EdgeKind::Ipc,
                cross_language: Some(CrossLanguageEdge {
                    source_lang: Language::Go,
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
    fn detect_go_project() {
        let ext = GoExtractor;
        assert!(!ext.detect(Path::new("/nonexistent")));
    }

    #[test]
    fn go_package_from_path_basic() {
        assert_eq!(
            go_package_from_path(Path::new("pkg/server/main.go"), Path::new(".")),
            "pkg/server"
        );
    }
}
