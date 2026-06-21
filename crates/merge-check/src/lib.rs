//! Dry-run merge conflict detection using `git merge-tree`.
//!
//! Performs read-only merge analysis between two branches without touching
//! the working tree. Requires Git >= 2.38 for `merge-tree --write-tree`.

use anyhow::{Context, Result};
use serde::Serialize;
use std::path::Path;
use std::process::Command;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
pub struct MergeCheckResult {
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub files_merged: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub files: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub conflicting_files: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub non_conflicting_files: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auto_resolvable: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub strategies: Option<StrategyResults>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recommended_strategy: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub conflict_details: Option<Vec<ConflictDetail>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct StrategyResults {
    pub default: String,
    pub theirs: String,
    pub ours: String,
}

#[derive(Debug, Serialize)]
pub struct ConflictDetail {
    pub file: String,
    pub conflict_type: String,
    pub description: String,
}

// ---------------------------------------------------------------------------
// Convenience constructors
// ---------------------------------------------------------------------------

impl MergeCheckResult {
    fn error(msg: impl Into<String>) -> Self {
        Self {
            status: "error".into(),
            files_merged: None,
            files: None,
            conflicting_files: None,
            non_conflicting_files: None,
            auto_resolvable: None,
            strategies: None,
            recommended_strategy: None,
            conflict_details: None,
            error: Some(msg.into()),
        }
    }

    fn clean(files: Vec<String>) -> Self {
        let count = files.len();
        Self {
            status: "clean".into(),
            files_merged: Some(count),
            files: Some(files),
            conflicting_files: None,
            non_conflicting_files: None,
            auto_resolvable: None,
            strategies: None,
            recommended_strategy: None,
            conflict_details: None,
            error: None,
        }
    }

    fn conflict(
        conflicting: Vec<String>,
        non_conflicting: Vec<String>,
        details: Vec<ConflictDetail>,
        strategies: StrategyResults,
        recommended: Option<String>,
    ) -> Self {
        let auto = recommended.is_some();
        Self {
            status: "conflict".into(),
            files_merged: None,
            files: None,
            conflicting_files: Some(conflicting),
            non_conflicting_files: Some(non_conflicting),
            auto_resolvable: Some(auto),
            strategies: Some(strategies),
            recommended_strategy: recommended,
            conflict_details: Some(details),
            error: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Parse `git --version` output into (major, minor, patch).
fn git_version(dir: &Path) -> Option<(u32, u32, u32)> {
    let output = Command::new("git")
        .arg("--version")
        .current_dir(dir)
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&output.stdout);
    // "git version 2.43.0\n" — take the version token
    let version_str = text.split_whitespace().nth(2)?;
    let parts: Vec<&str> = version_str.split('.').collect();
    if parts.len() < 3 {
        return None;
    }
    Some((
        parts[0].parse().ok()?,
        parts[1].parse().ok()?,
        parts[2].parse().ok()?,
    ))
}

/// Check whether a ref (branch, tag, HEAD) exists in the repo.
fn branch_exists(dir: &Path, branch: &str) -> bool {
    Command::new("git")
        .args(["rev-parse", "--verify", branch])
        .current_dir(dir)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Parse a single CONFLICT line from `git merge-tree` output.
fn parse_conflict_line(line: &str) -> Option<ConflictDetail> {
    if !line.starts_with("CONFLICT (") {
        return None;
    }

    // Extract the type inside the parentheses: "CONFLICT (content):" → "content"
    let after_open = line.strip_prefix("CONFLICT (")?;
    let close = after_open.find(')')?;
    let raw_type = &after_open[..close];

    // Normalise type string (e.g. "modify/delete" → "modify_delete")
    let conflict_type = raw_type.replace('/', "_");

    // Extract the file name — heuristics per conflict type
    let after_paren = &after_open[close + 1..]; // "): Merge conflict in foo.rs"

    let (file, description) = match conflict_type.as_str() {
        "content" | "add_add" => {
            // "): Merge conflict in <file>"
            let file = after_paren
                .rsplit("Merge conflict in ")
                .next()
                .unwrap_or("")
                .trim();
            let desc = if conflict_type == "add_add" {
                format!("Both branches added {file} with different content")
            } else {
                format!("Both branches modified {file}")
            };
            (file.to_string(), desc)
        }
        "modify_delete" => {
            // "): <file> deleted in <branch> and modified in <branch>."
            let rest = after_paren.trim().trim_start_matches(':').trim();
            let file = rest.split_whitespace().next().unwrap_or("").to_string();
            let desc = rest.to_string();
            (file, desc)
        }
        _ => {
            // rename_rename, submodule, etc. — best-effort file extraction
            let rest = after_paren.trim().trim_start_matches(':').trim();
            let file = rest
                .rsplit("Merge conflict in ")
                .next()
                .or_else(|| rest.split_whitespace().next())
                .unwrap_or("")
                .trim()
                .to_string();
            let desc = rest.to_string();
            (file, desc)
        }
    };

    Some(ConflictDetail {
        file,
        conflict_type,
        description,
    })
}

/// Run `git merge-tree --write-tree` with an optional strategy (`theirs` / `ours`).
/// Returns `(clean, combined_stdout_stderr)`.
fn run_merge_tree(
    dir: &Path,
    target: &str,
    source: &str,
    strategy: Option<&str>,
) -> Result<(bool, String)> {
    let mut args = vec!["merge-tree", "--write-tree"];
    if let Some(s) = strategy {
        args.push("-X");
        args.push(s);
    }
    args.push(target);
    args.push(source);

    let output = Command::new("git")
        .args(&args)
        .current_dir(dir)
        .output()
        .context("Failed to run git merge-tree")?;

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let combined = format!("{stdout}{stderr}");
    Ok((output.status.success(), combined))
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Perform a read-only merge check between `source_branch` and `target_branch`
/// (defaults to `HEAD`) inside the repository at `dir`.
///
/// No changes are made to the working tree or index.
pub fn merge_check(
    dir: &Path,
    source_branch: &str,
    target_branch: Option<&str>,
) -> Result<MergeCheckResult> {
    // 1. Verify git repo
    let git_dir_check = Command::new("git")
        .args(["rev-parse", "--git-dir"])
        .current_dir(dir)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();

    match git_dir_check {
        Ok(s) if s.success() => {}
        _ => return Ok(MergeCheckResult::error("Not a git repository")),
    }

    // 2. Resolve target
    let target = target_branch.unwrap_or("HEAD");

    // 3. Verify branches exist
    if !branch_exists(dir, target) {
        return Ok(MergeCheckResult::error(format!(
            "Branch not found: {target}"
        )));
    }
    if !branch_exists(dir, source_branch) {
        return Ok(MergeCheckResult::error(format!(
            "Branch not found: {source_branch}"
        )));
    }

    // 4. Check git version
    let version = git_version(dir);
    match version {
        Some((major, minor, _)) if major > 2 || (major == 2 && minor >= 38) => {}
        Some((major, minor, patch)) => {
            return Ok(MergeCheckResult::error(format!(
                "Git >= 2.38 required for merge-tree analysis. Current version: {major}.{minor}.{patch}"
            )));
        }
        None => {
            return Ok(MergeCheckResult::error(
                "Could not determine git version".to_string(),
            ));
        }
    }

    // 5. Primary: git merge-tree
    let (clean, output) = run_merge_tree(dir, target, source_branch, None)?;

    if clean {
        // First line of stdout is the tree hash
        let tree_hash = output.lines().next().unwrap_or("").trim();

        // Get changed files by diffing target against the merged tree
        let diff_output = Command::new("git")
            .args(["diff", "--name-only", target, tree_hash])
            .current_dir(dir)
            .output()
            .context("Failed to run git diff --name-only")?;

        let files: Vec<String> = String::from_utf8_lossy(&diff_output.stdout)
            .lines()
            .filter(|l| !l.is_empty())
            .map(String::from)
            .collect();

        return Ok(MergeCheckResult::clean(files));
    }

    // Conflicts detected — parse them
    let details: Vec<ConflictDetail> = output.lines().filter_map(parse_conflict_line).collect();

    let conflicting_files: Vec<String> = details.iter().map(|d| d.file.clone()).collect();

    // Try to get the tree hash (first line) even on conflict — merge-tree still
    // outputs one, though the tree contains conflict markers.
    let tree_hash = output.lines().next().unwrap_or("").trim();

    // Get all changed files from the merge tree
    let all_files: Vec<String> = if !tree_hash.is_empty() {
        Command::new("git")
            .args(["diff", "--name-only", target, tree_hash])
            .current_dir(dir)
            .output()
            .ok()
            .map(|o| {
                String::from_utf8_lossy(&o.stdout)
                    .lines()
                    .filter(|l| !l.is_empty())
                    .map(String::from)
                    .collect()
            })
            .unwrap_or_default()
    } else {
        Vec::new()
    };

    let non_conflicting: Vec<String> = all_files
        .into_iter()
        .filter(|f| !conflicting_files.contains(f))
        .collect();

    // 6. Strategy testing
    let (theirs_clean, _) = run_merge_tree(dir, target, source_branch, Some("theirs"))?;
    let (ours_clean, _) = run_merge_tree(dir, target, source_branch, Some("ours"))?;

    let strategies = StrategyResults {
        default: "conflict".into(),
        theirs: if theirs_clean { "clean" } else { "conflict" }.into(),
        ours: if ours_clean { "clean" } else { "conflict" }.into(),
    };

    let recommended = if theirs_clean {
        Some("theirs".into())
    } else if ours_clean {
        Some("ours".into())
    } else {
        None
    };

    Ok(MergeCheckResult::conflict(
        conflicting_files,
        non_conflicting,
        details,
        strategies,
        recommended,
    ))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn setup_git_repo(dir: &Path) {
        Command::new("git")
            .args(["init"])
            .current_dir(dir)
            .output()
            .unwrap();
        Command::new("git")
            .args(["config", "user.email", "test@test.com"])
            .current_dir(dir)
            .output()
            .unwrap();
        Command::new("git")
            .args(["config", "user.name", "Test"])
            .current_dir(dir)
            .output()
            .unwrap();
        fs::write(dir.join("README.md"), "# Test\n").unwrap();
        Command::new("git")
            .args(["add", "."])
            .current_dir(dir)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "initial"])
            .current_dir(dir)
            .output()
            .unwrap();
    }

    #[test]
    fn test_clean_merge() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        setup_git_repo(dir);

        // Create branch-a: modify file-a
        Command::new("git")
            .args(["checkout", "-b", "branch-a"])
            .current_dir(dir)
            .output()
            .unwrap();
        fs::write(dir.join("file-a.txt"), "content a\n").unwrap();
        Command::new("git")
            .args(["add", "."])
            .current_dir(dir)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "add file-a"])
            .current_dir(dir)
            .output()
            .unwrap();

        // Go back to main, create branch-b: modify file-b
        Command::new("git")
            .args(["checkout", "main"])
            .current_dir(dir)
            .output()
            .unwrap()
            .status
            .success()
            .then_some(())
            // Older git may default to "master"
            .or_else(|| {
                Command::new("git")
                    .args(["checkout", "master"])
                    .current_dir(dir)
                    .output()
                    .unwrap()
                    .status
                    .success()
                    .then_some(())
            })
            .expect("Could not checkout default branch");

        Command::new("git")
            .args(["checkout", "-b", "branch-b"])
            .current_dir(dir)
            .output()
            .unwrap();
        fs::write(dir.join("file-b.txt"), "content b\n").unwrap();
        Command::new("git")
            .args(["add", "."])
            .current_dir(dir)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "add file-b"])
            .current_dir(dir)
            .output()
            .unwrap();

        // Merge check: branch-a into branch-b — should be clean
        let result = merge_check(dir, "branch-a", Some("branch-b")).unwrap();
        assert_eq!(result.status, "clean");
        assert!(result.files_merged.unwrap() > 0);
        let files = result.files.unwrap();
        assert!(files.contains(&"file-a.txt".to_string()));
    }

    #[test]
    fn test_content_conflict() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        setup_git_repo(dir);

        // Get the default branch name
        let head_out = Command::new("git")
            .args(["rev-parse", "--abbrev-ref", "HEAD"])
            .current_dir(dir)
            .output()
            .unwrap();
        let default_branch = String::from_utf8_lossy(&head_out.stdout).trim().to_string();

        // Create branch-a: modify README
        Command::new("git")
            .args(["checkout", "-b", "branch-a"])
            .current_dir(dir)
            .output()
            .unwrap();
        fs::write(dir.join("README.md"), "# Changed by A\n").unwrap();
        Command::new("git")
            .args(["add", "."])
            .current_dir(dir)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "change by a"])
            .current_dir(dir)
            .output()
            .unwrap();

        // Back to default, create branch-b: modify same file differently
        Command::new("git")
            .args(["checkout", &default_branch])
            .current_dir(dir)
            .output()
            .unwrap();
        Command::new("git")
            .args(["checkout", "-b", "branch-b"])
            .current_dir(dir)
            .output()
            .unwrap();
        fs::write(dir.join("README.md"), "# Changed by B\n").unwrap();
        Command::new("git")
            .args(["add", "."])
            .current_dir(dir)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "change by b"])
            .current_dir(dir)
            .output()
            .unwrap();

        let result = merge_check(dir, "branch-a", Some("branch-b")).unwrap();
        assert_eq!(result.status, "conflict");
        let conflicting = result.conflicting_files.unwrap();
        assert!(conflicting.contains(&"README.md".to_string()));
        assert!(result.strategies.is_some());
        // theirs/ours should resolve a single-file content conflict
        assert!(result.auto_resolvable.unwrap());
    }

    #[test]
    fn test_not_a_git_repo() {
        let tmp = TempDir::new().unwrap();
        let result = merge_check(tmp.path(), "main", None).unwrap();
        assert_eq!(result.status, "error");
        assert!(result.error.unwrap().contains("Not a git repository"));
    }

    #[test]
    fn test_branch_not_found() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        setup_git_repo(dir);

        let result = merge_check(dir, "nonexistent-branch", None).unwrap();
        assert_eq!(result.status, "error");
        assert!(result.error.unwrap().contains("Branch not found"));
    }

    #[test]
    fn test_git_version_parsing() {
        // Use the current directory — git --version doesn't need a repo
        let v = git_version(Path::new("."));
        assert!(v.is_some(), "Should be able to parse git version");
        let (major, minor, _patch) = v.unwrap();
        assert!(major >= 2, "Expected git major version >= 2");
        assert!(minor > 0, "Expected git minor version > 0");
    }

    #[test]
    fn test_result_serializes() {
        let result = MergeCheckResult {
            status: "clean".into(),
            files_merged: Some(2),
            files: Some(vec!["a.rs".into(), "b.rs".into()]),
            conflicting_files: None,
            non_conflicting_files: None,
            auto_resolvable: None,
            strategies: None,
            recommended_strategy: None,
            conflict_details: None,
            error: None,
        };

        let json = serde_json::to_string(&result).unwrap();
        assert!(json.contains("\"status\":\"clean\""));
        assert!(json.contains("\"files_merged\":2"));
        assert!(json.contains("\"a.rs\""));
        // None fields should be absent (skip_serializing_if)
        assert!(!json.contains("conflicting_files"));
        assert!(!json.contains("error"));
    }
}
