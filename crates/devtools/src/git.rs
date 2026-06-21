//! Git summary: status, log, diff — structured and token-minimized.

use crate::runner::{run_cmd, truncate};
use serde::Serialize;

// ── git status ─────────────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct StatusResult {
    pub branch: String,
    pub staged: Vec<String>,
    pub modified: Vec<String>,
    pub untracked: Vec<String>,
    pub deleted: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

pub fn status(dir: &str) -> StatusResult {
    let branch_r = run_cmd("git", &["branch", "--show-current"], dir, None);
    let branch = branch_r.output.trim().to_string();

    let r = run_cmd("git", &["status", "--porcelain"], dir, None);

    if r.exit_code == -1 {
        return StatusResult {
            branch: String::new(),
            staged: vec![],
            modified: vec![],
            untracked: vec![],
            deleted: vec![],
            hint: Some("git not found on PATH.".to_string()),
        };
    }

    if r.exit_code != 0 {
        return StatusResult {
            branch: String::new(),
            staged: vec![],
            modified: vec![],
            untracked: vec![],
            deleted: vec![],
            hint: Some(
                "Not a git repository, or git command failed. Ensure you're in a valid repo."
                    .to_string(),
            ),
        };
    }

    let mut staged = Vec::new();
    let mut modified = Vec::new();
    let mut untracked = Vec::new();
    let mut deleted = Vec::new();

    for line in r.output.lines() {
        if line.len() < 4 {
            continue;
        }
        let index = line.as_bytes()[0];
        let worktree = line.as_bytes()[1];
        let file = line[3..].to_string();

        if index == b'?' {
            untracked.push(file);
        } else {
            if index != b' ' && index != b'?' {
                staged.push(file.clone());
            }
            if worktree == b'M' {
                modified.push(file.clone());
            }
            if worktree == b'D' || index == b'D' {
                deleted.push(file);
            }
        }
    }

    let hint = if !staged.is_empty() && !modified.is_empty() {
        Some("You have both staged and unstaged changes. Review with `git diff --cached` and `git diff` before committing.".to_string())
    } else {
        None
    };

    StatusResult {
        branch,
        staged,
        modified,
        untracked,
        deleted,
        hint,
    }
}

// ── git log ────────────────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct LogResult {
    pub commits: Vec<CommitEntry>,
    pub count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct CommitEntry {
    pub hash: String,
    pub short_hash: String,
    pub author: String,
    pub date: String,
    pub subject: String,
}

pub fn log(dir: &str, count: u32) -> LogResult {
    let count_str = count.to_string();
    let fmt_arg = "--format=%H|%h|%an|%ar|%s".to_string();
    let args = vec!["log", "--oneline", &fmt_arg, "-n", &count_str];
    let r = run_cmd("git", &args, dir, None);

    if r.exit_code != 0 {
        return LogResult {
            commits: vec![],
            count: 0,
            hint: Some(
                "Git log failed. Ensure you're in a git repository with at least one commit."
                    .to_string(),
            ),
        };
    }

    let commits: Vec<CommitEntry> = r
        .output
        .lines()
        .filter_map(|line| {
            let parts: Vec<&str> = line.splitn(5, '|').collect();
            if parts.len() == 5 {
                Some(CommitEntry {
                    hash: parts[0].to_string(),
                    short_hash: parts[1].to_string(),
                    author: parts[2].to_string(),
                    date: parts[3].to_string(),
                    subject: parts[4].to_string(),
                })
            } else {
                None
            }
        })
        .collect();

    let count = commits.len();
    LogResult {
        commits,
        count,
        hint: None,
    }
}

// ── git diff ───────────────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct DiffResult {
    pub staged_summary: String,
    pub unstaged_summary: String,
    pub diff: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

pub fn diff(dir: &str) -> DiffResult {
    let staged = run_cmd("git", &["diff", "--cached", "--stat"], dir, None);
    let unstaged = run_cmd("git", &["diff", "--stat"], dir, None);
    let full = run_cmd("git", &["diff"], dir, None);

    let staged_summary = staged.output.trim().to_string();
    let unstaged_summary = unstaged.output.trim().to_string();
    let diff_text = truncate(full.output.trim(), 8000);

    let hint = if staged_summary.is_empty() && unstaged_summary.is_empty() {
        Some("No changes detected. Working tree is clean.".to_string())
    } else {
        None
    };

    DiffResult {
        staged_summary,
        unstaged_summary,
        diff: diff_text,
        hint,
    }
}
