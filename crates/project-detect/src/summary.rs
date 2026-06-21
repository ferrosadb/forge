//! Project summary: file counts by extension, total LOC, module names, dependencies.

use regex::Regex;
use serde::Serialize;
use std::collections::HashMap;
use std::fs;
use std::path::Path;

#[derive(Debug, Serialize)]
pub struct ProjectSummary {
    pub total_files: usize,
    pub files_by_extension: Vec<ExtensionCount>,
    pub total_lines_of_code: usize,
    pub modules: Vec<String>,
    pub module_count: usize,
    pub dependencies: Vec<String>,
    pub dep_count: usize,
}

#[derive(Debug, Serialize)]
pub struct ExtensionCount {
    pub extension: String,
    pub count: usize,
}

/// Source extensions for LOC counting.
const SOURCE_EXTS: &[&str] = &[
    "rs", "ex", "exs", "eex", "heex", "py", "go", "ts", "tsx", "js", "jsx", "java", "cpp", "cc",
    "h", "hpp", "swift", "rb", "kt",
];

/// Directories to skip during walk.
const SKIP_DIRS: &[&str] = &[
    "_build",
    "deps",
    "node_modules",
    ".git",
    "target",
    ".elixir_ls",
    "cover",
    "__pycache__",
    ".venv",
    "vendor",
];

/// Summarize a project directory.
pub fn summarize(dir: &Path) -> ProjectSummary {
    let mut ext_counts: HashMap<String, usize> = HashMap::new();
    let mut total_files = 0usize;
    let mut total_lines_of_code = 0usize;

    let walker = ignore::WalkBuilder::new(dir)
        .hidden(false)
        .filter_entry(|e| {
            if e.file_type().map(|ft| ft.is_dir()).unwrap_or(false) {
                let name = e.file_name().to_string_lossy();
                !SKIP_DIRS.contains(&name.as_ref())
            } else {
                true
            }
        })
        .build();

    for result in walker {
        let entry = match result {
            Ok(e) => e,
            Err(_) => continue,
        };
        let ft = match entry.file_type() {
            Some(ft) => ft,
            None => continue,
        };
        if !ft.is_file() {
            continue;
        }
        total_files += 1;

        let path = entry.path();
        let ext = path
            .extension()
            .map(|e| e.to_string_lossy().to_lowercase())
            .unwrap_or_else(|| "".to_string());

        *ext_counts.entry(ext.clone()).or_insert(0) += 1;

        if SOURCE_EXTS.contains(&ext.as_str()) {
            if let Ok(content) = fs::read_to_string(path) {
                total_lines_of_code += content.lines().count();
            }
        }
    }

    // Sort by count descending, take top 15.
    let mut files_by_extension: Vec<ExtensionCount> = ext_counts
        .into_iter()
        .map(|(extension, count)| ExtensionCount { extension, count })
        .collect();
    files_by_extension.sort_by(|a, b| b.count.cmp(&a.count).then(a.extension.cmp(&b.extension)));
    files_by_extension.truncate(15);

    let modules = extract_modules(dir);
    let dependencies = extract_dependencies(dir);

    let module_count = modules.len();
    let dep_count = dependencies.len();

    ProjectSummary {
        total_files,
        files_by_extension,
        total_lines_of_code,
        modules,
        module_count,
        dependencies,
        dep_count,
    }
}

// ── Module extraction ──────────────────────────────────────────────────────

fn extract_modules(dir: &Path) -> Vec<String> {
    let mut modules = Vec::new();

    if dir.join("mix.exs").exists() {
        let re = Regex::new(r"defmodule\s+(\S+)").unwrap();
        collect_from_glob(dir, "lib", &["ex"], &re, 1, &mut modules);
    } else if dir.join("Cargo.toml").exists() {
        let re = Regex::new(r"(?:pub\s+)?mod\s+(\w+)").unwrap();
        collect_from_glob(dir, "src", &["rs"], &re, 1, &mut modules);
    } else if dir.join("go.mod").exists() {
        let re = Regex::new(r"^package\s+(\w+)").unwrap();
        let mut pkgs = Vec::new();
        collect_from_glob(dir, "", &["go"], &re, 1, &mut pkgs);
        // deduplicate
        pkgs.sort();
        pkgs.dedup();
        modules.extend(pkgs);
    } else if dir.join("package.json").exists() {
        let re = Regex::new(r"export\s+(?:class|function)\s+(\w+)").unwrap();
        collect_from_glob(
            dir,
            "src",
            &["ts", "tsx", "js", "jsx"],
            &re,
            1,
            &mut modules,
        );
    } else if dir.join("pom.xml").exists() || dir.join("build.gradle").exists() {
        let re = Regex::new(r"(?:class|interface)\s+(\w+)").unwrap();
        collect_from_glob(dir, "src", &["java"], &re, 1, &mut modules);
    } else if dir.join("pyproject.toml").exists() || dir.join("setup.py").exists() {
        let re = Regex::new(r"^(?:class|def)\s+(\w+)").unwrap();
        collect_from_glob(dir, "", &["py"], &re, 1, &mut modules);
    }

    modules.truncate(50);
    modules
}

/// Walk `base_dir/subdir` (or `base_dir` if subdir is empty) for files matching
/// `extensions`, apply `re`, capture group `capture_idx`, push into `out`.
fn collect_from_glob(
    base: &Path,
    subdir: &str,
    extensions: &[&str],
    re: &Regex,
    capture_idx: usize,
    out: &mut Vec<String>,
) {
    let root = if subdir.is_empty() {
        base.to_path_buf()
    } else {
        base.join(subdir)
    };
    if !root.exists() {
        return;
    }

    let walker = ignore::WalkBuilder::new(&root)
        .hidden(false)
        .filter_entry(|e| {
            if e.file_type().map(|ft| ft.is_dir()).unwrap_or(false) {
                let name = e.file_name().to_string_lossy();
                !SKIP_DIRS.contains(&name.as_ref())
            } else {
                true
            }
        })
        .build();

    for result in walker {
        let entry = match result {
            Ok(e) => e,
            Err(_) => continue,
        };
        if !entry.file_type().map(|ft| ft.is_file()).unwrap_or(false) {
            continue;
        }
        let path = entry.path();
        let ext = path
            .extension()
            .map(|e| e.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        if !extensions.contains(&ext.as_str()) {
            continue;
        }
        let content = match fs::read_to_string(path) {
            Ok(c) => c,
            Err(_) => continue,
        };
        for cap in re.captures_iter(&content) {
            if let Some(m) = cap.get(capture_idx) {
                out.push(m.as_str().to_string());
            }
        }
    }
}

// ── Dependency extraction ──────────────────────────────────────────────────

fn extract_dependencies(dir: &Path) -> Vec<String> {
    if dir.join("mix.exs").exists() {
        return extract_mix_deps(dir);
    }
    if dir.join("Cargo.toml").exists() {
        return extract_cargo_deps(dir);
    }
    if dir.join("go.mod").exists() {
        return extract_go_deps(dir);
    }
    if dir.join("package.json").exists() {
        return extract_npm_deps(dir);
    }
    if dir.join("pyproject.toml").exists() {
        return extract_pyproject_deps(dir);
    }
    if dir.join("pom.xml").exists() {
        return extract_pom_deps(dir);
    }
    Vec::new()
}

fn extract_mix_deps(dir: &Path) -> Vec<String> {
    let content = match fs::read_to_string(dir.join("mix.exs")) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    let re = Regex::new(r"\{:(\w+),").unwrap();
    let mut deps: Vec<String> = re
        .captures_iter(&content)
        .filter_map(|c| c.get(1))
        .map(|m| m.as_str().to_string())
        .collect();
    deps.sort();
    deps.dedup();
    deps
}

fn extract_cargo_deps(dir: &Path) -> Vec<String> {
    let content = match fs::read_to_string(dir.join("Cargo.toml")) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    // Find [dependencies] section and parse lines like `name = ...` or `name.workspace = ...`
    let re_section = Regex::new(r"^\[([^\]]+)\]").unwrap();
    let re_dep = Regex::new(r"^([\w][\w-]*)\s*[=.]").unwrap();
    let mut in_deps = false;
    let mut deps = Vec::new();
    for line in content.lines() {
        let trimmed = line.trim();
        if let Some(cap) = re_section.captures(trimmed) {
            let section = cap.get(1).unwrap().as_str().trim();
            in_deps = section == "dependencies";
            continue;
        }
        if in_deps && !trimmed.is_empty() && !trimmed.starts_with('#') {
            if let Some(cap) = re_dep.captures(trimmed) {
                deps.push(cap.get(1).unwrap().as_str().to_string());
            }
        }
    }
    deps.sort();
    deps.dedup();
    deps
}

fn extract_go_deps(dir: &Path) -> Vec<String> {
    let content = match fs::read_to_string(dir.join("go.mod")) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    // require block entries look like: `\tmodule/path v1.2.3`
    let re_block = Regex::new(r"require\s*\(([^)]*)\)").unwrap();
    let re_line = Regex::new(r"^\s*([\w./\-]+)\s+v").unwrap();
    let re_single = Regex::new(r"^require\s+([\w./\-]+)\s+v").unwrap();

    let mut deps = Vec::new();

    // Multi-line require blocks
    for cap in re_block.captures_iter(&content) {
        let block = cap.get(1).unwrap().as_str();
        for line in block.lines() {
            if let Some(m) = re_line.captures(line) {
                deps.push(m.get(1).unwrap().as_str().to_string());
            }
        }
    }

    // Single-line requires
    for line in content.lines() {
        if let Some(m) = re_single.captures(line) {
            deps.push(m.get(1).unwrap().as_str().to_string());
        }
    }

    deps.sort();
    deps.dedup();
    deps
}

fn extract_npm_deps(dir: &Path) -> Vec<String> {
    let content = match fs::read_to_string(dir.join("package.json")) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    let v: serde_json::Value = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    let mut deps = Vec::new();
    for key in &["dependencies", "devDependencies"] {
        if let Some(obj) = v.get(key).and_then(|d| d.as_object()) {
            for k in obj.keys() {
                deps.push(k.clone());
            }
        }
    }
    deps.sort();
    deps.dedup();
    deps
}

fn extract_pyproject_deps(dir: &Path) -> Vec<String> {
    let content = match fs::read_to_string(dir.join("pyproject.toml")) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    // Look for lines like `  "package>=1.0",` inside a dependencies array
    let re = Regex::new(r#"["']([A-Za-z0-9_\-]+)[>=<\[]?["']"#).unwrap();
    let in_deps_section = {
        let re_section = Regex::new(r"^\[([^\]]+)\]").unwrap();
        let mut in_dep = false;
        let mut lines_in_dep: Vec<String> = Vec::new();
        for line in content.lines() {
            if let Some(cap) = re_section.captures(line.trim()) {
                let sec = cap.get(1).unwrap().as_str();
                in_dep = sec.contains("dependencies");
                continue;
            }
            if in_dep {
                lines_in_dep.push(line.to_string());
            }
        }
        lines_in_dep.join("\n")
    };

    let mut deps: Vec<String> = re
        .captures_iter(&in_deps_section)
        .filter_map(|c| c.get(1))
        .map(|m| m.as_str().to_string())
        .collect();
    deps.sort();
    deps.dedup();
    deps
}

fn extract_pom_deps(dir: &Path) -> Vec<String> {
    let content = match fs::read_to_string(dir.join("pom.xml")) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    let re = Regex::new(r"<artifactId>([\w\-]+)</artifactId>").unwrap();
    let mut deps: Vec<String> = re
        .captures_iter(&content)
        .filter_map(|c| c.get(1))
        .map(|m| m.as_str().to_string())
        .collect();
    deps.sort();
    deps.dedup();
    deps
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn make_file(dir: &Path, rel: &str, content: &str) {
        let path = dir.join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, content).unwrap();
    }

    #[test]
    fn empty_directory_returns_zero_counts() {
        let dir = tempfile::tempdir().unwrap();
        let summary = summarize(dir.path());
        assert_eq!(summary.total_files, 0);
        assert_eq!(summary.total_lines_of_code, 0);
        assert_eq!(summary.module_count, 0);
        assert_eq!(summary.dep_count, 0);
        assert!(summary.files_by_extension.is_empty());
    }

    #[test]
    fn counts_rs_files_and_loc() {
        let dir = tempfile::tempdir().unwrap();
        make_file(
            dir.path(),
            "src/main.rs",
            "fn main() {\n    println!(\"hi\");\n}\n",
        );
        make_file(dir.path(), "src/lib.rs", "pub fn foo() {}\n");
        let summary = summarize(dir.path());
        assert_eq!(summary.total_files, 2);
        assert_eq!(summary.total_lines_of_code, 4); // 3 + 1
        assert!(summary
            .files_by_extension
            .iter()
            .any(|e| e.extension == "rs" && e.count == 2));
    }

    #[test]
    fn elixir_project_extracts_module_names() {
        let dir = tempfile::tempdir().unwrap();
        // mix.exs triggers Elixir module extraction
        make_file(dir.path(), "mix.exs", "{:phoenix, \"~> 1.7\"}");
        make_file(
            dir.path(),
            "lib/my_app.ex",
            "defmodule MyApp do\n  def hello, do: :world\nend\n",
        );
        make_file(
            dir.path(),
            "lib/my_app/router.ex",
            "defmodule MyApp.Router do\nend\n",
        );
        let summary = summarize(dir.path());
        assert!(summary.modules.contains(&"MyApp".to_string()));
        assert!(summary.modules.contains(&"MyApp.Router".to_string()));
        assert_eq!(summary.module_count, 2);
    }

    #[test]
    fn extension_counting_with_mixed_files() {
        let dir = tempfile::tempdir().unwrap();
        make_file(dir.path(), "a.rs", "fn a() {}");
        make_file(dir.path(), "b.rs", "fn b() {}");
        make_file(dir.path(), "c.py", "def c(): pass");
        make_file(dir.path(), "d.md", "# docs");
        let summary = summarize(dir.path());
        assert_eq!(summary.total_files, 4);
        let rs = summary
            .files_by_extension
            .iter()
            .find(|e| e.extension == "rs")
            .expect("rs entry");
        assert_eq!(rs.count, 2);
        let py = summary
            .files_by_extension
            .iter()
            .find(|e| e.extension == "py")
            .expect("py entry");
        assert_eq!(py.count, 1);
    }

    #[test]
    fn cargo_deps_extracted() {
        let dir = tempfile::tempdir().unwrap();
        make_file(
            dir.path(),
            "Cargo.toml",
            "[package]\nname = \"foo\"\n\n[dependencies]\nserde = \"1\"\nanyhow = \"1\"\n",
        );
        let summary = summarize(dir.path());
        assert!(summary.dependencies.contains(&"serde".to_string()));
        assert!(summary.dependencies.contains(&"anyhow".to_string()));
    }

    #[test]
    fn skips_target_directory() {
        let dir = tempfile::tempdir().unwrap();
        make_file(dir.path(), "src/main.rs", "fn main() {}\n");
        make_file(dir.path(), "target/debug/build.rs", "fn build() {}\n");
        let summary = summarize(dir.path());
        // Only src/main.rs should be counted; target/ is skipped
        assert_eq!(summary.total_files, 1);
    }
}
