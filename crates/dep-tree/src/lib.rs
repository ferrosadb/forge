//! Dependency tree extractor: walks source files in a directory, extracts
//! module names and their dependencies (imports/uses/aliases), and returns
//! a structured per-module dependency map.
//!
//! Multi-language aware: Elixir, Rust, Python, Go, TypeScript/JS, Java.

use anyhow::Result;
use ignore::WalkBuilder;
use regex::Regex;
use serde::Serialize;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// Top-level dependency tree for a project.
#[derive(Debug, Serialize)]
pub struct DepTree {
    pub root: PathBuf,
    pub language: String,
    pub module_count: usize,
    pub modules: Vec<ModuleDeps>,
}

/// Per-module dependency entry.
#[derive(Debug, Serialize)]
pub struct ModuleDeps {
    pub module: String,
    pub file: String,
    pub depends_on: Vec<String>,
}

/// Detected language enum (internal).
#[derive(Debug, Clone, PartialEq)]
enum Language {
    Elixir,
    Rust,
    Go,
    TypeScript,
    Java,
    Python,
}

impl Language {
    fn as_str(&self) -> &'static str {
        match self {
            Language::Elixir => "elixir",
            Language::Rust => "rust",
            Language::Go => "go",
            Language::TypeScript => "typescript",
            Language::Java => "java",
            Language::Python => "python",
        }
    }
}

/// Skip-list of directory names to always ignore during walks.
static SKIP_DIRS: &[&str] = &[
    "_build",
    "deps",
    "node_modules",
    ".git",
    "target",
    "vendor",
    "__pycache__",
    ".venv",
];

/// Detect the primary language from marker files in `dir` or its ancestors.
/// Priority: Elixir > Rust > Go > TypeScript > Java > Python.
///
/// Walks up parent directories so that `dep-tree lib/` still finds `mix.exs`
/// at the project root.
fn detect_language(dir: &Path) -> Option<Language> {
    let checks: &[(&str, Language)] = &[
        ("mix.exs", Language::Elixir),
        ("Cargo.toml", Language::Rust),
        ("go.mod", Language::Go),
        ("package.json", Language::TypeScript),
        ("pom.xml", Language::Java),
        ("build.gradle", Language::Java),
        ("pyproject.toml", Language::Python),
        ("setup.py", Language::Python),
    ];

    // Try the given dir first, then walk up ancestors.
    let canonical = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
    let mut current = Some(canonical.as_path());
    while let Some(d) = current {
        for (marker, lang) in checks {
            if d.join(marker).exists() {
                return Some(lang.clone());
            }
        }
        current = d.parent();
    }
    None
}

/// Returns true if any path component is in the SKIP_DIRS list.
fn should_skip(path: &Path) -> bool {
    path.components().any(|c| {
        SKIP_DIRS
            .iter()
            .any(|skip| c.as_os_str() == std::ffi::OsStr::new(skip))
    })
}

/// Build the dependency tree for the project rooted at `dir`.
pub fn build_dep_tree(dir: &Path) -> Result<DepTree> {
    let lang = detect_language(dir).unwrap_or(Language::Python);
    let modules = match &lang {
        Language::Elixir => extract_elixir(dir)?,
        Language::Rust => extract_rust(dir)?,
        Language::Go => extract_go(dir)?,
        Language::TypeScript => extract_typescript(dir)?,
        Language::Java => extract_java(dir)?,
        Language::Python => extract_python(dir)?,
    };

    let module_count = modules.len();
    Ok(DepTree {
        root: dir.to_path_buf(),
        language: lang.as_str().to_string(),
        module_count,
        modules,
    })
}

// ---------------------------------------------------------------------------
// Shared walk helper
// ---------------------------------------------------------------------------

/// Walk `dir` and collect all files whose extension matches `exts`.
/// Automatically skips SKIP_DIRS entries.
fn walk_files(dir: &Path, exts: &[&str]) -> Vec<PathBuf> {
    let mut paths = Vec::new();

    let walker = WalkBuilder::new(dir)
        .hidden(false)
        .git_ignore(true)
        .git_global(false)
        .build();

    for entry in walker.flatten() {
        let path = entry.path().to_path_buf();
        if path.is_dir() {
            continue;
        }
        if should_skip(&path) {
            continue;
        }
        if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            if exts.contains(&ext) {
                paths.push(path);
            }
        }
    }

    paths
}

/// Deduplicate a Vec<String> preserving insertion order.
fn dedup(mut v: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    v.retain(|s| seen.insert(s.clone()));
    v
}

// ---------------------------------------------------------------------------
// Elixir
// ---------------------------------------------------------------------------

fn extract_elixir(dir: &Path) -> Result<Vec<ModuleDeps>> {
    let defmodule_re = Regex::new(r"defmodule\s+(\S+)")?;
    // use/import/alias may have trailing comma, parens, or `as:` clause — grab
    // just the module name token (first non-whitespace after keyword).
    let dep_re = Regex::new(r"^\s*(?:use|import|alias)\s+([\w.]+)")?;

    let files = walk_files(dir, &["ex", "exs"]);
    let mut modules = Vec::new();

    for path in &files {
        let source = match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(_) => continue,
        };
        let rel = path
            .strip_prefix(dir)
            .unwrap_or(path)
            .to_string_lossy()
            .to_string();

        // A file may contain multiple defmodule blocks. Collect them all.
        // We associate deps with the most recently seen defmodule.
        let mut current_module: Option<String> = None;
        let mut current_deps: Vec<String> = Vec::new();

        for line in source.lines() {
            if let Some(cap) = defmodule_re.captures(line) {
                // Flush previous module if any
                if let Some(name) = current_module.take() {
                    modules.push(ModuleDeps {
                        module: name,
                        file: rel.clone(),
                        depends_on: dedup(std::mem::take(&mut current_deps)),
                    });
                }
                current_module = Some(cap[1].trim_end_matches(',').to_string());
                current_deps = Vec::new();
            } else if let Some(cap) = dep_re.captures(line) {
                let dep = cap[1].trim_end_matches(',').to_string();
                current_deps.push(dep);
            }
        }

        // Flush last module
        if let Some(name) = current_module {
            modules.push(ModuleDeps {
                module: name,
                file: rel,
                depends_on: dedup(current_deps),
            });
        }
    }

    Ok(modules)
}

// ---------------------------------------------------------------------------
// Rust
// ---------------------------------------------------------------------------

fn extract_rust(dir: &Path) -> Result<Vec<ModuleDeps>> {
    // Internal deps only: `use crate::...` and `use super::...`
    let use_re = Regex::new(r"^\s*use\s+(?:crate|super)::([\w:]+)")?;

    let files = walk_files(dir, &["rs"]);
    let mut modules = Vec::new();

    for path in &files {
        let source = match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(_) => continue,
        };
        let rel = path
            .strip_prefix(dir)
            .unwrap_or(path)
            .to_string_lossy()
            .to_string();

        // Derive module name from file path: strip leading `src/`, drop `.rs`,
        // replace path separators with `::`.
        let module_name = rust_module_name(&rel);

        let mut deps: Vec<String> = Vec::new();
        for line in source.lines() {
            if let Some(cap) = use_re.captures(line) {
                // Normalise trailing `{...}` group — keep the path prefix only.
                let raw = cap[1].trim_end_matches(';').to_string();
                let clean = raw
                    .trim_end_matches(['{', ' '])
                    .trim_end_matches("::")
                    .to_string();
                deps.push(clean);
            }
        }

        modules.push(ModuleDeps {
            module: module_name,
            file: rel,
            depends_on: dedup(deps),
        });
    }

    Ok(modules)
}

/// Convert a relative file path to a Rust module path.
/// `src/extract/java.rs` → `extract::java`
/// `src/lib.rs`          → `lib`
fn rust_module_name(rel: &str) -> String {
    let stripped = rel.trim_start_matches("src/").trim_end_matches(".rs");
    stripped.replace('/', "::")
}

// ---------------------------------------------------------------------------
// Go
// ---------------------------------------------------------------------------

fn extract_go(dir: &Path) -> Result<Vec<ModuleDeps>> {
    let pkg_re = Regex::new(r"^package\s+(\w+)")?;
    let import_single_re = Regex::new(r#"^\s*import\s+"([^"]+)""#)?;
    let import_item_re = Regex::new(r#"^\s*"([^"]+)""#)?;

    let files = walk_files(dir, &["go"]);
    let mut modules = Vec::new();

    for path in &files {
        let source = match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(_) => continue,
        };
        let rel = path
            .strip_prefix(dir)
            .unwrap_or(path)
            .to_string_lossy()
            .to_string();

        let mut package_name = String::new();
        let mut deps: Vec<String> = Vec::new();
        let mut in_import_block = false;

        for line in source.lines() {
            if let Some(cap) = pkg_re.captures(line) {
                package_name = cap[1].to_string();
                continue;
            }

            if line.trim() == "import (" {
                in_import_block = true;
                continue;
            }
            if in_import_block {
                if line.trim() == ")" {
                    in_import_block = false;
                    continue;
                }
                if let Some(cap) = import_item_re.captures(line) {
                    deps.push(cap[1].to_string());
                }
                continue;
            }

            // Single-line import
            if let Some(cap) = import_single_re.captures(line) {
                deps.push(cap[1].to_string());
            }
        }

        // Module name = package + file path context
        let dir_part = path
            .parent()
            .and_then(|p| p.strip_prefix(dir).ok())
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();

        let module_name = if dir_part.is_empty() {
            package_name
        } else {
            format!("{}/{}", dir_part, package_name)
        };

        modules.push(ModuleDeps {
            module: module_name,
            file: rel,
            depends_on: dedup(deps),
        });
    }

    Ok(modules)
}

// ---------------------------------------------------------------------------
// TypeScript / JavaScript
// ---------------------------------------------------------------------------

fn extract_typescript(dir: &Path) -> Result<Vec<ModuleDeps>> {
    // Only relative imports (start with ./ or ../)
    let import_re = Regex::new(r#"(?:import|from)\s+['"](\./[^'"]+|\.\.\/[^'"]+)['"]"#)?;

    let files = walk_files(dir, &["ts", "tsx", "js", "jsx"]);
    let mut modules = Vec::new();

    for path in &files {
        let source = match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(_) => continue,
        };
        let rel = path
            .strip_prefix(dir)
            .unwrap_or(path)
            .to_string_lossy()
            .to_string();

        // Module name from path: strip leading `src/`, drop extension.
        let module_name = rel
            .trim_start_matches("src/")
            .trim_end_matches(".tsx")
            .trim_end_matches(".ts")
            .trim_end_matches(".jsx")
            .trim_end_matches(".js")
            .to_string();

        let mut deps: Vec<String> = Vec::new();
        for cap in import_re.captures_iter(&source) {
            deps.push(cap[1].to_string());
        }

        modules.push(ModuleDeps {
            module: module_name,
            file: rel,
            depends_on: dedup(deps),
        });
    }

    Ok(modules)
}

// ---------------------------------------------------------------------------
// Java
// ---------------------------------------------------------------------------

fn extract_java(dir: &Path) -> Result<Vec<ModuleDeps>> {
    let package_re = Regex::new(r"^package\s+([\w.]+);")?;
    let class_re = Regex::new(r"(?:public\s+)?(?:class|interface|enum|record)\s+(\w+)")?;
    let import_re = Regex::new(r"^import\s+([\w.]+);")?;

    let files = walk_files(dir, &["java"]);
    let mut modules = Vec::new();

    for path in &files {
        let source = match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(_) => continue,
        };
        let rel = path
            .strip_prefix(dir)
            .unwrap_or(path)
            .to_string_lossy()
            .to_string();

        let mut package_name = String::new();
        let mut class_name = String::new();
        let mut deps: Vec<String> = Vec::new();

        for line in source.lines() {
            if let Some(cap) = package_re.captures(line) {
                package_name = cap[1].to_string();
            } else if let Some(cap) = import_re.captures(line) {
                deps.push(cap[1].to_string());
            } else if class_name.is_empty() {
                if let Some(cap) = class_re.captures(line) {
                    class_name = cap[1].to_string();
                }
            }
        }

        let module_name = if package_name.is_empty() {
            class_name
        } else if class_name.is_empty() {
            package_name
        } else {
            format!("{}.{}", package_name, class_name)
        };

        modules.push(ModuleDeps {
            module: module_name,
            file: rel,
            depends_on: dedup(deps),
        });
    }

    Ok(modules)
}

// ---------------------------------------------------------------------------
// Python
// ---------------------------------------------------------------------------

fn extract_python(dir: &Path) -> Result<Vec<ModuleDeps>> {
    // `from X import Y` — capture X; `import X` — capture X.
    let from_re = Regex::new(r"^from\s+([\w.]+)\s+import")?;
    let import_re = Regex::new(r"^import\s+([\w.,\s]+)")?;

    let files = walk_files(dir, &["py"]);
    let mut modules = Vec::new();

    for path in &files {
        let source = match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(_) => continue,
        };
        let rel = path
            .strip_prefix(dir)
            .unwrap_or(path)
            .to_string_lossy()
            .to_string();

        // Module name: replace `/` with `.`, drop `.py`, strip leading `./`
        let module_name = rel
            .trim_start_matches("./")
            .trim_end_matches(".py")
            .replace('/', ".");

        let mut deps: Vec<String> = Vec::new();

        for line in source.lines() {
            if let Some(cap) = from_re.captures(line) {
                deps.push(cap[1].to_string());
            } else if let Some(cap) = import_re.captures(line) {
                // `import os, sys` → ["os", "sys"]
                for part in cap[1].split(',') {
                    let name = part.trim().to_string();
                    if !name.is_empty() {
                        deps.push(name);
                    }
                }
            }
        }

        modules.push(ModuleDeps {
            module: module_name,
            file: rel,
            depends_on: dedup(deps),
        });
    }

    Ok(modules)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    // === Test list ===
    // [x] empty directory returns empty module list
    // [x] Elixir: extracts defmodule, use, import, alias
    // [x] Elixir: multiple defmodules in one file
    // [x] Rust: extracts crate-internal use statements only
    // [x] Rust: module name derived from file path
    // [x] Python: from X import and import X
    // [x] Go: package + import block
    // [x] TypeScript: relative imports only
    // [x] Java: package + class + imports
    // [x] language detection priority

    fn write(dir: &Path, rel: &str, content: &str) {
        let full = dir.join(rel);
        if let Some(parent) = full.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(full, content).unwrap();
    }

    // ------------------------------------------------------------------

    #[test]
    fn empty_dir_returns_empty_modules() {
        let tmp = tempfile::tempdir().unwrap();
        // No marker file → falls back to Python extractor, no .py files
        write(
            tmp.path(),
            "pyproject.toml",
            "[project]\nname = \"empty\"\n",
        );
        let tree = build_dep_tree(tmp.path()).unwrap();
        assert_eq!(tree.module_count, 0);
        assert!(tree.modules.is_empty());
    }

    // ------------------------------------------------------------------

    #[test]
    fn elixir_extracts_defmodule_and_deps() {
        let tmp = tempfile::tempdir().unwrap();
        write(tmp.path(), "mix.exs", "# mix\n");
        write(
            tmp.path(),
            "lib/my_app/worker.ex",
            r#"
defmodule MyApp.Worker do
  use GenServer
  import Logger
  alias MyApp.Repo
  alias MyApp.Schema.User

  def start_link(opts), do: GenServer.start_link(__MODULE__, opts)
end
"#,
        );

        let tree = build_dep_tree(tmp.path()).unwrap();
        assert_eq!(tree.language, "elixir");
        assert_eq!(tree.module_count, 1);

        let m = &tree.modules[0];
        assert_eq!(m.module, "MyApp.Worker");
        assert!(m.depends_on.contains(&"GenServer".to_string()));
        assert!(m.depends_on.contains(&"Logger".to_string()));
        assert!(m.depends_on.contains(&"MyApp.Repo".to_string()));
        assert!(m.depends_on.contains(&"MyApp.Schema.User".to_string()));
    }

    #[test]
    fn elixir_multiple_defmodules_in_one_file() {
        let tmp = tempfile::tempdir().unwrap();
        write(tmp.path(), "mix.exs", "# mix\n");
        write(
            tmp.path(),
            "lib/combined.ex",
            r#"
defmodule Alpha do
  import Plug.Conn
end

defmodule Beta do
  use Phoenix.Controller
  alias Alpha
end
"#,
        );

        let tree = build_dep_tree(tmp.path()).unwrap();
        assert_eq!(tree.module_count, 2);

        let alpha = tree.modules.iter().find(|m| m.module == "Alpha").unwrap();
        assert!(alpha.depends_on.contains(&"Plug.Conn".to_string()));

        let beta = tree.modules.iter().find(|m| m.module == "Beta").unwrap();
        assert!(beta.depends_on.contains(&"Phoenix.Controller".to_string()));
        assert!(beta.depends_on.contains(&"Alpha".to_string()));
    }

    // ------------------------------------------------------------------

    #[test]
    fn rust_extracts_internal_uses_only() {
        let tmp = tempfile::tempdir().unwrap();
        write(tmp.path(), "Cargo.toml", "[package]\nname = \"test\"\n");
        write(
            tmp.path(),
            "src/processor.rs",
            r#"
use crate::extractor::parse;
use crate::config::Config;
use super::utils::helper;
use std::collections::HashMap;  // external — should be ignored
use anyhow::Result;             // external — should be ignored
"#,
        );

        let tree = build_dep_tree(tmp.path()).unwrap();
        assert_eq!(tree.language, "rust");

        let m = tree
            .modules
            .iter()
            .find(|m| m.module == "processor")
            .unwrap();
        assert!(m.depends_on.contains(&"extractor::parse".to_string()));
        assert!(m.depends_on.contains(&"config::Config".to_string()));
        assert!(m.depends_on.contains(&"utils::helper".to_string()));
        // External crates must not appear
        assert!(!m.depends_on.iter().any(|d| d.contains("HashMap")));
        assert!(!m.depends_on.iter().any(|d| d.contains("anyhow")));
    }

    #[test]
    fn rust_module_name_from_path() {
        assert_eq!(rust_module_name("src/lib.rs"), "lib");
        assert_eq!(rust_module_name("src/extract/java.rs"), "extract::java");
        assert_eq!(rust_module_name("src/mod/sub/deep.rs"), "mod::sub::deep");
        // Files outside src/ keep full path-derived name
        assert_eq!(
            rust_module_name("tests/integration.rs"),
            "tests::integration"
        );
    }

    // ------------------------------------------------------------------

    #[test]
    fn python_extracts_from_and_import() {
        let tmp = tempfile::tempdir().unwrap();
        write(tmp.path(), "pyproject.toml", "[project]\nname = \"proj\"\n");
        write(
            tmp.path(),
            "myapp/utils.py",
            r#"
from myapp.models import User
from myapp.db import session
import os
import sys, pathlib
"#,
        );

        let tree = build_dep_tree(tmp.path()).unwrap();
        assert_eq!(tree.language, "python");

        let m = tree
            .modules
            .iter()
            .find(|m| m.module == "myapp.utils")
            .unwrap();
        assert!(m.depends_on.contains(&"myapp.models".to_string()));
        assert!(m.depends_on.contains(&"myapp.db".to_string()));
        assert!(m.depends_on.contains(&"os".to_string()));
        assert!(m.depends_on.contains(&"sys".to_string()));
        assert!(m.depends_on.contains(&"pathlib".to_string()));
    }

    // ------------------------------------------------------------------

    #[test]
    fn go_extracts_package_and_imports() {
        let tmp = tempfile::tempdir().unwrap();
        write(tmp.path(), "go.mod", "module example.com/myapp\ngo 1.21\n");
        write(
            tmp.path(),
            "handler/http.go",
            r#"
package handler

import (
    "net/http"
    "encoding/json"
    "example.com/myapp/service"
)

func Handle(w http.ResponseWriter, r *http.Request) {}
"#,
        );

        let tree = build_dep_tree(tmp.path()).unwrap();
        assert_eq!(tree.language, "go");

        let m = tree
            .modules
            .iter()
            .find(|m| m.module.ends_with("handler"))
            .unwrap();
        assert!(m.depends_on.contains(&"net/http".to_string()));
        assert!(m.depends_on.contains(&"encoding/json".to_string()));
        assert!(m
            .depends_on
            .contains(&"example.com/myapp/service".to_string()));
    }

    // ------------------------------------------------------------------

    #[test]
    fn typescript_relative_imports_only() {
        let tmp = tempfile::tempdir().unwrap();
        write(tmp.path(), "package.json", r#"{"name":"app"}"#);
        write(
            tmp.path(),
            "src/components/Button.tsx",
            r#"
import React from 'react';
import { theme } from './theme';
import { useAuth } from '../hooks/useAuth';
import type { ButtonProps } from './types';
"#,
        );

        let tree = build_dep_tree(tmp.path()).unwrap();
        assert_eq!(tree.language, "typescript");

        let m = tree
            .modules
            .iter()
            .find(|m| m.module.contains("Button"))
            .unwrap();
        assert!(m.depends_on.contains(&"./theme".to_string()));
        assert!(m.depends_on.contains(&"../hooks/useAuth".to_string()));
        assert!(m.depends_on.contains(&"./types".to_string()));
        // Non-relative import must not appear
        assert!(!m.depends_on.iter().any(|d| d == "react"));
    }

    // ------------------------------------------------------------------

    #[test]
    fn java_extracts_package_class_imports() {
        let tmp = tempfile::tempdir().unwrap();
        write(tmp.path(), "pom.xml", "<project/>");
        write(
            tmp.path(),
            "src/main/java/com/example/UserService.java",
            r#"
package com.example;

import java.util.List;
import com.example.model.User;
import com.example.repository.UserRepository;

public class UserService {
    public List<User> findAll() { return null; }
}
"#,
        );

        let tree = build_dep_tree(tmp.path()).unwrap();
        assert_eq!(tree.language, "java");

        let m = tree
            .modules
            .iter()
            .find(|m| m.module == "com.example.UserService")
            .unwrap();
        assert!(m.depends_on.contains(&"java.util.List".to_string()));
        assert!(m.depends_on.contains(&"com.example.model.User".to_string()));
        assert!(m
            .depends_on
            .contains(&"com.example.repository.UserRepository".to_string()));
    }

    // ------------------------------------------------------------------

    #[test]
    fn language_detection_priority() {
        let tmp = tempfile::tempdir().unwrap();
        // Both mix.exs and Cargo.toml present — Elixir wins (higher priority)
        write(tmp.path(), "mix.exs", "# mix\n");
        write(tmp.path(), "Cargo.toml", "[package]\nname=\"x\"\n");
        let lang = detect_language(tmp.path());
        assert_eq!(lang, Some(Language::Elixir));
    }

    #[test]
    fn language_detection_walks_up_parents() {
        let tmp = tempfile::tempdir().unwrap();
        // mix.exs at root, but we detect from a subdirectory
        write(tmp.path(), "mix.exs", "# mix\n");
        let sub = tmp.path().join("lib").join("my_app");
        std::fs::create_dir_all(&sub).unwrap();
        let lang = detect_language(&sub);
        assert_eq!(lang, Some(Language::Elixir));
    }

    #[test]
    fn dedup_preserves_order() {
        let v = vec![
            "b".to_string(),
            "a".to_string(),
            "b".to_string(),
            "c".to_string(),
        ];
        let result = dedup(v);
        assert_eq!(result, vec!["b", "a", "c"]);
    }
}
