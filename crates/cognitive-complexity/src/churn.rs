//! Module: Cross-correlate git churn with cognitive complexity per file.
//! Correctness: Correct when a file's hotspot score rises with both its
//! complexity and its change frequency, and unavailable churn is disclosed
//! rather than silently scored as zero.
//! Last revised: 2026-10-01
//! Last changed: Initial implementation, churn-weighted refactoring targets.

//! Complexity tells you what is hard to read; churn tells you what you keep
//! touching. A file that is both is where refactoring actually pays: you are
//! re-reading the same hard code every time you change it. A complex file that
//! nobody edits is a museum piece; a churning file that is simple is fine.
//!
//! ```no_run
//! use forge_cognitive_complexity::{analyze_path, churn, CognitiveConfig};
//!
//! let report = analyze_path("src".as_ref(), &CognitiveConfig::default())?;
//! let map = churn::collect(".".as_ref(), &churn::ChurnConfig::default())?;
//! let ranked = churn::rank_files(&report, &map);
//! # Ok::<(), anyhow::Error>(())
//! ```

use crate::report::CognitiveReport;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Tuning for the git history walk.
#[derive(Debug, Clone)]
pub struct ChurnConfig {
    /// History window passed to `git log --since`, e.g. `"12 months ago"`.
    /// `None` walks the whole history.
    pub since: Option<String>,
    /// Upper bound on commits read before stopping. A pathologically long
    /// history would otherwise dominate runtime; stopping is disclosed.
    pub max_commits: usize,
}

impl Default for ChurnConfig {
    fn default() -> Self {
        Self {
            // A year is long enough to reflect current work and short enough
            // that a long-dead file does not rank on ancient churn.
            since: Some("12 months ago".to_string()),
            max_commits: 20_000,
        }
    }
}

/// Commits per repository-relative path, plus how the walk was bounded.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ChurnMap {
    /// Repository-relative path (forward slashes) -> commits touching it.
    pub commits: BTreeMap<String, usize>,
    /// Repository root the paths are relative to.
    pub repo_root: String,
    /// History window used, if any.
    pub since: Option<String>,
    /// Commits actually examined.
    pub commits_examined: usize,
    /// True when `max_commits` cut the walk short before the window was covered.
    pub truncated: bool,
}

impl ChurnMap {
    /// Commits touching `path`, where `path` may be absolute or relative to
    /// anywhere inside the repository.
    pub fn commits_for(&self, path: &str) -> usize {
        self.commits
            .get(&self.relative_key(path))
            .copied()
            .unwrap_or(0)
    }

    /// Normalise a filesystem path to the key used in `commits`.
    ///
    /// Handles the three ways a caller can hand us a path: absolute, relative to
    /// the current directory, or already relative to the repository root. The
    /// last case is what a scan rooted at the repo produces, and missing it makes
    /// every lookup return zero churn for no visible reason.
    fn relative_key(&self, path: &str) -> String {
        let root = Path::new(&self.repo_root);
        let candidate = Path::new(path);

        let mut tries: Vec<PathBuf> = Vec::new();
        if candidate.is_absolute() {
            tries.push(candidate.to_path_buf());
        } else {
            if let Ok(cwd) = std::env::current_dir() {
                tries.push(cwd.join(candidate));
            }
            tries.push(root.join(candidate));
        }

        for absolute in tries {
            let absolute = std::fs::canonicalize(&absolute).unwrap_or(absolute);
            if let Ok(relative) = absolute.strip_prefix(root) {
                return relative
                    .components()
                    .filter_map(|c| c.as_os_str().to_str())
                    .collect::<Vec<_>>()
                    .join("/");
            }
        }

        path.to_string()
    }
}

/// One file ranked by complexity weighted by churn.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FileHotspot {
    pub file: String,
    /// Commits touching this file inside the window.
    pub commits: usize,
    /// Multiplier applied to complexity, `1 + ln(1 + commits)`.
    pub churn_factor: f64,
    /// Sum of cognitive complexity across the file's functions.
    pub cognitive_total: u32,
    /// Worst single function in the file.
    pub max_cognitive: u32,
    /// Number of functions scored in the file.
    pub functions: usize,
    /// `cognitive_total * churn_factor` — the ranking key.
    pub hotspot_score: f64,
}

/// Read git history and tally commits per file.
///
/// Returns an error when git cannot be run or the path is not a repository, so
/// a caller must choose how to disclose it. Churn is never fabricated: a file
/// missing from the map is genuinely unchanged in the window, which is
/// different from churn being unavailable.
pub fn collect(root: &Path, config: &ChurnConfig) -> Result<ChurnMap> {
    let reported_root = git(root, &["rev-parse", "--show-toplevel"])
        .context("reading the repository root")?
        .trim()
        .to_string();
    if reported_root.is_empty() {
        anyhow::bail!("git reported an empty repository root");
    }
    // Canonicalise so keys match scanned paths. On macOS a temp dir reports as
    // `/var/...` while git reports `/private/var/...`; without this every path
    // lookup misses and all churn silently reads as zero.
    let repo_root = std::fs::canonicalize(&reported_root)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or(reported_root);

    let mut args: Vec<String> = vec![
        "log".into(),
        "--no-merges".into(),
        "--numstat".into(),
        "--format=%x00%H".into(),
    ];
    if let Some(since) = &config.since {
        args.push(format!("--since={since}"));
    }
    let arg_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    let output = git(Path::new(&repo_root), &arg_refs).context("reading git history")?;

    let mut commits: BTreeMap<String, usize> = BTreeMap::new();
    let mut commits_examined = 0usize;
    let mut truncated = false;
    // Distinct paths within one commit, so a file touched twice in a squashed
    // commit still counts once.
    let mut seen_in_commit: HashSet<String> = HashSet::new();

    for line in output.lines() {
        if let Some(rest) = line.strip_prefix('\0') {
            if !rest.is_empty() {
                if commits_examined >= config.max_commits {
                    truncated = true;
                    break;
                }
                commits_examined += 1;
                // Attribute the previous commit's paths, then start the next.
                for path in seen_in_commit.drain() {
                    *commits.entry(path).or_insert(0) += 1;
                }
            }
            continue;
        }
        if let Some(path) = numstat_path(line) {
            seen_in_commit.insert(path);
        }
    }
    for path in seen_in_commit.drain() {
        *commits.entry(path).or_insert(0) += 1;
    }

    Ok(ChurnMap {
        commits,
        repo_root,
        since: config.since.clone(),
        commits_examined,
        truncated,
    })
}

/// Run git, returning stdout as text or an error carrying stderr.
fn git(dir: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .with_context(|| format!("running `git {}`", args.join(" ")))?;
    if !output.status.success() {
        anyhow::bail!(
            "`git {}` failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Extract the path from a `--numstat` line (`added\tdeleted\tpath`).
///
/// Handles binary files (`-\t-\tpath`) and renames, which git reports as
/// `old => new` or `dir/{old => new}/file`. The post-rename path is used.
fn numstat_path(line: &str) -> Option<String> {
    let mut parts = line.splitn(3, '\t');
    let added = parts.next()?;
    let _deleted = parts.next()?;
    let path = parts.next()?.trim();
    if added.is_empty() || path.is_empty() {
        return None;
    }
    Some(normalise_rename(path))
}

/// Resolve git's rename notation to the path that exists now.
fn normalise_rename(path: &str) -> String {
    if let Some(open) = path.find('{') {
        if let Some(close) = path[open..].find('}') {
            let close = open + close;
            let inner = &path[open + 1..close];
            let new_part = inner
                .rsplit_once(" => ")
                .map(|(_, new)| new.trim())
                .unwrap_or(inner);
            return format!("{}{}{}", &path[..open], new_part, &path[close + 1..]);
        }
    }
    path.rsplit_once(" => ")
        .map(|(_, new)| new.trim().to_string())
        .unwrap_or_else(|| path.to_string())
}

/// Churn multiplier: `1 + ln(1 + commits)`.
///
/// Unchanged files keep factor `1.0` — complexity is not discarded, it is simply
/// not amplified. The curve rises steeply at first and flattens, so the
/// difference between 5 and 10 commits matters more than between 100 and 105.
pub fn churn_factor(commits: usize) -> f64 {
    1.0 + ((1 + commits) as f64).ln()
}

/// Rank the files in a cognitive report by complexity weighted by churn.
///
/// Files absent from `map` are genuinely unchanged in the window; their factor
/// stays `1.0`. Only files with at least one scored function are considered.
///
/// The report's own `file` strings are used as-is; `ChurnMap::commits_for`
/// resolves them. Do NOT prefix them with the scan root here — a scan rooted deep
/// in a tree records paths relative to *that* root, and prefixing produced keys
/// that matched nothing, silently scoring every file as unchanged.
pub fn rank_files(report: &CognitiveReport, map: &ChurnMap) -> Vec<FileHotspot> {
    let mut per_file: BTreeMap<&str, (u32, u32, usize)> = BTreeMap::new();
    for function in &report.functions {
        let entry = per_file.entry(function.file.as_str()).or_insert((0, 0, 0));
        entry.0 += function.cognitive;
        entry.1 = entry.1.max(function.cognitive);
        entry.2 += 1;
    }

    let mut hotspots: Vec<FileHotspot> = per_file
        .into_iter()
        .map(|(file, (cognitive_total, max_cognitive, functions))| {
            let commits = map.commits_for(file);
            let factor = churn_factor(commits);
            FileHotspot {
                file: file.to_string(),
                commits,
                churn_factor: factor,
                cognitive_total,
                max_cognitive,
                functions,
                hotspot_score: cognitive_total as f64 * factor,
            }
        })
        .collect();

    // Worst first; ties broken deterministically.
    hotspots.sort_by(|a, b| {
        b.hotspot_score
            .partial_cmp(&a.hotspot_score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.file.cmp(&b.file))
    });
    hotspots
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::report::{band, FunctionComplexity, FunctionKind, Percentiles};
    use std::process::Command;

    fn git_in(dir: &Path, args: &[&str]) {
        let status = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@example.com")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@example.com")
            .output()
            .expect("git runs");
        assert!(
            status.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&status.stderr)
        );
    }

    fn init_repo(dir: &Path) {
        git_in(dir, &["init", "-q"]);
        git_in(dir, &["config", "user.email", "t@example.com"]);
        git_in(dir, &["config", "user.name", "t"]);
    }

    fn write_and_commit(dir: &Path, file: &str, body: &str, message: &str) {
        std::fs::write(dir.join(file), body).unwrap();
        git_in(dir, &["add", "."]);
        git_in(dir, &["commit", "-q", "-m", message]);
    }

    fn function(file: &str, name: &str, cognitive: u32) -> FunctionComplexity {
        FunctionComplexity {
            file: file.to_string(),
            name: name.to_string(),
            kind: FunctionKind::Function,
            line: 1,
            lines: 10,
            cognitive,
            nesting: 2,
            band: band(cognitive),
            is_test: false,
        }
    }

    fn report_of(functions: Vec<FunctionComplexity>) -> CognitiveReport {
        let total: u32 = functions.iter().map(|f| f.cognitive).sum();
        let max = functions.iter().map(|f| f.cognitive).max().unwrap_or(0);
        CognitiveReport {
            files_scanned: 1,
            files_analyzed: 1,
            functions_analyzed: functions.len(),
            functions_total: functions.len(),
            returned: functions.len(),
            truncated: false,
            threshold: None,
            top: None,
            total_cognitive: total,
            max_cognitive: max,
            bands: Default::default(),
            percentiles: Percentiles::default(),
            file_hotspots: None,
            churn: None,
            functions,
            failures: Vec::new(),
            warnings: Vec::new(),
        }
    }

    #[test]
    fn churn_factor_grows_and_flattens() {
        assert_eq!(churn_factor(0), 1.0, "unchanged files are not amplified");
        assert!(churn_factor(1) > churn_factor(0));
        assert!(churn_factor(10) > churn_factor(5));
        // Diminishing returns: a marginal commit matters less as churn grows.
        let early = churn_factor(6) - churn_factor(5);
        let late = churn_factor(101) - churn_factor(100);
        assert!(early > late, "curve flattens: {early} vs {late}");
    }

    #[test]
    fn numstat_parsing_handles_binary_and_renames() {
        assert_eq!(
            numstat_path("10\t2\tsrc/lib.rs").as_deref(),
            Some("src/lib.rs")
        );
        assert_eq!(
            numstat_path("-\t-\tassets/logo.png").as_deref(),
            Some("assets/logo.png")
        );
        assert_eq!(
            numstat_path("1\t1\tsrc/{old.rs => new.rs}").as_deref(),
            Some("src/new.rs")
        );
        assert_eq!(
            numstat_path("3\t0\told.rs => new.rs").as_deref(),
            Some("new.rs")
        );
        assert_eq!(numstat_path("garbage"), None);
    }

    #[test]
    fn churn_is_measured_from_real_history() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        write_and_commit(dir.path(), "hot.rs", "fn a() {}\n", "one");
        write_and_commit(dir.path(), "hot.rs", "fn a() {}\nfn b() {}\n", "two");
        write_and_commit(
            dir.path(),
            "hot.rs",
            "fn a() {}\nfn b() {}\nfn c() {}\n",
            "three",
        );
        write_and_commit(dir.path(), "cold.rs", "fn z() {}\n", "add cold");

        let map = collect(dir.path(), &ChurnConfig::default()).unwrap();
        assert_eq!(
            map.commits.get("hot.rs"),
            Some(&3),
            "hot.rs was committed 3x"
        );
        assert_eq!(map.commits.get("cold.rs"), Some(&1));
        assert_eq!(map.commits_examined, 4);
        assert!(!map.truncated);
    }

    /// The point of the correlation: equal complexity, different churn.
    #[test]
    fn churn_breaks_ties_between_equally_complex_files() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        write_and_commit(dir.path(), "hot.rs", "fn a() {}\n", "one");
        write_and_commit(dir.path(), "hot.rs", "fn a() {}\nfn b() {}\n", "two");
        write_and_commit(
            dir.path(),
            "hot.rs",
            "fn a() {}\nfn b() {}\nfn c() {}\n",
            "three",
        );
        write_and_commit(dir.path(), "cold.rs", "fn z() {}\n", "add cold");

        let report = report_of(vec![
            function("hot.rs", "hot_fn", 60),
            function("cold.rs", "cold_fn", 60),
        ]);
        let map = collect(dir.path(), &ChurnConfig::default()).unwrap();
        let ranked = rank_files(&report, &map);

        assert_eq!(ranked.len(), 2);
        assert_eq!(
            ranked[0].file, "hot.rs",
            "equal complexity, more churn wins"
        );
        assert_eq!(ranked[0].commits, 3);
        assert_eq!(ranked[1].commits, 1);
        assert!(
            ranked[0].hotspot_score > ranked[1].hotspot_score,
            "churn must separate them: {:?}",
            ranked
        );
        assert_eq!(ranked[0].cognitive_total, 60);
    }

    /// Complexity still matters more than churn: a very complex file that
    /// nobody touches must outrank a trivial file edited constantly.
    #[test]
    fn complexity_is_not_swamped_by_churn() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        write_and_commit(dir.path(), "churny.rs", "fn z() {}\n", "one");
        for i in 0..30 {
            write_and_commit(
                dir.path(),
                "churny.rs",
                &format!("fn z() {{}}\n// {i}\n"),
                &format!("edit {i}"),
            );
        }
        write_and_commit(dir.path(), "gnarly.rs", "fn g() {}\n", "add gnarly");

        let report = report_of(vec![
            function("gnarly.rs", "gnarly_fn", 400),
            function("churny.rs", "tiny_fn", 2),
        ]);
        let map = collect(dir.path(), &ChurnConfig::default()).unwrap();
        let ranked = rank_files(&report, &map);
        assert_eq!(ranked[0].file, "gnarly.rs", "complexity dominates");
    }

    /// A scan rooted at the repository records paths already relative to that
    /// root. Those must resolve; prefixing them with the scan root silently
    /// matched nothing and scored every file as unchanged.
    #[test]
    fn repo_relative_paths_resolve_without_a_root_argument() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        std::fs::create_dir_all(dir.path().join("crates/core/src")).unwrap();
        write_and_commit(
            dir.path(),
            "crates/core/src/lib.rs",
            "pub fn a() {}\n",
            "one",
        );
        write_and_commit(
            dir.path(),
            "crates/core/src/lib.rs",
            "pub fn a() {}\n// x\n",
            "two",
        );

        let map = collect(dir.path(), &ChurnConfig::default()).unwrap();
        // Exactly how a report records it when the scan root is the repo.
        let report = report_of(vec![function("crates/core/src/lib.rs", "a", 50)]);
        let ranked = rank_files(&report, &map);

        assert_eq!(
            ranked[0].commits, 2,
            "a repo-relative path must find its churn, not silently read zero"
        );
        assert!(
            ranked[0].churn_factor > 1.0,
            "factor must reflect the churn"
        );
        assert!(
            ranked[0].hotspot_score > 50.0,
            "score must be amplified: {}",
            ranked[0].hotspot_score
        );
    }

    /// The failure this guards against: churn present in the map but never
    /// matched, so every score is silently the raw complexity total.
    #[test]
    fn churn_actually_changes_the_ranking() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        for i in 0..10 {
            write_and_commit(
                dir.path(),
                "hot.rs",
                &format!("pub fn a() {{}}// {i}\n"),
                &format!("c{i}"),
            );
        }
        write_and_commit(dir.path(), "cold.rs", "pub fn b() {}\n", "once");

        let map = collect(dir.path(), &ChurnConfig::default()).unwrap();
        let report = report_of(vec![
            function("cold.rs", "b", 100),
            function("hot.rs", "a", 100),
        ]);
        let ranked = rank_files(&report, &map);

        assert!(
            ranked.iter().any(|h| h.commits > 0),
            "at least one file must show its churn: {ranked:?}"
        );
        assert_eq!(ranked[0].file, "hot.rs", "the churning file must win");
        assert_ne!(
            ranked[0].hotspot_score, ranked[1].hotspot_score,
            "identical complexity must be separated by churn"
        );
    }

    #[test]
    fn unchanged_files_keep_factor_one() {
        let report = report_of(vec![function("elsewhere.rs", "f", 50)]);
        let map = ChurnMap {
            repo_root: "/repo".to_string(),
            ..Default::default()
        };
        let ranked = rank_files(&report, &map);
        assert_eq!(ranked[0].commits, 0);
        assert_eq!(ranked[0].churn_factor, 1.0);
        assert_eq!(ranked[0].hotspot_score, 50.0);
    }

    #[test]
    fn missing_repository_is_an_error_not_fake_churn() {
        let dir = tempfile::tempdir().unwrap();
        let result = collect(dir.path(), &ChurnConfig::default());
        assert!(
            result.is_err(),
            "a non-repository must error so the caller can disclose it"
        );
    }

    #[test]
    fn max_commits_is_disclosed_when_it_bounds_the_walk() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        for i in 0..5 {
            write_and_commit(
                dir.path(),
                "a.rs",
                &format!("fn a() {{}}\n// {i}\n"),
                &format!("c{i}"),
            );
        }
        let config = ChurnConfig {
            since: None,
            max_commits: 2,
        };
        let map = collect(dir.path(), &config).unwrap();
        assert_eq!(map.commits_examined, 2);
        assert!(map.truncated, "a bounded walk must say so");
    }
}
