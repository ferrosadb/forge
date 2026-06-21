//! Resolve a user-supplied path to the enclosing project root.
//!
//! The ingest extractor only accepts directories anchored by a project
//! manifest (`Cargo.toml`, `mix.exs`, `*.csproj`). When a user points at
//! a single file (e.g. `src/foo.rs`), we walk upward to find the
//! nearest manifest. A Rust workspace root (`[workspace]` in Cargo.toml)
//! is preferred over a contained single crate so extractor picks up the
//! full workspace graph.

use anyhow::{bail, Result};
use std::path::{Path, PathBuf};

/// Walk from `path` upward looking for a project manifest and return the
/// directory containing it. If `path` is already a directory and itself
/// contains a manifest, returns it unchanged.
pub fn resolve(path: &Path) -> Result<PathBuf> {
    // Canonicalize up front so our parent-walk doesn't hit "" and then
    // resolve it back to CWD, returning an empty path (regression guard).
    let canon = path
        .canonicalize()
        .map_err(|e| anyhow::anyhow!("cannot access '{}': {e}", path.display()))?;
    let start = if canon.is_file() {
        canon.parent().unwrap_or(&canon).to_path_buf()
    } else {
        canon.clone()
    };

    // Walk upward collecting candidate manifest dirs. The outermost
    // Cargo.toml marked `[workspace]` wins; otherwise the innermost
    // manifest-bearing dir wins.
    let mut innermost: Option<PathBuf> = None;
    let mut outermost_workspace: Option<PathBuf> = None;
    let mut cursor = Some(start.as_path());
    while let Some(cur) = cursor {
        if has_manifest(cur) {
            if innermost.is_none() {
                innermost = Some(cur.to_path_buf());
            }
            if is_cargo_workspace(cur) {
                outermost_workspace = Some(cur.to_path_buf());
            }
        }
        cursor = cur.parent();
    }

    if let Some(ws) = outermost_workspace {
        return Ok(ws);
    }
    if let Some(inner) = innermost {
        return Ok(inner);
    }
    bail!(
        "no project manifest (Cargo.toml / mix.exs / *.csproj) found at or above '{}'; \
         pass a project directory or a file inside one",
        path.display()
    );
}

fn has_manifest(dir: &Path) -> bool {
    dir.join("Cargo.toml").is_file() || dir.join("mix.exs").is_file() || has_csproj(dir)
}

fn has_csproj(dir: &Path) -> bool {
    let Ok(read_dir) = std::fs::read_dir(dir) else {
        return false;
    };
    read_dir
        .flatten()
        .any(|e| e.path().extension().and_then(|x| x.to_str()) == Some("csproj"))
}

fn is_cargo_workspace(dir: &Path) -> bool {
    let cargo = dir.join("Cargo.toml");
    if !cargo.is_file() {
        return false;
    }
    std::fs::read_to_string(&cargo)
        .map(|s| s.contains("[workspace]"))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn returns_unchanged_when_dir_is_rust_project_root() {
        let td = tempfile::tempdir().unwrap();
        let root = td.path();
        fs::write(
            root.join("Cargo.toml"),
            "[package]\nname=\"x\"\nversion=\"0.1.0\"",
        )
        .unwrap();
        let out = resolve(root).unwrap();
        assert_eq!(out.canonicalize().unwrap(), root.canonicalize().unwrap());
    }

    #[test]
    fn resolves_file_to_containing_rust_project() {
        let td = tempfile::tempdir().unwrap();
        let root = td.path();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(
            root.join("Cargo.toml"),
            "[package]\nname=\"x\"\nversion=\"0.1.0\"",
        )
        .unwrap();
        fs::write(root.join("src/lib.rs"), "pub fn a() {}").unwrap();

        let out = resolve(&root.join("src/lib.rs")).unwrap();
        assert_eq!(out.canonicalize().unwrap(), root.canonicalize().unwrap());
    }

    #[test]
    fn prefers_workspace_root_over_contained_crate() {
        let td = tempfile::tempdir().unwrap();
        let ws = td.path();
        fs::write(
            ws.join("Cargo.toml"),
            "[workspace]\nmembers=[\"crate-a\"]\n",
        )
        .unwrap();
        fs::create_dir_all(ws.join("crate-a/src")).unwrap();
        fs::write(
            ws.join("crate-a/Cargo.toml"),
            "[package]\nname=\"a\"\nversion=\"0.1.0\"",
        )
        .unwrap();
        fs::write(ws.join("crate-a/src/lib.rs"), "pub fn x() {}").unwrap();

        let out = resolve(&ws.join("crate-a/src/lib.rs")).unwrap();
        assert_eq!(out.canonicalize().unwrap(), ws.canonicalize().unwrap());
    }

    #[test]
    fn resolves_file_in_elixir_project() {
        let td = tempfile::tempdir().unwrap();
        let root = td.path();
        fs::create_dir_all(root.join("lib")).unwrap();
        fs::write(root.join("mix.exs"), "defmodule X.MixProject do\nend").unwrap();
        fs::write(root.join("lib/x.ex"), "defmodule X do\nend").unwrap();

        let out = resolve(&root.join("lib/x.ex")).unwrap();
        assert_eq!(out.canonicalize().unwrap(), root.canonicalize().unwrap());
    }

    #[test]
    fn relative_path_inside_workspace_still_resolves_to_workspace() {
        // Regression: walking the parent chain of a relative path used to
        // hit the CWD workspace (if any) and return an empty PathBuf.
        let td = tempfile::tempdir().unwrap();
        let ws = td.path();
        fs::write(
            ws.join("Cargo.toml"),
            "[workspace]\nmembers=[\"crate-a\"]\n",
        )
        .unwrap();
        fs::create_dir_all(ws.join("crate-a/src")).unwrap();
        fs::write(
            ws.join("crate-a/Cargo.toml"),
            "[package]\nname=\"a\"\nversion=\"0.1.0\"",
        )
        .unwrap();
        fs::write(ws.join("crate-a/src/lib.rs"), "pub fn x() {}").unwrap();

        // Use a relative path from the tempdir (simulates `frg` run from ws).
        let prev = std::env::current_dir().unwrap();
        std::env::set_current_dir(ws).unwrap();
        let result = resolve(std::path::Path::new("crate-a/src/lib.rs"));
        std::env::set_current_dir(prev).unwrap();

        let out = result.unwrap();
        assert!(
            !out.as_os_str().is_empty(),
            "resolved path must not be empty"
        );
        assert_eq!(out.canonicalize().unwrap(), ws.canonicalize().unwrap());
    }

    #[test]
    fn fails_clearly_when_no_project_found() {
        let td = tempfile::tempdir().unwrap();
        let root = td.path();
        fs::create_dir_all(root.join("nothing")).unwrap();
        fs::write(root.join("nothing/orphan.rs"), "pub fn x() {}").unwrap();

        let err = resolve(&root.join("nothing/orphan.rs")).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("no project manifest"), "msg: {msg}");
        assert!(msg.contains("orphan.rs"), "msg: {msg}");
    }
}
