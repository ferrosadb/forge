//! Structured TODO/FIXME/HACK inventory extraction.
//!
//! Walks a directory (respecting `.gitignore`) and scans each text file for
//! debt-comment tokens (TODO, FIXME, HACK, XXX, BUG, NOTE, OPTIMIZE,
//! DEPRECATED). Optionally attaches `git blame` author + commit SHA per
//! finding and computes staleness buckets.
//!
//! Produces a deterministic structured report suitable for consumption by
//! `code-audit`, `complexity-audit`, and `refactor` skills.

use anyhow::Result;
use chrono::{TimeZone, Utc};
use ignore::WalkBuilder;
use regex::Regex;
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

// ---------------------------------------------------------------------------
// Public API types
// ---------------------------------------------------------------------------

/// Options controlling the extraction run.
#[derive(Debug, Clone)]
pub struct Options {
    /// Attach `git blame` author + commit SHA per finding.
    pub blame: bool,
    /// Restrict kinds considered; empty = all default kinds.
    pub kinds: Vec<String>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            blame: true,
            kinds: Vec::new(),
        }
    }
}

/// A single debt-comment finding.
#[derive(Debug, Serialize, PartialEq)]
pub struct Finding {
    pub kind: String,
    pub file: String,
    pub line: usize,
    /// Extracted text body after the token.
    pub text: String,
    /// Entire source line as-is.
    pub full_line: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub age_days: Option<i64>,
    /// Whether this finding looks like real, actionable debt. `false` when the
    /// match comes from an obvious non-debt context (test fixtures, the
    /// detector's own keyword list, a string literal, etc.). Conservative:
    /// findings are flagged, never dropped, so nothing is silently hidden.
    pub actionable: bool,
    /// If `actionable == false`, a short stable reason code explaining why
    /// (e.g. `"test-path"`, `"fixture-file"`, `"detector-source"`,
    /// `"string-literal"`). `None` when the finding is actionable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exclude_reason: Option<String>,
}

/// Staleness bucket counts (by age in days).
#[derive(Debug, Serialize, Default, PartialEq)]
pub struct Staleness {
    #[serde(rename = "0-30")]
    pub bucket_0_30: usize,
    #[serde(rename = "31-90")]
    pub bucket_31_90: usize,
    #[serde(rename = "91-365")]
    pub bucket_91_365: usize,
    #[serde(rename = "365+")]
    pub bucket_365_plus: usize,
}

/// Top-level report returned by [`extract`].
#[derive(Debug, Serialize)]
pub struct Report {
    pub path: PathBuf,
    pub files_scanned: usize,
    pub findings: Vec<Finding>,
    /// Count of findings with `actionable == true`. Skills like `/roadmap`
    /// should drive prioritization off this rather than `findings.len()`, which
    /// includes flagged fixtures/string-literals/detector-source matches.
    pub actionable_count: usize,
    pub summary: BTreeMap<String, usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub oldest_days: Option<i64>,
    pub staleness: Staleness,
}

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// Default debt-comment tokens recognized by v1 scans.
pub const DEFAULT_KINDS: &[&str] = &[
    "TODO",
    "FIXME",
    "HACK",
    "XXX",
    "BUG",
    "NOTE",
    "OPTIMIZE",
    "DEPRECATED",
];

/// Extract debt-comment findings under `path`.
pub fn extract(path: &Path, opts: &Options) -> Result<Report> {
    let kinds: Vec<String> = if opts.kinds.is_empty() {
        DEFAULT_KINDS.iter().map(|s| s.to_string()).collect()
    } else {
        opts.kinds.clone()
    };
    let kind_set: std::collections::HashSet<String> = kinds.iter().cloned().collect();

    let token_re = build_token_regex();

    let mut findings = Vec::new();
    let mut files_scanned = 0usize;

    let walker = WalkBuilder::new(path).standard_filters(true).build();

    for entry in walker.flatten() {
        let p = entry.path();
        if !p.is_file() {
            continue;
        }
        let Ok(content) = std::fs::read_to_string(p) else {
            // Binary or non-UTF-8 — skip silently (not an error condition).
            continue;
        };
        files_scanned += 1;

        for (idx, line) in content.lines().enumerate() {
            let Some(cap) = token_re.captures(line) else {
                continue;
            };
            let kind = cap
                .get(1)
                .map(|m| m.as_str().to_string())
                .unwrap_or_default();
            if !kind_set.contains(&kind) {
                continue;
            }
            let text = cap
                .get(2)
                .map(|m| m.as_str().trim().to_string())
                .unwrap_or_default();
            let file_display = p.to_string_lossy().to_string();
            let line_num = idx + 1;

            let (author, commit, age_days) = if opts.blame {
                blame_line(p, line_num).unwrap_or((None, None, None))
            } else {
                (None, None, None)
            };

            let exclude_reason = classify_exclusion(&file_display, line, &kind);

            findings.push(Finding {
                kind,
                file: file_display,
                line: line_num,
                text,
                full_line: line.to_string(),
                author,
                commit,
                age_days,
                actionable: exclude_reason.is_none(),
                exclude_reason,
            });
        }
    }

    let mut summary: BTreeMap<String, usize> = BTreeMap::new();
    for f in &findings {
        *summary.entry(f.kind.clone()).or_insert(0) += 1;
    }
    let actionable_count = findings.iter().filter(|f| f.actionable).count();

    let mut staleness = Staleness::default();
    let mut oldest: Option<i64> = None;
    for f in &findings {
        if let Some(age) = f.age_days {
            oldest = Some(oldest.map_or(age, |o| o.max(age)));
            bucket_age(&mut staleness, age);
        }
    }

    Ok(Report {
        path: path.to_path_buf(),
        files_scanned,
        findings,
        actionable_count,
        summary,
        oldest_days: oldest,
        staleness,
    })
}

// ---------------------------------------------------------------------------
// Private helpers
// ---------------------------------------------------------------------------

/// Build the canonical debt-comment token regex.
///
/// Matches when a token appears as its own word preceded by line/block
/// comment markers or whitespace. Requires the token to be uppercase so
/// prose references to "todo" are ignored.
fn build_token_regex() -> Regex {
    // Allowed prefixes: start, whitespace, //, #, --, /*, *, ;, <!--
    Regex::new(
        r"(?:^|\s|//|#|--|/\*|\*|;|<!--)\b(TODO|FIXME|HACK|XXX|BUG|NOTE|OPTIMIZE|DEPRECATED)\b[:\s]*(.*)",
    )
    .expect("built-in token regex is valid")
}

/// Decide whether a finding is non-actionable (a false positive / non-debt
/// context) and, if so, return a stable reason code. Returns `None` for
/// findings that look like real debt.
///
/// The heuristic is intentionally conservative — it flags, it never drops —
/// and covers the dominant noise sources observed in live scans:
///
/// * `"test-path"` — file lives under a `tests/`-style directory, or is itself
///   a test file (`*_test.*`, `test_*`, `*.test.*`, `*_spec.*`). Test code
///   routinely contains sample TODOs that are not project debt.
/// * `"fixture-file"` — file name contains `fixture` (sample data files that
///   deliberately embed debt tokens).
/// * `"detector-source"` — file lives under a `todo-extract/` path, i.e. this
///   scanner's own source, whose keyword lists and docs mention every token.
///
/// String-literal detection is intentionally *not* a separate heuristic here.
/// The token regex ([`build_token_regex`]) only matches when a token is
/// preceded by a comment marker or whitespace, so a directly-quoted token
/// (`"TODO"`, `'FIXME'`) — the dominant keyword-list / regex false positive —
/// is already rejected upstream and never becomes a finding. Full lexer-grade
/// "is this byte inside a string literal" detection is out of scope (it needs
/// per-language tokenization and risks dropping real debt in commented-out
/// code); see the CHANGELOG limitation note. `line` and `_kind` are kept for
/// future per-line / per-kind tuning.
fn classify_exclusion(file: &str, line: &str, _kind: &str) -> Option<String> {
    let _ = line;
    let norm = file.replace('\\', "/");

    // Path-based excludes (most reliable signal).
    if path_is_test(&norm) {
        return Some("test-path".to_string());
    }
    if file_name_contains(&norm, "fixture") {
        return Some("fixture-file".to_string());
    }
    if norm.contains("todo-extract/") {
        return Some("detector-source".to_string());
    }

    None
}

/// True if the path looks like test code: a `tests/`/`test/`/`__tests__/`
/// directory segment, or a test-named file.
fn path_is_test(norm_path: &str) -> bool {
    let has_test_dir = norm_path.split('/').any(|seg| {
        seg == "tests" || seg == "test" || seg == "__tests__" || seg == "spec" || seg == "specs"
    });
    if has_test_dir {
        return true;
    }
    let name = norm_path.rsplit('/').next().unwrap_or(norm_path);
    name.starts_with("test_")
        || name.ends_with("_test.rs")
        || name.ends_with("_test.go")
        || name.ends_with("_test.py")
        || name.contains(".test.")
        || name.contains(".spec.")
        || name.ends_with("_spec.rb")
}

/// True if the basename contains `needle` (case-insensitive).
fn file_name_contains(norm_path: &str, needle: &str) -> bool {
    let name = norm_path.rsplit('/').next().unwrap_or(norm_path);
    name.to_ascii_lowercase().contains(needle)
}

fn bucket_age(s: &mut Staleness, age: i64) {
    match age {
        a if a <= 30 => s.bucket_0_30 += 1,
        a if a <= 90 => s.bucket_31_90 += 1,
        a if a <= 365 => s.bucket_91_365 += 1,
        _ => s.bucket_365_plus += 1,
    }
}

/// Run `git blame --porcelain` for a single line; parse author email,
/// short commit SHA, and author timestamp. Returns all-None if git fails
/// (not a repo, file untracked, command missing).
fn blame_line(file: &Path, line: usize) -> Option<(Option<String>, Option<String>, Option<i64>)> {
    let parent = file.parent()?;
    let filename = file.file_name()?;
    let range = format!("{line},{line}");

    let output = Command::new("git")
        .arg("blame")
        .arg("--porcelain")
        .arg("-L")
        .arg(&range)
        .arg(filename)
        .current_dir(parent)
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let mut author_mail: Option<String> = None;
    let mut author_time: Option<i64> = None;
    let mut commit: Option<String> = None;

    for (i, raw) in text.lines().enumerate() {
        if i == 0 {
            // First line: "<sha> <orig-line> <final-line> <num-lines>"
            if let Some(sha) = raw.split_whitespace().next() {
                if sha.len() >= 7 {
                    commit = Some(sha[..7].to_string());
                }
            }
            continue;
        }
        if let Some(rest) = raw.strip_prefix("author-mail ") {
            author_mail = Some(rest.trim_matches(|c| c == '<' || c == '>').to_string());
        } else if let Some(rest) = raw.strip_prefix("author-time ") {
            author_time = rest.trim().parse::<i64>().ok();
        }
    }

    let age_days = author_time.map(|ts| {
        let then = Utc.timestamp_opt(ts, 0).single().unwrap_or_else(Utc::now);
        let now = Utc::now();
        (now - then).num_days()
    });

    Some((author_mail, commit, age_days))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn write(dir: &Path, rel: &str, body: &str) {
        let p = dir.join(rel);
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(p, body).unwrap();
    }

    #[test]
    fn token_regex_matches_uppercase_tokens() {
        let re = build_token_regex();
        assert!(re.is_match("// TODO: fix this"));
        assert!(re.is_match("# FIXME handle edge"));
        assert!(re.is_match("-- HACK"));
        assert!(re.is_match("/* TODO: block */"));
        assert!(re.is_match(" * XXX broken"));
        assert!(re.is_match("    // BUG: off by one"));
    }

    #[test]
    fn token_regex_ignores_lowercase_prose() {
        let re = build_token_regex();
        assert!(!re.is_match("I have a todo list"));
        assert!(!re.is_match("things to do later"));
    }

    #[test]
    fn extract_finds_markers_across_languages() {
        let tmp = TempDir::new().unwrap();
        write(
            tmp.path(),
            "a.rs",
            "fn main() {\n    // TODO: rust thing\n}\n",
        );
        write(tmp.path(), "b.py", "# FIXME: py thing\ndef f(): pass\n");
        write(tmp.path(), "c.js", "// HACK: js thing\nfunction f() {}\n");
        write(
            tmp.path(),
            "d.ex",
            "defmodule D do\n  # XXX: ex thing\n  def f, do: :ok\nend\n",
        );

        let opts = Options {
            blame: false,
            kinds: Vec::new(),
        };
        let report = extract(tmp.path(), &opts).unwrap();

        assert_eq!(report.findings.len(), 4);
        assert_eq!(report.files_scanned, 4);
        assert_eq!(report.summary.get("TODO").copied(), Some(1));
        assert_eq!(report.summary.get("FIXME").copied(), Some(1));
        assert_eq!(report.summary.get("HACK").copied(), Some(1));
        assert_eq!(report.summary.get("XXX").copied(), Some(1));
    }

    #[test]
    fn extract_captures_text_body() {
        let tmp = TempDir::new().unwrap();
        write(
            tmp.path(),
            "a.rs",
            "fn main() {\n    // FIXME: handle escape characters in raw strings\n}\n",
        );
        let report = extract(
            tmp.path(),
            &Options {
                blame: false,
                kinds: Vec::new(),
            },
        )
        .unwrap();
        assert_eq!(report.findings.len(), 1);
        let f = &report.findings[0];
        assert_eq!(f.kind, "FIXME");
        assert_eq!(f.text, "handle escape characters in raw strings");
        assert_eq!(f.line, 2);
        assert!(f.full_line.contains("FIXME"));
    }

    #[test]
    fn kind_filter_restricts_results() {
        let tmp = TempDir::new().unwrap();
        write(
            tmp.path(),
            "a.rs",
            "// TODO: one\n// FIXME: two\n// HACK: three\n",
        );
        let opts = Options {
            blame: false,
            kinds: vec!["FIXME".to_string()],
        };
        let report = extract(tmp.path(), &opts).unwrap();
        assert_eq!(report.findings.len(), 1);
        assert_eq!(report.findings[0].kind, "FIXME");
    }

    #[test]
    fn staleness_buckets_by_age() {
        let mut s = Staleness::default();
        bucket_age(&mut s, 0);
        bucket_age(&mut s, 15);
        bucket_age(&mut s, 45);
        bucket_age(&mut s, 200);
        bucket_age(&mut s, 800);
        assert_eq!(s.bucket_0_30, 2);
        assert_eq!(s.bucket_31_90, 1);
        assert_eq!(s.bucket_91_365, 1);
        assert_eq!(s.bucket_365_plus, 1);
    }

    #[test]
    fn blame_captures_author_when_git_repo_present() {
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path();

        // Initialize a throwaway git repo.
        let git = |args: &[&str]| {
            Command::new("git")
                .args(args)
                .current_dir(repo)
                .output()
                .expect("git command runs")
        };
        if !git(&["init", "-q"]).status.success() {
            eprintln!("git not available; skipping blame test");
            return;
        }
        let _ = git(&["config", "user.email", "test@example.com"]);
        let _ = git(&["config", "user.name", "Test"]);
        let _ = git(&["config", "commit.gpgsign", "false"]);

        write(repo, "a.rs", "fn main() {\n    // TODO: blame me\n}\n");
        let _ = git(&["add", "a.rs"]);
        let out = git(&["commit", "-q", "-m", "initial"]);
        if !out.status.success() {
            eprintln!(
                "git commit failed; skipping blame test: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            return;
        }

        let report = extract(
            repo,
            &Options {
                blame: true,
                kinds: Vec::new(),
            },
        )
        .unwrap();
        assert_eq!(report.findings.len(), 1);
        let f = &report.findings[0];
        assert_eq!(
            f.author.as_deref(),
            Some("test@example.com"),
            "expected blame author captured, got {:?}",
            f.author
        );
        assert!(f.commit.as_deref().is_some_and(|c| c.len() == 7));
        assert!(f.age_days.is_some());
    }

    // === Actionability filtering (task t_129ba347) ===

    #[test]
    fn real_todo_in_source_is_actionable() {
        let tmp = TempDir::new().unwrap();
        write(
            tmp.path(),
            "src/app.rs",
            "fn main() {\n    // TODO: wire up retry logic\n}\n",
        );
        let report = extract(
            tmp.path(),
            &Options {
                blame: false,
                kinds: Vec::new(),
            },
        )
        .unwrap();
        assert_eq!(report.findings.len(), 1);
        let f = &report.findings[0];
        assert!(
            f.actionable,
            "a real TODO in a source comment should be actionable"
        );
        assert!(f.exclude_reason.is_none());
        assert_eq!(report.actionable_count, 1);
    }

    #[test]
    fn todo_in_tests_dir_is_not_actionable() {
        let tmp = TempDir::new().unwrap();
        write(
            tmp.path(),
            "tests/it_works.rs",
            "// TODO: not real debt, a fixture\n",
        );
        let report = extract(
            tmp.path(),
            &Options {
                blame: false,
                kinds: Vec::new(),
            },
        )
        .unwrap();
        assert_eq!(report.findings.len(), 1);
        let f = &report.findings[0];
        assert!(
            !f.actionable,
            "TODO under tests/ should be flagged non-actionable"
        );
        assert_eq!(f.exclude_reason.as_deref(), Some("test-path"));
        assert_eq!(report.actionable_count, 0);
    }

    #[test]
    fn todo_in_fixture_file_is_not_actionable() {
        let tmp = TempDir::new().unwrap();
        write(
            tmp.path(),
            "src/todo_fixture.rs",
            "// TODO: sample debt comment\n",
        );
        let report = extract(
            tmp.path(),
            &Options {
                blame: false,
                kinds: Vec::new(),
            },
        )
        .unwrap();
        let f = &report.findings[0];
        assert!(!f.actionable);
        assert_eq!(f.exclude_reason.as_deref(), Some("fixture-file"));
    }

    #[test]
    fn token_in_detectors_own_source_is_not_actionable() {
        let tmp = TempDir::new().unwrap();
        // Simulate the detector's own crate path. The comment-shaped token here
        // would normally be actionable; the path exclude must win.
        write(
            tmp.path(),
            "crates/todo-extract/src/lib.rs",
            "// TODO: this scanner mentions every token\n",
        );
        let report = extract(
            tmp.path(),
            &Options {
                blame: false,
                kinds: Vec::new(),
            },
        )
        .unwrap();
        assert!(
            report.findings.iter().all(|f| !f.actionable),
            "findings inside todo-extract/ source must be non-actionable"
        );
        assert_eq!(
            report.findings[0].exclude_reason.as_deref(),
            Some("detector-source")
        );
    }

    #[test]
    fn directly_quoted_tokens_never_become_findings() {
        // The classic keyword-list / regex false positive: tokens used as
        // quoted data. The token regex only matches after a comment marker or
        // whitespace, so a directly-quoted token (`"TODO"`) is rejected upstream
        // and never reaches the finding list at all — this is the documented
        // substitute for a dedicated string-literal heuristic.
        let tmp = TempDir::new().unwrap();
        write(
            tmp.path(),
            "src/scanner.rs",
            "let kinds = [\"TODO\", \"FIXME\", \"HACK\"];\n",
        );
        let report = extract(
            tmp.path(),
            &Options {
                blame: false,
                kinds: Vec::new(),
            },
        )
        .unwrap();
        assert_eq!(
            report.findings.len(),
            0,
            "directly-quoted tokens must not be reported as debt, got {:?}",
            report.findings
        );
    }

    #[test]
    fn actionable_count_excludes_flagged_findings() {
        let tmp = TempDir::new().unwrap();
        write(tmp.path(), "src/real.rs", "// TODO: real debt\n");
        write(tmp.path(), "tests/t.rs", "// TODO: test debt\n");
        let report = extract(
            tmp.path(),
            &Options {
                blame: false,
                kinds: Vec::new(),
            },
        )
        .unwrap();
        assert_eq!(report.findings.len(), 2);
        assert_eq!(report.actionable_count, 1);
    }

    #[test]
    fn blame_absent_when_not_a_git_repo() {
        let tmp = TempDir::new().unwrap();
        write(tmp.path(), "a.rs", "// TODO: no git here\n");
        let report = extract(
            tmp.path(),
            &Options {
                blame: true,
                kinds: Vec::new(),
            },
        )
        .unwrap();
        assert_eq!(report.findings.len(), 1);
        assert!(report.findings[0].author.is_none());
        assert!(report.findings[0].commit.is_none());
    }
}
