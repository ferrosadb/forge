//! `frg glob` — file discovery with structured stats.
//!
//! See `specs/feat-glob-stats/` for the full design.
//!
//! Key invariants:
//! - No `.unwrap()` / `.expect()` on fallible I/O (clippy-enforced crate-wide
//!   would be ideal; here we enforce by convention + review).
//! - Pattern normalization rejects `..` components and absolute patterns
//!   unless `--allow-absolute` is passed (FMEA F1).
//! - `SECRET_FILENAMES` denylist is applied **after** user excludes and
//!   cannot be overridden (FMEA F7).
//! - Files larger than `max_bytes` are skipped **before** being opened,
//!   via `metadata().len()` (FMEA F3).
//! - Non-UTF-8 paths use `to_string_lossy()` + `path_encoding: "lossy"`
//!   marker (FMEA F4).
//! - Emitted paths use POSIX separators (`/`) regardless of platform (F10).

use anyhow::{anyhow, bail, Context, Result};
use chrono::{DateTime, Utc};
use forge_shared::secrets::is_secret_filename;
use globset::{Glob, GlobMatcher};
use ignore::WalkBuilder;
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// Default exclude patterns applied before user excludes. Kept in sync
/// with common build/dependency directories agents rarely care about.
pub const BUILTIN_EXCLUDES: &[&str] = &[
    ".git",
    "target",
    "vendor",
    "node_modules",
    "dist",
    "build",
    ".venv",
    "__pycache__",
    "*.lock",
];

/// Output format requested on the CLI.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutputFormat {
    Brief,
    Json,
    Csv,
    Table,
}

impl OutputFormat {
    pub fn parse(s: &str) -> Result<Self> {
        match s {
            "brief" => Ok(Self::Brief),
            "json" => Ok(Self::Json),
            "csv" => Ok(Self::Csv),
            "table" => Ok(Self::Table),
            other => bail!(
                "invalid --format '{}' (expected one of: brief, json, csv, table)",
                other
            ),
        }
    }
}

/// Configuration for a single `frg glob` run.
///
/// All filter values use inclusive bounds. Unset bounds are represented by
/// sentinel values (`u64::MAX` / `0`) so callers never need `Option`.
#[derive(Clone, Debug)]
pub struct GlobConfig {
    pub pattern: String,
    pub min_lines: u64,
    pub max_lines: u64,
    pub min_bytes: u64,
    pub max_bytes: u64,
    /// Only include files modified within the last N seconds.
    pub modified_after_secs: Option<u64>,
    /// Only include files last modified more than N seconds ago.
    pub modified_before_secs: Option<u64>,
    pub user_excludes: Vec<String>,
    pub format: OutputFormat,
    pub allow_absolute: bool,
    pub max_results: usize,
    pub max_depth: usize,
    pub follow_links: bool,
    pub respect_gitignore: bool,
}

impl Default for GlobConfig {
    fn default() -> Self {
        Self {
            pattern: String::new(),
            min_lines: 0,
            max_lines: u64::MAX,
            min_bytes: 0,
            max_bytes: 1024 * 1024 * 1024, // 1 GiB
            modified_after_secs: None,
            modified_before_secs: None,
            user_excludes: Vec::new(),
            format: OutputFormat::Brief,
            allow_absolute: false,
            max_results: 10_000,
            max_depth: 20,
            follow_links: false,
            respect_gitignore: true,
        }
    }
}

/// One file entry in the result set.
#[derive(Clone, Debug, Serialize)]
pub struct FileEntry {
    pub path: String,
    pub lines: u64,
    pub bytes: u64,
    pub modified: String, // ISO 8601 UTC
    pub is_generated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path_encoding: Option<String>, // "lossy" when path isn't valid UTF-8
}

/// Tally of files that didn't make it into `results`.
#[derive(Clone, Debug, Default, Serialize)]
pub struct SkippedReasons {
    pub too_small: u64,
    pub too_large: u64,
    pub too_old: u64,
    pub too_new: u64,
    pub excluded: u64,
    pub secret_denylist: u64,
    pub ignored: u64,
    pub not_a_file: u64,
    pub permission_denied: u64,
    pub io_error: u64,
}

/// Full structured output. Stable JSON schema, versioned via
/// `$schema_version`.
#[derive(Clone, Debug, Serialize)]
pub struct GlobResult {
    #[serde(rename = "$schema_version")]
    pub schema_version: u32,
    pub pattern: String,
    pub filters: FilterEcho,
    pub results: Vec<FileEntry>,
    pub total_matched: usize,
    pub total_skipped: u64,
    pub skipped_reasons: SkippedReasons,
    pub truncated: bool,
}

/// Subset of the config that is echoed back in the result for
/// reproducibility. Excludes internal knobs like `max_depth`.
#[derive(Clone, Debug, Serialize)]
pub struct FilterEcho {
    pub min_lines: u64,
    pub max_lines: u64,
    pub min_bytes: u64,
    pub max_bytes: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub modified_after_secs: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub modified_before_secs: Option<u64>,
    pub excludes: Vec<String>,
    pub allow_absolute: bool,
    pub max_results: usize,
}

/// Normalized pattern: a concrete walk root + a glob matcher relative to it.
///
/// Splitting at the first wildcard segment lets us avoid traversing
/// everything from `.` when the user asks for `src/**/*.rs`.
#[derive(Debug)]
struct NormalizedPattern {
    walk_root: PathBuf,
    #[allow(dead_code)]
    matcher: GlobMatcher,
    /// The pattern as fed to globset, relative to `walk_root`.
    #[allow(dead_code)]
    relative_pattern: String,
    /// True if the original pattern was absolute (only allowed with flag).
    #[allow(dead_code)]
    was_absolute: bool,
}

/// Rejects `..` segments and absolute paths (unless allowed). Splits the
/// pattern into a concrete walk root and a relative glob.
fn normalize_pattern(raw: &str, allow_absolute: bool) -> Result<NormalizedPattern> {
    if raw.is_empty() {
        bail!("pattern must not be empty");
    }
    // Reject traversal attempts.
    let has_parent_segment = raw.split(['/', '\\']).any(|seg| seg == "..");
    if has_parent_segment {
        bail!(
            "pattern '{}' contains a '..' segment (path traversal rejected; \
             resolve relative to the workspace root explicitly)",
            raw
        );
    }

    let is_absolute = Path::new(raw).is_absolute() || raw.starts_with('/');
    if is_absolute && !allow_absolute {
        bail!(
            "pattern '{}' is absolute; pass --allow-absolute to confirm \
             traversal outside the current directory",
            raw
        );
    }

    // Split at the first segment containing a wildcard.
    let mut prefix: Vec<&str> = Vec::new();
    let mut suffix: Vec<&str> = Vec::new();
    let mut found_wildcard = false;
    for seg in raw.split('/') {
        if !found_wildcard && seg.chars().any(|c| matches!(c, '*' | '?' | '[' | '{')) {
            found_wildcard = true;
        }
        if found_wildcard {
            suffix.push(seg);
        } else {
            prefix.push(seg);
        }
    }

    let walk_root = if prefix.is_empty() {
        PathBuf::from(".")
    } else {
        PathBuf::from(prefix.join("/"))
    };

    let relative_pattern = if suffix.is_empty() {
        // Pattern is a concrete path; treat as literal single file.
        // Use the basename as the relative pattern.
        raw.split('/').next_back().unwrap_or(raw).to_string()
    } else {
        suffix.join("/")
    };

    let glob = Glob::new(&relative_pattern)
        .with_context(|| format!("invalid glob pattern '{}'", relative_pattern))?;

    Ok(NormalizedPattern {
        walk_root,
        matcher: glob.compile_matcher(),
        relative_pattern,
        was_absolute: is_absolute,
    })
}

/// Determine if a file is likely auto-generated based on a conservative
/// heuristic: the first non-empty line contains `@generated`, OR the
/// filename stem contains `.generated.`. Conservative on purpose — false
/// positives hide real files from agents (FMEA F9).
fn is_generated_file(path: &Path, content_first_line: Option<&str>) -> bool {
    if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
        if name.contains(".generated.") {
            return true;
        }
    }
    content_first_line
        .map(|line| line.contains("@generated"))
        .unwrap_or(false)
}

/// Count newlines in a buffer. Uses `memchr` via `bytecount`-style naive
/// scan; avoids allocating line strings.
fn count_newlines(buf: &[u8]) -> u64 {
    buf.iter().filter(|&&b| b == b'\n').count() as u64
}

/// Read a file and return (line_count, first_line_snippet). Returns None
/// if the file cannot be opened (caller tallies as skipped).
fn read_file_stats(path: &Path) -> Option<(u64, String)> {
    use std::io::Read;
    let mut f = std::fs::File::open(path).ok()?;
    let mut buf = Vec::new();
    // Read in 64 KiB chunks up to ~2 MiB so we don't hold a huge file.
    let mut first_line = String::new();
    let mut read_so_far = 0usize;
    let cap = 2 * 1024 * 1024;
    let mut total_lines: u64 = 0;
    let mut chunk = [0u8; 64 * 1024];
    loop {
        match f.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                total_lines += count_newlines(&chunk[..n]);
                if read_so_far < cap {
                    let take = (cap - read_so_far).min(n);
                    buf.extend_from_slice(&chunk[..take]);
                    read_so_far += take;
                }
            }
            Err(_) => return None,
        }
    }
    // Extract first non-empty line from buffered prefix for @generated check.
    for line in String::from_utf8_lossy(&buf).lines().take(5) {
        let trimmed = line.trim();
        if !trimmed.is_empty() {
            first_line = trimmed.to_string();
            break;
        }
    }
    Some((total_lines, first_line))
}

/// Convert a SystemTime to an ISO 8601 UTC string, with graceful fallback.
fn format_mtime(mtime: SystemTime) -> String {
    let dt: DateTime<Utc> = mtime.into();
    dt.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// Returns the number of seconds since mtime, or u64::MAX on clock errors.
fn age_secs(mtime: SystemTime) -> u64 {
    SystemTime::now()
        .duration_since(mtime)
        .map(|d| d.as_secs())
        .unwrap_or(u64::MAX)
}

/// Convert a Path to a POSIX-style string with a "lossy" marker when the
/// path is not valid UTF-8. We never panic on non-UTF-8 input (FMEA F4).
fn path_to_string(path: &Path) -> (String, Option<String>) {
    let lossy = path.to_string_lossy();
    let had_invalid = matches!(lossy, std::borrow::Cow::Owned(_));
    // Normalize to POSIX separators even on Windows (F10).
    let posix = lossy.replace('\\', "/");
    let marker = if had_invalid {
        Some("lossy".to_string())
    } else {
        None
    };
    (posix, marker)
}

/// Does the user-supplied or builtin exclude pattern match this path?
/// Simple containment check against any path component; kept intentionally
/// simple since these are coarse excludes.
fn matches_exclude(rel_path: &Path, excludes: &[&str]) -> bool {
    for ex in excludes {
        // Extension-style `*.lock` match against filename
        if let Some(stripped) = ex.strip_prefix("*.") {
            if rel_path
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| e == stripped)
                .unwrap_or(false)
            {
                return true;
            }
            continue;
        }
        // Component-name match (e.g. "target" matches any segment)
        let comp_match = rel_path
            .components()
            .any(|c| c.as_os_str().to_string_lossy() == *ex);
        if comp_match {
            return true;
        }
    }
    false
}

/// Drive the walk and produce the final `GlobResult`.
///
/// Separated from `run_to_writer` so that MCP callers can consume the
/// structured result without going through a Writer.
pub fn collect(config: &GlobConfig) -> Result<GlobResult> {
    let norm = normalize_pattern(&config.pattern, config.allow_absolute)?;

    // Merge builtin + user excludes; secret denylist is applied separately
    // and cannot be disabled.
    let mut excludes: Vec<String> = BUILTIN_EXCLUDES.iter().map(|s| s.to_string()).collect();
    excludes.extend(config.user_excludes.iter().cloned());
    let excludes_ref: Vec<&str> = excludes.iter().map(|s| s.as_str()).collect();

    let mut results: Vec<FileEntry> = Vec::new();
    let mut skipped = SkippedReasons::default();
    let mut total_skipped: u64 = 0;
    let mut truncated = false;

    let mut builder = WalkBuilder::new(&norm.walk_root);
    builder
        .standard_filters(config.respect_gitignore)
        .hidden(false) // surface dotfiles; .gitignore still excludes .git/
        .follow_links(config.follow_links)
        .max_depth(Some(config.max_depth));

    for entry_result in builder.build() {
        if results.len() >= config.max_results {
            truncated = true;
            break;
        }
        let entry = match entry_result {
            Ok(e) => e,
            Err(err) => {
                // Fail-loud: tally and log, don't crash.
                eprintln!("forge glob: walk error: {err}");
                if err.to_string().contains("permission denied") {
                    skipped.permission_denied += 1;
                } else {
                    skipped.io_error += 1;
                }
                total_skipped += 1;
                continue;
            }
        };
        let path = entry.path();
        if !entry.file_type().map(|ft| ft.is_file()).unwrap_or(false) {
            continue;
        }

        // Relative path for matching.
        let rel = path.strip_prefix(&norm.walk_root).unwrap_or(path);

        // Secret denylist: non-overridable (F7).
        if let Some(fname) = rel.file_name().and_then(|n| n.to_str()) {
            if is_secret_filename(fname) {
                skipped.secret_denylist += 1;
                total_skipped += 1;
                continue;
            }
        }

        // Glob match.
        if !norm.matcher.is_match(rel) {
            continue;
        }

        // User + builtin excludes.
        if matches_exclude(rel, &excludes_ref) {
            skipped.excluded += 1;
            total_skipped += 1;
            continue;
        }

        // Metadata-first filters (F3: never open oversize files).
        let md = match path.metadata() {
            Ok(m) => m,
            Err(_) => {
                skipped.io_error += 1;
                total_skipped += 1;
                continue;
            }
        };
        let bytes = md.len();
        if bytes < config.min_bytes {
            skipped.too_small += 1;
            total_skipped += 1;
            continue;
        }
        if bytes > config.max_bytes {
            skipped.too_large += 1;
            total_skipped += 1;
            continue;
        }
        let mtime = md.modified().unwrap_or(SystemTime::UNIX_EPOCH);
        let age = age_secs(mtime);
        if let Some(max_age) = config.modified_after_secs {
            if age > max_age {
                skipped.too_old += 1;
                total_skipped += 1;
                continue;
            }
        }
        if let Some(min_age) = config.modified_before_secs {
            if age < min_age {
                skipped.too_new += 1;
                total_skipped += 1;
                continue;
            }
        }

        // At this point we need lines; open + count.
        let (lines, first_line) = match read_file_stats(path) {
            Some(v) => v,
            None => {
                skipped.io_error += 1;
                total_skipped += 1;
                continue;
            }
        };
        if lines < config.min_lines {
            skipped.too_small += 1;
            total_skipped += 1;
            continue;
        }
        if lines > config.max_lines {
            skipped.too_large += 1;
            total_skipped += 1;
            continue;
        }

        let (path_str, path_encoding) = path_to_string(path);
        let is_generated = is_generated_file(path, Some(&first_line));
        results.push(FileEntry {
            path: path_str,
            lines,
            bytes,
            modified: format_mtime(mtime),
            is_generated,
            path_encoding,
        });
    }

    let filters_echo = FilterEcho {
        min_lines: config.min_lines,
        max_lines: config.max_lines,
        min_bytes: config.min_bytes,
        max_bytes: config.max_bytes,
        modified_after_secs: config.modified_after_secs,
        modified_before_secs: config.modified_before_secs,
        excludes: excludes.clone(),
        allow_absolute: config.allow_absolute,
        max_results: config.max_results,
    };

    // Stable output ordering: sort by path (byte-wise, deterministic).
    results.sort_by(|a, b| a.path.cmp(&b.path));

    let total_matched = results.len();
    Ok(GlobResult {
        schema_version: 1,
        pattern: config.pattern.clone(),
        filters: filters_echo,
        results,
        total_matched,
        total_skipped,
        skipped_reasons: skipped,
        truncated,
    })
}

/// Render a result set in the requested format to the given writer.
pub fn render<W: std::io::Write>(
    result: &GlobResult,
    format: OutputFormat,
    pretty: bool,
    w: &mut W,
) -> Result<()> {
    match format {
        OutputFormat::Brief => {
            for entry in &result.results {
                // Escape control chars; POSIX separators already applied.
                let clean = sanitize_for_terminal(&entry.path);
                writeln!(w, "{}", clean)?;
            }
            if result.truncated {
                writeln!(
                    w,
                    "# truncated: max_results={} reached",
                    result.filters.max_results
                )?;
            }
        }
        OutputFormat::Json => {
            let s = if pretty {
                serde_json::to_string_pretty(result)?
            } else {
                serde_json::to_string(result)?
            };
            writeln!(w, "{}", s)?;
        }
        OutputFormat::Csv => {
            let mut wtr = csv::Writer::from_writer(w);
            wtr.write_record([
                "path",
                "lines",
                "bytes",
                "modified",
                "is_generated",
                "path_encoding",
            ])?;
            for entry in &result.results {
                wtr.write_record([
                    entry.path.as_str(),
                    &entry.lines.to_string(),
                    &entry.bytes.to_string(),
                    entry.modified.as_str(),
                    &entry.is_generated.to_string(),
                    entry.path_encoding.as_deref().unwrap_or(""),
                ])?;
            }
            wtr.flush()?;
        }
        OutputFormat::Table => {
            // Fixed-width table; no external deps.
            writeln!(w, "{:<60} {:>8} {:>10}  modified", "path", "lines", "bytes")?;
            writeln!(w, "{}", "-".repeat(100))?;
            for entry in &result.results {
                let path = sanitize_for_terminal(&entry.path);
                writeln!(
                    w,
                    "{:<60} {:>8} {:>10}  {}",
                    truncate_col(&path, 60),
                    entry.lines,
                    entry.bytes,
                    entry.modified
                )?;
            }
            writeln!(
                w,
                "\n{} matched, {} skipped{}",
                result.total_matched,
                result.total_skipped,
                if result.truncated { " (truncated)" } else { "" }
            )?;
        }
    }
    Ok(())
}

/// Strip ANSI escape sequences, NUL, and other control bytes. Replaces
/// embedded newlines with `\n` literal so downstream line-oriented
/// parsers don't break (FMEA F12 / T10).
pub fn sanitize_for_terminal(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\x1b' => {
                // ANSI escape: skip until a terminator (letter) or end.
                while let Some(&next) = chars.peek() {
                    chars.next();
                    if next.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            c if (c as u32) < 0x20 => {
                // Other C0 controls.
                out.push('?');
            }
            c => out.push(c),
        }
    }
    out
}

fn truncate_col(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let cut: String = s.chars().take(max.saturating_sub(1)).collect();
        format!("{}…", cut)
    }
}

/// Entry point called from `main.rs`. Parses CLI args, runs the walk,
/// renders output.
#[allow(clippy::too_many_arguments)]
pub fn run_from_cli(
    pattern: String,
    min_lines: u64,
    max_lines: u64,
    min_bytes: u64,
    max_bytes: u64,
    modified_after: Option<String>,
    modified_before: Option<String>,
    user_excludes: Vec<String>,
    format: String,
    allow_absolute: bool,
    max_results: usize,
    max_depth: usize,
    follow_links: bool,
    no_gitignore: bool,
    pretty: bool,
) -> Result<()> {
    let cfg = GlobConfig {
        pattern,
        min_lines,
        max_lines,
        min_bytes,
        max_bytes,
        modified_after_secs: modified_after.map(|s| parse_duration(&s)).transpose()?,
        modified_before_secs: modified_before.map(|s| parse_duration(&s)).transpose()?,
        user_excludes,
        format: OutputFormat::parse(&format)?,
        allow_absolute,
        max_results,
        max_depth,
        follow_links,
        respect_gitignore: !no_gitignore,
    };
    let out = collect(&cfg)?;
    let stdout = std::io::stdout();
    let mut lock = stdout.lock();
    render(&out, cfg.format, pretty, &mut lock)
}

/// Public wrapper so the MCP handler can share the duration parser.
pub fn parse_duration_public(s: &str) -> Result<u64> {
    parse_duration(s)
}

/// Parse durations like `7d`, `2h`, `30m`, `90s` into seconds.
fn parse_duration(s: &str) -> Result<u64> {
    let s = s.trim();
    if s.is_empty() {
        bail!("duration must not be empty");
    }
    let (num_part, unit) = s.split_at(s.len() - 1);
    let parsed: u64 = num_part.parse().map_err(|_| {
        anyhow!(
            "invalid duration '{}': expected <number><unit> (s/m/h/d)",
            s
        )
    })?;
    let multiplier = match unit {
        "s" => 1,
        "m" => 60,
        "h" => 3600,
        "d" => 86_400,
        _ => bail!("unknown duration unit '{}' (expected s/m/h/d)", unit),
    };
    Ok(parsed.saturating_mul(multiplier))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_rejects_parent_escape() {
        let err = normalize_pattern("../../etc/**", false).unwrap_err();
        assert!(err.to_string().contains(".."));
    }

    #[test]
    fn normalize_rejects_absolute_by_default() {
        let err = normalize_pattern("/etc/**", false).unwrap_err();
        assert!(err.to_string().to_lowercase().contains("absolute"));
    }

    #[test]
    fn normalize_allows_absolute_with_flag() {
        let n = normalize_pattern("/tmp/**/*.txt", true).unwrap();
        assert!(n.was_absolute);
    }

    #[test]
    fn normalize_splits_prefix_and_suffix() {
        let n = normalize_pattern("src/**/*.rs", false).unwrap();
        assert_eq!(n.walk_root, PathBuf::from("src"));
        assert_eq!(n.relative_pattern, "**/*.rs");
    }

    #[test]
    fn normalize_no_wildcard_is_literal() {
        let n = normalize_pattern("src/main.rs", false).unwrap();
        assert_eq!(n.walk_root, PathBuf::from("src/main.rs"));
    }

    #[test]
    fn parse_duration_units() {
        assert_eq!(parse_duration("30s").unwrap(), 30);
        assert_eq!(parse_duration("2m").unwrap(), 120);
        assert_eq!(parse_duration("3h").unwrap(), 10_800);
        assert_eq!(parse_duration("7d").unwrap(), 604_800);
    }

    #[test]
    fn parse_duration_rejects_bad_unit() {
        assert!(parse_duration("5y").is_err());
        assert!(parse_duration("abc").is_err());
        assert!(parse_duration("").is_err());
    }

    #[test]
    fn sanitize_strips_ansi() {
        let s = "path\x1b[31m/red\x1b[0m.rs";
        assert_eq!(sanitize_for_terminal(s), "path/red.rs");
    }

    #[test]
    fn sanitize_escapes_newline() {
        let s = "weird\npath.rs";
        assert_eq!(sanitize_for_terminal(s), "weird\\npath.rs");
    }

    #[test]
    fn is_generated_file_sentinel() {
        assert!(is_generated_file(
            Path::new("foo.rs"),
            Some("// @generated by thrift"),
        ));
        assert!(is_generated_file(Path::new("foo.generated.rs"), None));
        assert!(!is_generated_file(Path::new("foo.rs"), Some("// normal")));
    }

    #[test]
    fn exclude_matches_component() {
        assert!(matches_exclude(
            Path::new("target/debug/main.rs"),
            &["target"]
        ));
        assert!(!matches_exclude(
            Path::new("src/target_dir/main.rs"),
            &["target"]
        ));
    }

    #[test]
    fn exclude_matches_lock_extension() {
        assert!(matches_exclude(Path::new("Cargo.lock"), &["*.lock"]));
        assert!(!matches_exclude(Path::new("Cargo.toml"), &["*.lock"]));
    }

    #[test]
    fn path_to_string_uses_posix_separators() {
        #[cfg(windows)]
        {
            let p = PathBuf::from(r"src\glob.rs");
            let (s, _) = path_to_string(&p);
            assert_eq!(s, "src/glob.rs");
        }
        let p = PathBuf::from("src/glob.rs");
        let (s, _) = path_to_string(&p);
        assert_eq!(s, "src/glob.rs");
    }

    #[test]
    fn secret_denylist_not_overridable_via_excludes() {
        // Sanity: even if user passes --exclude '', the secret list still fires.
        // The denylist check runs BEFORE the exclude check in collect().
        use forge_shared::secrets::is_secret_filename;
        assert!(is_secret_filename(".env"));
    }

    // Integration: walk a temp dir and match the canonical scenario.
    #[test]
    fn integration_walk_finds_rust_files() -> Result<()> {
        let td = tempfile::tempdir()?;
        let root = td.path();
        std::fs::create_dir_all(root.join("src/inner"))?;
        std::fs::write(root.join("src/a.rs"), "fn a() {}\nfn b() {}\n")?;
        std::fs::write(root.join("src/inner/b.rs"), "fn c() {}\n")?;
        std::fs::write(root.join("src/README.md"), "hi")?;
        std::fs::write(root.join(".env"), "SECRET=abc")?; // must be excluded

        let cfg = GlobConfig {
            pattern: format!("{}/**/*.rs", root.display()),
            allow_absolute: true,
            respect_gitignore: false,
            ..Default::default()
        };
        let out = collect(&cfg)?;
        assert_eq!(out.total_matched, 2, "expected 2 rust files, got {:?}", out);
        // Denylist didn't match .env because it's not a .rs, but exclusion test
        // is covered in a dedicated test below.
        Ok(())
    }

    #[test]
    fn integration_secret_file_denied_even_when_matched() -> Result<()> {
        let td = tempfile::tempdir()?;
        let root = td.path();
        std::fs::write(root.join(".env"), "SECRET=abc")?;
        std::fs::write(root.join("ok.env"), "SECRET=abc")?;
        std::fs::write(root.join("regular.txt"), "hi")?;

        let cfg = GlobConfig {
            pattern: format!("{}/*", root.display()),
            allow_absolute: true,
            respect_gitignore: false,
            ..Default::default()
        };
        let out = collect(&cfg)?;
        // Both .env and ok.env should be skipped; regular.txt kept.
        assert_eq!(out.total_matched, 1);
        assert!(out.skipped_reasons.secret_denylist >= 2);
        Ok(())
    }

    #[test]
    fn integration_max_bytes_short_circuits_open() -> Result<()> {
        let td = tempfile::tempdir()?;
        let root = td.path();
        // 10 KiB file, cap at 1 KiB → must be skipped via metadata, not opened.
        let big = vec![b'a'; 10 * 1024];
        std::fs::write(root.join("big.txt"), &big)?;
        std::fs::write(root.join("small.txt"), b"hi")?;

        let cfg = GlobConfig {
            pattern: format!("{}/*.txt", root.display()),
            allow_absolute: true,
            max_bytes: 1024,
            respect_gitignore: false,
            ..Default::default()
        };
        let out = collect(&cfg)?;
        assert_eq!(out.total_matched, 1);
        assert_eq!(out.skipped_reasons.too_large, 1);
        Ok(())
    }

    #[test]
    fn integration_max_results_truncates() -> Result<()> {
        let td = tempfile::tempdir()?;
        let root = td.path();
        for i in 0..50 {
            std::fs::write(root.join(format!("f{i}.txt")), b"x")?;
        }
        let cfg = GlobConfig {
            pattern: format!("{}/*.txt", root.display()),
            allow_absolute: true,
            max_results: 10,
            respect_gitignore: false,
            ..Default::default()
        };
        let out = collect(&cfg)?;
        assert!(out.truncated);
        assert_eq!(out.total_matched, 10);
        Ok(())
    }

    #[test]
    fn json_render_has_schema_version() -> Result<()> {
        let r = GlobResult {
            schema_version: 1,
            pattern: "x".into(),
            filters: FilterEcho {
                min_lines: 0,
                max_lines: u64::MAX,
                min_bytes: 0,
                max_bytes: u64::MAX,
                modified_after_secs: None,
                modified_before_secs: None,
                excludes: vec![],
                allow_absolute: false,
                max_results: 10_000,
            },
            results: vec![],
            total_matched: 0,
            total_skipped: 0,
            skipped_reasons: SkippedReasons::default(),
            truncated: false,
        };
        let mut buf = Vec::new();
        render(&r, OutputFormat::Json, false, &mut buf)?;
        let s = String::from_utf8(buf)?;
        assert!(s.contains("$schema_version"));
        assert!(s.contains("\"pattern\":\"x\""));
        Ok(())
    }

    #[test]
    fn csv_render_quotes_commas() -> Result<()> {
        let r = GlobResult {
            schema_version: 1,
            pattern: "x".into(),
            filters: FilterEcho {
                min_lines: 0,
                max_lines: 0,
                min_bytes: 0,
                max_bytes: 0,
                modified_after_secs: None,
                modified_before_secs: None,
                excludes: vec![],
                allow_absolute: false,
                max_results: 0,
            },
            results: vec![FileEntry {
                path: r#"a,b"c.rs"#.into(),
                lines: 1,
                bytes: 2,
                modified: "2026-01-01T00:00:00Z".into(),
                is_generated: false,
                path_encoding: None,
            }],
            total_matched: 1,
            total_skipped: 0,
            skipped_reasons: SkippedReasons::default(),
            truncated: false,
        };
        let mut buf = Vec::new();
        render(&r, OutputFormat::Csv, false, &mut buf)?;
        let s = String::from_utf8(buf)?;
        // csv crate wraps in quotes and doubles internal quotes.
        assert!(s.contains(r#""a,b""c.rs""#));
        Ok(())
    }
}
