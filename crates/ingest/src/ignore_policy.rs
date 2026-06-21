//! Shared ignore policy for codebase ingestion.
//!
//! Two consumers:
//!   * the filesystem walkers in [`crate::extractor`] (via [`code_walker`]), and
//!   * the rust-analyzer LSP session (via [`excluded_dir_names`] /
//!     [`rust_analyzer_ingest_options`]), which must not index build output.
//!
//! The goal is uniform behaviour: ingest never descends into — nor asks an LSP
//! to index — build-artifact / dependency-cache directories, and it honours
//! `.gitignore` even outside a git repository.

use std::path::Path;

use ignore::{DirEntry, WalkBuilder};
use serde_json::{json, Value};

/// Directory names that are build artifacts or dependency caches. These are
/// pruned from every codebase walk and excluded from LSP indexing regardless of
/// whether a `.gitignore` lists them — a fallback for repos that don't.
///
/// Dot-prefixed build dirs (`.venv`, `.next`, `.gradle`, …) are already skipped
/// by `WalkBuilder::hidden(true)`, so only the non-hidden names are listed here.
pub const BUILD_ARTIFACT_DIRS: &[&str] = &[
    "target",       // Rust / general
    "node_modules", // Node
    "dist",         // JS/TS bundlers
    "build",        // Gradle/CMake/etc.
    "out",          // Next.js / misc
    "_build",       // Elixir / Erlang
    "__pycache__",  // Python
    "venv",         // Python virtualenv (non-hidden form)
];

/// True when `entry` is a directory whose name is a known build-artifact dir.
fn is_build_artifact_dir(entry: &DirEntry) -> bool {
    entry.file_type().is_some_and(|ft| ft.is_dir())
        && entry
            .file_name()
            .to_str()
            .is_some_and(|name| BUILD_ARTIFACT_DIRS.contains(&name))
}

/// Shared walker for codebase ingestion.
///
/// Honours `.gitignore` (even outside a git repo, via `require_git(false)`), the
/// global gitignore, `.git/info/exclude`, `.ignore`, and parent-directory
/// ignores — and always prunes build-artifact directories so ingest never
/// descends into multi-gigabyte build output such as `target/`.
///
/// Returns the builder so callers can layer extra constraints (e.g.
/// `.max_depth(..)`) before calling `.build()`.
pub fn code_walker(root: &Path) -> WalkBuilder {
    let mut builder = WalkBuilder::new(root);
    builder
        .hidden(true)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .parents(true)
        .require_git(false)
        .filter_entry(|entry| !is_build_artifact_dir(entry));
    builder
}

/// Directory names to exclude from LSP indexing: the build-artifact defaults
/// plus any simple top-level directory listed in the project's `.gitignore`.
pub fn excluded_dir_names(root: &Path) -> Vec<String> {
    let mut names: Vec<String> = BUILD_ARTIFACT_DIRS
        .iter()
        .map(|s| (*s).to_string())
        .collect();
    names.extend(gitignore_top_level_dirs(root));
    names.sort();
    names.dedup();
    names
}

/// Extract simple top-level directory names from `root/.gitignore`.
///
/// Only single-component, glob-free entries are taken (e.g. `target/`,
/// `node_modules/`, `.ferrosa/`); path-scoped or glob patterns (`**/foo`,
/// `*.log`, `a/b`) are left to the walker's full gitignore matcher.
fn gitignore_top_level_dirs(root: &Path) -> Vec<String> {
    let Ok(content) = std::fs::read_to_string(root.join(".gitignore")) else {
        return Vec::new();
    };
    content
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#') && !line.starts_with('!'))
        .map(|line| line.trim_start_matches('/').trim_end_matches('/'))
        .filter(|name| {
            !name.is_empty() && !name.contains('/') && !name.contains('*') && !name.contains('?')
        })
        .map(str::to_string)
        .collect()
}

/// rust-analyzer `initializationOptions` for ingestion: keep indexing cheap and
/// robust on large repos. Build scripts, proc-macro expansion, and check-on-save
/// all compile dependencies and grind through `target/` — none of which is
/// needed to read symbols. Syntactic `documentSymbol` is unaffected; some
/// cross-file semantics are reduced, which is an acceptable trade for ingest.
pub fn rust_analyzer_ingest_options(root: &Path) -> Value {
    json!({
        "cargo": { "buildScripts": { "enable": false } },
        "procMacro": { "enable": false },
        "checkOnSave": false,
        "files": { "excludeDirs": excluded_dir_names(root) },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_cover_common_build_dirs() {
        let names = excluded_dir_names(Path::new("/nonexistent-xyz"));
        for expected in ["target", "node_modules", "dist", "__pycache__"] {
            assert!(names.contains(&expected.to_string()), "missing {expected}");
        }
    }

    #[test]
    fn gitignore_dirs_are_merged_and_globs_skipped() {
        let tmp = std::env::temp_dir().join(format!("forge-ignore-test-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(
            tmp.join(".gitignore"),
            "# comment\n.ferrosa/\ncustom_out/\n*.log\nsub/dir\n!keep\n",
        )
        .unwrap();
        let names = excluded_dir_names(&tmp);
        assert!(names.contains(&".ferrosa".to_string()));
        assert!(names.contains(&"custom_out".to_string()));
        assert!(!names.iter().any(|n| n.contains('*')));
        assert!(!names.iter().any(|n| n.contains('/')));
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn rust_analyzer_options_disable_expensive_features() {
        let opts = rust_analyzer_ingest_options(Path::new("/nonexistent-xyz"));
        assert_eq!(opts["cargo"]["buildScripts"]["enable"], json!(false));
        assert_eq!(opts["procMacro"]["enable"], json!(false));
        assert_eq!(opts["checkOnSave"], json!(false));
        assert!(opts["files"]["excludeDirs"].is_array());
    }
}
