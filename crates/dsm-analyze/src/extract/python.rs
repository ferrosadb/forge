use crate::extract::*;
use anyhow::Result;
use std::path::Path;

pub struct PythonExtractor;

impl Extractor for PythonExtractor {
    fn name(&self) -> &str {
        "python"
    }
    fn language(&self) -> Language {
        Language::Python
    }

    fn detect(&self, dir: &Path) -> bool {
        dir.join("setup.py").exists()
            || dir.join("pyproject.toml").exists()
            || dir.join("setup.cfg").exists()
            || dir.join("requirements.txt").exists()
    }

    fn extract(&self, dir: &Path, config: &ExtractConfig) -> Result<Vec<Edge>> {
        let import_re = regex::Regex::new(r"(?m)^\s*import\s+([\w.]+)")?;
        let from_re = regex::Regex::new(r"(?m)^\s*from\s+([\w.]+)\s+import")?;

        let mut edges = Vec::new();

        for entry in super::java::walkdir(dir, "py") {
            let content = std::fs::read_to_string(&entry)?;
            let source = py_module_from_path(&entry, dir);

            for cap in import_re.captures_iter(&content) {
                let target = normalize_import(&cap[1], config);
                if should_include(&target, &source, config) {
                    edges.push(Edge {
                        source: source.clone(),
                        target,
                        weight: 1.0,
                        kind: EdgeKind::Import,
                        cross_language: None,
                    });
                }
            }

            for cap in from_re.captures_iter(&content) {
                let mut target = cap[1].to_string();
                // Handle relative imports
                if target.starts_with('.') {
                    target = resolve_relative(&source, &target);
                }
                let target = normalize_import(&target, config);
                if should_include(&target, &source, config) {
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

        Ok(edges)
    }

    fn detect_cross_language(&self, dir: &Path, config: &ExtractConfig) -> Result<Vec<Edge>> {
        if !config.detect_cross_language {
            return Ok(vec![]);
        }
        let mut edges = Vec::new();
        scan_ctypes(dir, &mut edges)?;
        scan_cffi(dir, &mut edges)?;
        scan_c_extensions(dir, &mut edges)?;
        scan_pyo3_python_side(dir, &mut edges)?;
        Ok(edges)
    }
}

fn py_module_from_path(file: &Path, root: &Path) -> String {
    let relative = file.strip_prefix(root).unwrap_or(file);
    let parts: Vec<&str> = relative
        .components()
        .filter_map(|c| c.as_os_str().to_str())
        .collect();
    let mut module_parts: Vec<String> = parts
        .iter()
        .map(|p| p.strip_suffix(".py").unwrap_or(p).to_string())
        .collect();
    // Remove __init__
    if module_parts.last().is_some_and(|p| p == "__init__") {
        module_parts.pop();
    }
    module_parts.join(".")
}

fn normalize_import(import: &str, config: &ExtractConfig) -> String {
    match config.level {
        GranularityLevel::Summary => {
            let parts: Vec<&str> = import.split('.').collect();
            if parts.len() > 1 {
                parts[0].to_string()
            } else {
                import.to_string()
            }
        }
        GranularityLevel::Full => import.to_string(),
    }
}

fn resolve_relative(source: &str, relative: &str) -> String {
    let dots = relative.chars().take_while(|&c| c == '.').count();
    let suffix = &relative[dots..];
    let parts: Vec<&str> = source.split('.').collect();
    if dots <= parts.len() {
        let base = parts[..parts.len() - dots].join(".");
        if suffix.is_empty() {
            base
        } else {
            format!("{}.{}", base, suffix)
        }
    } else {
        suffix.to_string()
    }
}

fn should_include(target: &str, source: &str, config: &ExtractConfig) -> bool {
    if target == source || target.is_empty() {
        return false;
    }
    if let Some(prefix) = &config.prefix_filter {
        if !target.starts_with(prefix) {
            return false;
        }
    }
    // Exclude stdlib
    let stdlib = [
        "os",
        "sys",
        "re",
        "json",
        "typing",
        "collections",
        "functools",
        "pathlib",
        "subprocess",
        "unittest",
        "pytest",
        "abc",
        "io",
        "logging",
        "datetime",
        "math",
        "itertools",
        "dataclasses",
    ];
    !stdlib.contains(&target.split('.').next().unwrap_or(""))
}

fn scan_ctypes(dir: &Path, edges: &mut Vec<Edge>) -> Result<()> {
    let re = regex::Regex::new(r"ctypes\.(?:cdll|CDLL|windll|WinDLL)")?;
    for entry in super::java::walkdir(dir, "py") {
        let content = std::fs::read_to_string(&entry)?;
        if re.is_match(&content) {
            let source = py_module_from_path(&entry, dir);
            edges.push(Edge {
                source,
                target: "native:ctypes_lib".to_string(),
                weight: 1.0,
                kind: EdgeKind::Ffi,
                cross_language: Some(CrossLanguageEdge {
                    source_lang: Language::Python,
                    target_lang: Language::C,
                    mechanism: FfiMechanism::Ctypes,
                }),
            });
        }
    }
    Ok(())
}

fn scan_cffi(dir: &Path, edges: &mut Vec<Edge>) -> Result<()> {
    let re = regex::Regex::new(r"cffi\.FFI\(\)")?;
    for entry in super::java::walkdir(dir, "py") {
        let content = std::fs::read_to_string(&entry)?;
        if re.is_match(&content) {
            let source = py_module_from_path(&entry, dir);
            edges.push(Edge {
                source,
                target: "native:cffi_lib".to_string(),
                weight: 1.0,
                kind: EdgeKind::Ffi,
                cross_language: Some(CrossLanguageEdge {
                    source_lang: Language::Python,
                    target_lang: Language::C,
                    mechanism: FfiMechanism::Cffi,
                }),
            });
        }
    }
    Ok(())
}

fn scan_c_extensions(dir: &Path, edges: &mut Vec<Edge>) -> Result<()> {
    // Check setup.py for ext_modules
    let setup_py = dir.join("setup.py");
    if setup_py.exists() {
        let content = std::fs::read_to_string(&setup_py)?;
        if content.contains("ext_modules") || content.contains("Extension(") {
            edges.push(Edge {
                source: "setup.py".to_string(),
                target: "native:c_extension".to_string(),
                weight: 1.0,
                kind: EdgeKind::Ffi,
                cross_language: Some(CrossLanguageEdge {
                    source_lang: Language::Python,
                    target_lang: Language::C,
                    mechanism: FfiMechanism::PyExtension,
                }),
            });
        }
    }
    // Check for .pyx Cython files
    for entry in super::java::walkdir(dir, "pyx") {
        let source = py_module_from_path(&entry, dir);
        edges.push(Edge {
            source,
            target: "native:cython".to_string(),
            weight: 1.0,
            kind: EdgeKind::Ffi,
            cross_language: Some(CrossLanguageEdge {
                source_lang: Language::Python,
                target_lang: Language::C,
                mechanism: FfiMechanism::PyExtension,
            }),
        });
    }
    Ok(())
}

fn scan_pyo3_python_side(dir: &Path, edges: &mut Vec<Edge>) -> Result<()> {
    let pyproject = dir.join("pyproject.toml");
    if pyproject.exists() {
        let content = std::fs::read_to_string(&pyproject)?;
        if content.contains("maturin") {
            edges.push(Edge {
                source: "python:project".to_string(),
                target: "rust:crate".to_string(),
                weight: 1.0,
                kind: EdgeKind::Ffi,
                cross_language: Some(CrossLanguageEdge {
                    source_lang: Language::Python,
                    target_lang: Language::Rust,
                    mechanism: FfiMechanism::PyO3,
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
    fn detect_python_project() {
        let ext = PythonExtractor;
        assert!(!ext.detect(Path::new("/nonexistent")));
    }

    #[test]
    fn py_module_from_path_basic() {
        assert_eq!(
            py_module_from_path(Path::new("src/mymod/core.py"), Path::new("src")),
            "mymod.core"
        );
        assert_eq!(
            py_module_from_path(Path::new("pkg/__init__.py"), Path::new(".")),
            "pkg"
        );
    }

    #[test]
    fn resolve_relative_imports() {
        assert_eq!(resolve_relative("pkg.sub.mod", ".other"), "pkg.sub.other");
        assert_eq!(resolve_relative("pkg.sub.mod", "..other"), "pkg.other");
    }

    #[test]
    fn stdlib_excluded() {
        let config = ExtractConfig::default();
        assert!(!should_include("os", "mymod", &config));
        assert!(!should_include("sys", "mymod", &config));
        assert!(should_include("mypackage", "mymod", &config));
    }
}
