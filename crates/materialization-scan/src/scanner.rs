use regex::Regex;
use serde::Serialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ScanConfig {
    /// Include tests and fixtures. Defaults to false because tests legitimately collect.
    pub include_tests: bool,
    /// Maximum findings to return across a tree.
    pub max_findings: usize,
}

impl Default for ScanConfig {
    fn default() -> Self {
        Self {
            include_tests: false,
            max_findings: 500,
        }
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ScanReport {
    pub scanned_files: usize,
    pub finding_count: usize,
    pub findings: Vec<Finding>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct FileReport {
    pub file: String,
    pub findings: Vec<Finding>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Finding {
    pub file: String,
    pub line: usize,
    pub function: String,
    pub kind: FindingKind,
    pub severity: Severity,
    pub evidence: String,
    pub reason: String,
    pub remediation: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FindingKind {
    WholeFileRead,
    QueryRowsMaterialization,
    CollectInIoPath,
    GrowingVecInIoPath,
    ExpandingMapOfVecInIoPath,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    High,
    Medium,
    Low,
}

#[derive(Debug, Clone)]
struct FunctionSpan {
    name: String,
    start_line: usize,
    end_line: usize,
    body: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Lang {
    Rust,
    Go,
}

fn detect_lang(file: &str) -> Option<Lang> {
    if file.ends_with(".rs") {
        Some(Lang::Rust)
    } else if file.ends_with(".go") {
        Some(Lang::Go)
    } else {
        None
    }
}

/// Scan a file or directory recursively. Directory walks respect `.gitignore`.
pub fn scan_path(path: &Path, config: &ScanConfig) -> ScanReport {
    let mut findings = Vec::new();
    let mut scanned_files = 0usize;

    if path.is_file() {
        if should_scan_file(path, config) {
            if let Ok(source) = std::fs::read_to_string(path) {
                scanned_files += 1;
                findings.extend(scan_file(&path.display().to_string(), &source, config).findings);
            }
        }
    } else if path.is_dir() {
        for entry in ignore::WalkBuilder::new(path).build().flatten() {
            let entry_path = entry.path();
            if !entry.file_type().is_some_and(|ft| ft.is_file())
                || !should_scan_file(entry_path, config)
            {
                continue;
            }
            if findings.len() >= config.max_findings {
                break;
            }
            if let Ok(source) = std::fs::read_to_string(entry_path) {
                scanned_files += 1;
                findings
                    .extend(scan_file(&entry_path.display().to_string(), &source, config).findings);
                if findings.len() > config.max_findings {
                    findings.truncate(config.max_findings);
                    break;
                }
            }
        }
    }

    ScanReport {
        scanned_files,
        finding_count: findings.len(),
        findings,
    }
}

/// Scan one source file.
pub fn scan_file(file: &str, source: &str, config: &ScanConfig) -> FileReport {
    let mut findings = Vec::new();
    let lines: Vec<&str> = source.lines().collect();

    let Some(lang) = detect_lang(file) else {
        return FileReport {
            file: file.to_string(),
            findings,
        };
    };

    if !config.include_tests && is_test_file(file) {
        return FileReport {
            file: file.to_string(),
            findings,
        };
    }

    // File-level whole-file reads are often outside a parsed function (e.g. closures/tests).
    for (i, line) in lines.iter().enumerate() {
        let trimmed = strip_line_comment(line).trim();
        if is_whole_file_read(trimmed, lang) && !is_obviously_bounded_or_config(trimmed) {
            findings.push(Finding {
                file: file.to_string(),
                line: i + 1,
                function: function_name_at(&lines, i, lang).unwrap_or_default(),
                kind: FindingKind::WholeFileRead,
                severity: Severity::Medium,
                evidence: trimmed.to_string(),
                reason: "whole-file read can materialize arbitrary input; prefer streaming BufRead/read_exact chunks or explicit size caps".to_string(),
                remediation: "Replace with streaming/chunked reads, or document and enforce a small max size before allocation.".to_string(),
            });
        }
    }

    for func in find_functions(&lines, lang) {
        if !config.include_tests && looks_like_test_function(&func.name, &func.body) {
            continue;
        }
        let io = io_score(&func.body, lang);
        if io == 0 {
            continue;
        }
        if contains_query_materialization(&func.body, lang) {
            push_func_finding(
                &mut findings,
                file,
                &func,
                FindingKind::QueryRowsMaterialization,
                Severity::High,
                first_matching_line(&func, query_materialization_evidence_needles(lang)),
                query_materialization_reason(lang).to_string(),
                query_materialization_remediation(lang).to_string(),
            );
        }

        if contains_collect_in_io_path(&func.body, lang) {
            push_func_finding(
                &mut findings,
                file,
                &func,
                FindingKind::CollectInIoPath,
                if io >= 2 {
                    Severity::High
                } else {
                    Severity::Medium
                },
                first_matching_line(&func, collect_evidence_needles(lang)),
                collect_reason(lang).to_string(),
                collect_remediation(lang).to_string(),
            );
        }

        if contains_vec_growth(&func.body, lang) {
            push_func_finding(
                &mut findings,
                file,
                &func,
                FindingKind::GrowingVecInIoPath,
                if io >= 2 {
                    Severity::High
                } else {
                    Severity::Medium
                },
                first_matching_line(&func, vec_growth_evidence_needles(lang)),
                vec_growth_reason(lang).to_string(),
                vec_growth_remediation(lang).to_string(),
            );
        }

        if contains_map_of_vec_growth(&func.body, lang) {
            push_func_finding(
                &mut findings,
                file,
                &func,
                FindingKind::ExpandingMapOfVecInIoPath,
                Severity::High,
                first_matching_line(&func, map_of_vec_evidence_needles(lang)),
                map_of_vec_reason(lang).to_string(),
                map_of_vec_remediation(lang).to_string(),
            );
        }
    }

    dedupe_findings(&mut findings);
    FileReport {
        file: file.to_string(),
        findings,
    }
}

fn should_scan_file(path: &Path, config: &ScanConfig) -> bool {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    let source_ext = matches!(ext, "rs" | "go");
    source_ext && (config.include_tests || !is_test_file(&path.display().to_string()))
}

fn is_test_file(file: &str) -> bool {
    file.contains("/tests/")
        || file.ends_with("_test.rs")
        || file.ends_with("_tests.rs")
        || file.ends_with("_test.go")
        || file.ends_with(".test.ts")
        || file.ends_with(".spec.ts")
}

fn looks_like_test_function(name: &str, body: &str) -> bool {
    // Rust: snake_case `test_*` plus the test attribute macros.
    if name.contains("test") || body.contains("#[test]") || body.contains("#[tokio::test]") {
        return true;
    }
    // Go: Test/Benchmark/Example/Fuzz prefix or *testing.T/B/M/F parameter.
    if name.starts_with("Test")
        || name.starts_with("Benchmark")
        || name.starts_with("Example")
        || name.starts_with("Fuzz")
    {
        return true;
    }
    if body.contains("*testing.T")
        || body.contains("*testing.B")
        || body.contains("*testing.M")
        || body.contains("*testing.F")
    {
        return true;
    }
    false
}

fn strip_line_comment(line: &str) -> &str {
    line.split("//").next().unwrap_or(line)
}

fn is_whole_file_read(line: &str, lang: Lang) -> bool {
    let patterns: &[&str] = match lang {
        Lang::Rust => &[
            "std::fs::read(",
            "fs::read(",
            "std::fs::read_to_string",
            "fs::read_to_string",
            ".read_to_end(",
            ".read_to_string(",
        ],
        Lang::Go => &[
            "os.ReadFile(",
            "ioutil.ReadFile(",
            "io.ReadAll(",
            "ioutil.ReadAll(",
        ],
    };
    contains_any(line, patterns)
}

fn is_obviously_bounded_or_config(line: &str) -> bool {
    contains_any(
        line,
        &[
            "Cargo.toml",
            "config",
            "metadata",
            "fixture",
            "testdata",
            "go.mod",
            "go.sum",
        ],
    )
}

fn io_score(body: &str, lang: Lang) -> usize {
    let markers: &[&str] = match lang {
        Lang::Rust => &[
            "std::fs::",
            "fs::",
            "File::open",
            "OpenOptions",
            "read_dir",
            "BufReader",
            "ReadAt",
            "SSTableReader",
            "partitions_iter",
            "read_range",
            "read_partitions",
            "query_unpaged",
            "execute_iter",
            "rows_or_empty",
            "ALLOW FILTERING",
            "SELECT ",
            "scan",
            "stream",
            "snapshot",
            "AsyncRead",
            "read_to_end",
            "read_to_string",
        ],
        Lang::Go => &[
            "os.Open",
            "os.OpenFile",
            "os.Create",
            "os.ReadFile",
            "os.ReadDir",
            "ioutil.ReadFile",
            "ioutil.ReadAll",
            "ioutil.ReadDir",
            "io.ReadAll",
            "io.Copy",
            "bufio.NewReader",
            "bufio.NewScanner",
            "filepath.Walk",
            "db.Query",
            "QueryContext",
            "db.QueryRow",
            "rows.Next(",
            "rows.Scan(",
            "rows.Err(",
            "cursor.Next",
            "cursor.Decode",
            "cursor.All(",
            ".Iter(",
            "iter.Scan(",
            "iter.Next(",
            "gocql",
            "SELECT ",
            "ALLOW FILTERING",
            "http.Get",
            "http.Post",
            "http.NewRequest",
            "resp.Body",
            "s3.GetObject",
            "s3.PutObject",
            "GetObjectInput",
        ],
    };
    markers.iter().filter(|m| body.contains(**m)).count()
}

fn contains_any(haystack: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| haystack.contains(needle))
}

fn contains_query_materialization(body: &str, lang: Lang) -> bool {
    match lang {
        Lang::Rust => {
            contains_any(
                body,
                &[
                    "rows_or_empty()",
                    ".rows_or_empty(",
                    "query_unpaged",
                    "ALLOW FILTERING",
                ],
            ) && contains_any(
                body,
                &[
                    "rows_or_empty().len",
                    ".len()",
                    "for row in",
                    ".collect()",
                    ".collect::<",
                    ".extend(",
                    ".push(",
                ],
            )
        }
        Lang::Go => {
            // Bulk one-shot cursor materialization is itself the anti-pattern.
            if body.contains("cursor.All(") {
                return true;
            }
            let has_iter = contains_any(
                body,
                &[
                    "for rows.Next()",
                    "rows.Next(",
                    "for cursor.Next(",
                    "cursor.Next(",
                    "for iter.Scan(",
                    "iter.Scan(",
                    "iter.Next(",
                ],
            );
            let has_materialize = contains_any(
                body,
                &["append(", "rows.Scan(", "iter.Scan(", "cursor.Decode("],
            );
            has_iter && has_materialize
        }
    }
}

fn query_materialization_evidence_needles(lang: Lang) -> &'static [&'static str] {
    match lang {
        Lang::Rust => &[
            "rows_or_empty",
            "query_unpaged",
            "ALLOW FILTERING",
            ".collect",
            ".push",
        ],
        Lang::Go => &[
            "cursor.All(",
            "for rows.Next()",
            "rows.Next(",
            "iter.Scan(",
            "append(",
        ],
    }
}

fn query_materialization_reason(lang: Lang) -> &'static str {
    match lang {
        Lang::Rust => {
            "query/scan result appears to be materialized before counting, filtering, or streaming; Ferrosa history shows this causes OOMs on tenant-wide scans"
        }
        Lang::Go => {
            "query/cursor result appears to be materialized into a slice (or `cursor.All` reads the entire result); this is a common OOM shape on large result sets"
        }
    }
}

fn query_materialization_remediation(lang: Lang) -> &'static str {
    match lang {
        Lang::Rust => {
            "Use paged driver iteration, server-side COUNT/SUM, bounded LIMIT/page-size, or stream chunks through a channel."
        }
        Lang::Go => {
            "Stream rows/cursor with a bounded handler (`for rows.Next() { handle(row) }` without `append`), use server-side aggregates, or set an explicit `LIMIT`/page size."
        }
    }
}

fn contains_collect_in_io_path(body: &str, lang: Lang) -> bool {
    match lang {
        Lang::Rust => contains_any(
            body,
            &[
                ".collect()",
                ".collect::<",
                "collect::<Vec",
                "collect::<Result<Vec",
            ],
        ),
        // Go 1.23+ adds slices.Collect / maps.Collect — same shape as Rust's collect().
        Lang::Go => contains_any(body, &["slices.Collect(", "maps.Collect("]),
    }
}

fn collect_evidence_needles(lang: Lang) -> &'static [&'static str] {
    match lang {
        Lang::Rust => &[".collect", "collect::<"],
        Lang::Go => &["slices.Collect(", "maps.Collect("],
    }
}

fn collect_reason(lang: Lang) -> &'static str {
    match lang {
        Lang::Rust => {
            "I/O-like function uses collect(), which can drain an unbounded iterator/result set into memory"
        }
        Lang::Go => {
            "I/O-like function uses slices.Collect/maps.Collect, which drains an iterator into memory"
        }
    }
}

fn collect_remediation(lang: Lang) -> &'static str {
    match lang {
        Lang::Rust => {
            "Prefer returning an iterator/stream, using try_for_each, paging tokens, or collecting only after a proven bound."
        }
        Lang::Go => {
            "Prefer returning the iterator (`iter.Seq`/`iter.Seq2`), processing one item at a time, or collecting only after a proven bound."
        }
    }
}

fn contains_vec_growth(body: &str, lang: Lang) -> bool {
    match lang {
        Lang::Rust => {
            contains_any(body, &["Vec::new", "Vec::with_capacity", ": Vec<", "Vec<"])
                && contains_any(
                    body,
                    &[".push(", ".extend(", ".append(", ".collect(", ".collect::<"],
                )
        }
        Lang::Go => {
            let has_slice_decl = contains_any(
                body,
                &[
                    "make([]",
                    ":= []",
                    "= []",
                    "[]string{",
                    "[]byte{",
                    "[]int{",
                    "[]rune{",
                ],
            ) || contains_go_var_slice_decl(body);
            let has_growth = contains_any(body, &[" = append(", "\t= append(", "  = append("])
                || body.contains("= append(");
            has_slice_decl && has_growth
        }
    }
}

/// Match Go `var name []T` style declarations.
fn contains_go_var_slice_decl(body: &str) -> bool {
    let re = Regex::new(r"(?m)^\s*var\s+[A-Za-z_][A-Za-z0-9_]*\s+\[\]").unwrap();
    re.is_match(body)
}

fn vec_growth_evidence_needles(lang: Lang) -> &'static [&'static str] {
    match lang {
        Lang::Rust => &[
            "Vec::new",
            "Vec::with_capacity",
            ".push(",
            ".extend(",
            ".append(",
        ],
        Lang::Go => &["append(", "make([]", ":= []", "var "],
    }
}

fn vec_growth_reason(lang: Lang) -> &'static str {
    match lang {
        Lang::Rust => {
            "I/O-like function grows a Vec while reading/scanning; this is a common eager materialization shape"
        }
        Lang::Go => {
            "I/O-like function grows a slice via append while reading/scanning; this is a common eager materialization shape"
        }
    }
}

fn vec_growth_remediation(lang: Lang) -> &'static str {
    match lang {
        Lang::Rust => {
            "Stream/paginate to the consumer, process one item at a time, or enforce a hard cap before push/extend."
        }
        Lang::Go => {
            "Stream/paginate to the consumer, process one item at a time, pre-size with `make([]T, 0, n)` when bounded, or enforce a hard cap before append."
        }
    }
}

fn contains_map_of_vec_growth(body: &str, lang: Lang) -> bool {
    match lang {
        Lang::Rust => {
            (contains_any(body, &["BTreeMap<", "HashMap<", ".entry("])
                && contains_any(
                    body,
                    &[
                        "Vec<",
                        "Vec::new",
                        ".or_default().push",
                        ".or_insert_with(Vec::new)",
                    ],
                ))
                || contains_any(body, &["BTreeMap<Vec", "HashMap<Vec"])
        }
        Lang::Go => {
            // map[K][]V declaration shape + `m[k] = append(m[k], v)` growth shape.
            let has_map_of_slice = (body.contains("map[") && body.contains("][]"))
                || body.contains("make(map[") && body.contains("][]");
            let has_map_append = contains_go_map_slice_append(body);
            has_map_of_slice && has_map_append
        }
    }
}

/// Match Go `m[k] = append(m[k], v)` style map-of-slice growth.
///
/// Substring match instead of a regex because real-world keys may include
/// nested brackets (`m[line[:1]] = append(...)`), which trip up a naïve
/// `[^\]]+` character class.
fn contains_go_map_slice_append(body: &str) -> bool {
    body.contains("] = append(") || body.contains("]=append(")
}

fn map_of_vec_evidence_needles(lang: Lang) -> &'static [&'static str] {
    match lang {
        Lang::Rust => &[".or_default().push", ".entry(", "BTreeMap", "HashMap"],
        // Skip the bare `map[` token — it matches the function signature's
        // return type, which is not useful evidence. Prefer the actual growth
        // pattern; fall back to `make(map[...` which is the initializer line.
        Lang::Go => &["] = append(", "]=append(", "make(map["],
    }
}

fn map_of_vec_reason(lang: Lang) -> &'static str {
    match lang {
        Lang::Rust => {
            "I/O-like function groups scanned data into a map of Vecs; prior Ferrosa compaction bugs used this shape before switching to k-way streaming merge"
        }
        Lang::Go => {
            "I/O-like function groups scanned data into a map[K][]V; unbounded grouping is a common OOM shape on long-running streams"
        }
    }
}

fn map_of_vec_remediation(lang: Lang) -> &'static str {
    match lang {
        Lang::Rust => {
            "Use a streaming merge/heap, external sort, bounded grouping window, or spill-to-disk sidecar instead of retaining every group."
        }
        Lang::Go => {
            "Use a streaming merge/heap, bounded grouping window, periodic flush, or spill-to-disk sidecar instead of retaining every group."
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn push_func_finding(
    findings: &mut Vec<Finding>,
    file: &str,
    func: &FunctionSpan,
    kind: FindingKind,
    severity: Severity,
    evidence: (usize, String),
    reason: String,
    remediation: String,
) {
    findings.push(Finding {
        file: file.to_string(),
        line: evidence.0,
        function: func.name.clone(),
        kind,
        severity,
        evidence: evidence.1,
        reason,
        remediation,
    });
}

fn first_matching_line(func: &FunctionSpan, needles: &[&str]) -> (usize, String) {
    for (idx, line) in func.body.lines().enumerate() {
        if needles.iter().any(|needle| line.contains(needle)) {
            return (func.start_line + idx, line.trim().to_string());
        }
    }
    (
        func.start_line,
        func.body
            .lines()
            .next()
            .unwrap_or_default()
            .trim()
            .to_string(),
    )
}

fn dedupe_findings(findings: &mut Vec<Finding>) {
    findings.sort_by(|a, b| {
        (&a.file, a.line, format!("{:?}", a.kind)).cmp(&(&b.file, b.line, format!("{:?}", b.kind)))
    });
    findings.dedup_by(|a, b| {
        a.file == b.file && a.line == b.line && a.kind == b.kind && a.function == b.function
    });
}

fn function_name_at(lines: &[&str], line_idx: usize, lang: Lang) -> Option<String> {
    let funcs = find_functions(lines, lang);
    funcs
        .into_iter()
        .find(|f| line_idx + 1 >= f.start_line && line_idx < f.end_line)
        .map(|f| f.name)
}

fn find_functions(lines: &[&str], lang: Lang) -> Vec<FunctionSpan> {
    let rust_re =
        Regex::new(r"^\s*(?:pub\s+)?(?:async\s+)?fn\s+([A-Za-z_][A-Za-z0-9_]*)\s*[<(]").unwrap();
    let go_fn_re = Regex::new(r"^\s*func\s+([A-Za-z_][A-Za-z0-9_]*)\s*\(").unwrap();
    // Go method receiver: `func (r *Receiver) Name(`.
    let go_method_re = Regex::new(r"^\s*func\s+\([^)]+\)\s+([A-Za-z_][A-Za-z0-9_]*)\s*\(").unwrap();
    let mut funcs = Vec::new();
    let mut i = 0usize;
    while i < lines.len() {
        let line = lines[i];
        let name: Option<String> = match lang {
            Lang::Rust => rust_re
                .captures(line)
                .and_then(|c| c.get(1).map(|m| m.as_str().to_string())),
            Lang::Go => go_method_re
                .captures(line)
                .and_then(|c| c.get(1).map(|m| m.as_str().to_string()))
                .or_else(|| {
                    go_fn_re
                        .captures(line)
                        .and_then(|c| c.get(1).map(|m| m.as_str().to_string()))
                }),
        };
        let Some(name) = name else {
            i += 1;
            continue;
        };
        let end = find_block_end(lines, i);
        let body = lines[i..=end].join("\n");
        funcs.push(FunctionSpan {
            name,
            start_line: i + 1,
            end_line: end + 1,
            body,
        });
        i = end.saturating_add(1);
    }
    funcs
}

fn find_block_end(lines: &[&str], start: usize) -> usize {
    let mut depth = 0i32;
    let mut saw_open = false;
    for (idx, line) in lines.iter().enumerate().skip(start) {
        for ch in strip_string_literals(line).chars() {
            match ch {
                '{' => {
                    depth += 1;
                    saw_open = true;
                }
                '}' => {
                    depth -= 1;
                    if saw_open && depth <= 0 {
                        return idx;
                    }
                }
                _ => {}
            }
        }
    }
    // Fallback for single-line/indent languages: stop at next blank top-level-ish line.
    let base_indent = lines[start].len() - lines[start].trim_start().len();
    for (idx, line) in lines.iter().enumerate().skip(start + 1) {
        let trimmed = line.trim();
        if !trimmed.is_empty() && line.len() - line.trim_start().len() <= base_indent {
            return idx.saturating_sub(1);
        }
    }
    lines.len().saturating_sub(1)
}

fn strip_string_literals(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut in_string = false;
    let mut escaped = false;
    for ch in line.chars() {
        if in_string {
            if escaped {
                escaped = false;
                continue;
            }
            if ch == '\\' {
                escaped = true;
                continue;
            }
            if ch == '"' {
                in_string = false;
            }
            continue;
        }
        if ch == '"' {
            in_string = true;
            continue;
        }
        out.push(ch);
    }
    out
}

#[allow(dead_code)]
fn _normalize_path(path: PathBuf) -> String {
    path.display().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flags_vec_growth_in_disk_io_function() {
        let src = r#"
            fn load_partitions(path: &Path) -> Result<Vec<Partition>> {
                let mut reader = SSTableReader::open(path)?;
                let mut out = Vec::new();
                for p in reader.partitions_iter()? {
                    out.push(p?);
                }
                Ok(out)
            }
        "#;
        let report = scan_file("src/storage.rs", src, &ScanConfig::default());
        assert!(report
            .findings
            .iter()
            .any(|f| f.kind == FindingKind::GrowingVecInIoPath));
    }

    #[test]
    fn flags_collect_in_storage_read_path() {
        let src = r#"
            async fn read_rows(store: &Store) -> Result<Vec<Row>> {
                store.read_range(..).await?.into_iter().map(row_to_wire).collect()
            }
        "#;
        let report = scan_file("src/router.rs", src, &ScanConfig::default());
        assert!(report
            .findings
            .iter()
            .any(|f| f.kind == FindingKind::CollectInIoPath));
    }

    #[test]
    fn flags_query_rows_or_empty_materialization() {
        let src = r#"
            async fn count_edges(session: &Session, query: String) -> Result<usize> {
                let result = session.query_unpaged(query, ()).await?;
                let rows = result.rows_or_empty();
                Ok(rows.len())
            }
        "#;
        let report = scan_file("src/cql_storage.rs", src, &ScanConfig::default());
        assert!(report
            .findings
            .iter()
            .any(|f| f.kind == FindingKind::QueryRowsMaterialization));
    }

    #[test]
    fn flags_map_of_vec_grouping_in_compaction_path() {
        let src = r#"
            fn compact(inputs: Vec<PathBuf>) -> Result<()> {
                let mut all: BTreeMap<Vec<u8>, Vec<Partition>> = BTreeMap::new();
                for path in inputs {
                    let reader = SSTableReader::open(path)?;
                    for p in reader.partitions_iter()? {
                        all.entry(p?.key).or_default().push(p?);
                    }
                }
                Ok(())
            }
        "#;
        let report = scan_file("src/compaction.rs", src, &ScanConfig::default());
        assert!(report
            .findings
            .iter()
            .any(|f| f.kind == FindingKind::ExpandingMapOfVecInIoPath));
    }

    #[test]
    fn skips_tests_by_default() {
        let src = r#"
            fn test_fixture() {
                let data = std::fs::read_to_string("big.fixture").unwrap();
                let rows: Vec<_> = data.lines().collect();
            }
        "#;
        let report = scan_file("tests/fixture.rs", src, &ScanConfig::default());
        assert!(report.findings.is_empty());
    }

    // ----- Go ---------------------------------------------------------------

    #[test]
    fn flags_slice_growth_in_disk_io_function_go() {
        let src = r#"
package storage

func LoadPartitions(path string) ([]Partition, error) {
    f, err := os.Open(path)
    if err != nil {
        return nil, err
    }
    defer f.Close()
    var out []Partition
    scanner := bufio.NewScanner(f)
    for scanner.Scan() {
        out = append(out, parsePartition(scanner.Bytes()))
    }
    return out, scanner.Err()
}
"#;
        let report = scan_file("storage/loader.go", src, &ScanConfig::default());
        assert!(
            report
                .findings
                .iter()
                .any(|f| f.kind == FindingKind::GrowingVecInIoPath),
            "expected GrowingVecInIoPath, got {:?}",
            report.findings
        );
    }

    #[test]
    fn flags_io_readall_whole_file_read_go() {
        let src = r#"
package handler

import (
    "io"
)

func ReadBody(r io.Reader) ([]byte, error) {
    return io.ReadAll(r)
}
"#;
        let report = scan_file("handler/handler.go", src, &ScanConfig::default());
        assert!(
            report
                .findings
                .iter()
                .any(|f| f.kind == FindingKind::WholeFileRead),
            "expected WholeFileRead, got {:?}",
            report.findings
        );
    }

    #[test]
    fn flags_os_readfile_whole_file_read_go() {
        let src = r#"
package config

import "os"

func Load(path string) ([]byte, error) {
    return os.ReadFile(path)
}
"#;
        let report = scan_file("config/load.go", src, &ScanConfig::default());
        // os.ReadFile on an arbitrary path is flagged as whole-file read.
        assert!(
            report
                .findings
                .iter()
                .any(|f| f.kind == FindingKind::WholeFileRead),
            "expected WholeFileRead, got {:?}",
            report.findings
        );
    }

    #[test]
    fn flags_query_rows_materialization_go() {
        let src = r#"
package db

import (
    "context"
    "database/sql"
)

func ListEdges(ctx context.Context, db *sql.DB, q string) ([]Edge, error) {
    rows, err := db.QueryContext(ctx, q)
    if err != nil {
        return nil, err
    }
    defer rows.Close()
    var out []Edge
    for rows.Next() {
        var e Edge
        if err := rows.Scan(&e.Src, &e.Dst, &e.Type); err != nil {
            return nil, err
        }
        out = append(out, e)
    }
    return out, rows.Err()
}
"#;
        let report = scan_file("db/edges.go", src, &ScanConfig::default());
        assert!(
            report
                .findings
                .iter()
                .any(|f| f.kind == FindingKind::QueryRowsMaterialization),
            "expected QueryRowsMaterialization, got {:?}",
            report.findings
        );
    }

    #[test]
    fn flags_mongo_cursor_all_query_materialization_go() {
        let src = r#"
package mongo

func FindAll(ctx context.Context, coll *Collection, filter bson.M) ([]Doc, error) {
    cursor, err := coll.Find(ctx, filter)
    if err != nil {
        return nil, err
    }
    var out []Doc
    if err := cursor.All(ctx, &out); err != nil {
        return nil, err
    }
    return out, nil
}
"#;
        let report = scan_file("mongo/find.go", src, &ScanConfig::default());
        assert!(
            report
                .findings
                .iter()
                .any(|f| f.kind == FindingKind::QueryRowsMaterialization),
            "expected QueryRowsMaterialization for cursor.All, got {:?}",
            report.findings
        );
    }

    #[test]
    fn flags_map_of_slice_grouping_go() {
        let src = r#"
package compact

import (
    "bufio"
    "os"
)

func Compact(paths []string) error {
    all := map[string][]string{}
    for _, p := range paths {
        f, err := os.Open(p)
        if err != nil {
            return err
        }
        scanner := bufio.NewScanner(f)
        for scanner.Scan() {
            line := scanner.Text()
            key := line[:1]
            all[key] = append(all[key], line)
        }
        f.Close()
    }
    _ = all
    return nil
}
"#;
        let report = scan_file("compact/compact.go", src, &ScanConfig::default());
        assert!(
            report
                .findings
                .iter()
                .any(|f| f.kind == FindingKind::ExpandingMapOfVecInIoPath),
            "expected ExpandingMapOfVecInIoPath, got {:?}",
            report.findings
        );
    }

    #[test]
    fn flags_map_of_slice_grouping_go_with_nested_bracket_key() {
        // Regression: keys like `m[line[:1]]` contain a nested `[`/`]` which
        // tripped an earlier `[^\]]+` regex.
        let src = r#"
package compact

import (
    "bufio"
    "os"
)

func GroupLines(path string) (map[string][]string, error) {
    f, err := os.Open(path)
    if err != nil {
        return nil, err
    }
    defer f.Close()
    m := map[string][]string{}
    sc := bufio.NewScanner(f)
    for sc.Scan() {
        line := sc.Text()
        m[line[:1]] = append(m[line[:1]], line)
    }
    return m, nil
}
"#;
        let report = scan_file("compact/group.go", src, &ScanConfig::default());
        assert!(
            report
                .findings
                .iter()
                .any(|f| f.kind == FindingKind::ExpandingMapOfVecInIoPath),
            "expected ExpandingMapOfVecInIoPath with nested-bracket key, got {:?}",
            report.findings
        );
    }

    #[test]
    fn flags_slices_collect_in_io_path_go() {
        let src = r#"
package edges

import "slices"

func AllEdges(ctx context.Context, db *sql.DB) []Edge {
    rows, _ := db.QueryContext(ctx, "SELECT * FROM edges")
    defer rows.Close()
    return slices.Collect(rowsIter(rows))
}
"#;
        let report = scan_file("edges/all.go", src, &ScanConfig::default());
        assert!(
            report
                .findings
                .iter()
                .any(|f| f.kind == FindingKind::CollectInIoPath),
            "expected CollectInIoPath (slices.Collect), got {:?}",
            report.findings
        );
    }

    #[test]
    fn detects_go_method_receiver_function() {
        // The function-finding pass must see `func (s *Store) Load(...)` so it
        // can run the in-function checks on it.
        let src = r#"
package storage

type Store struct{}

func (s *Store) Load(ctx context.Context, db *sql.DB) ([]Row, error) {
    rows, err := db.QueryContext(ctx, "SELECT * FROM t")
    if err != nil {
        return nil, err
    }
    defer rows.Close()
    var out []Row
    for rows.Next() {
        var r Row
        rows.Scan(&r.A, &r.B)
        out = append(out, r)
    }
    return out, nil
}
"#;
        let report = scan_file("storage/store.go", src, &ScanConfig::default());
        assert!(
            report.findings.iter().any(|f| f.function == "Load"),
            "expected a finding inside method Load, got {:?}",
            report.findings
        );
    }

    #[test]
    fn skips_go_test_files_by_default() {
        let src = r#"
package db

import "testing"

func TestThing(t *testing.T) {
    data, _ := os.ReadFile("big.fixture")
    _ = data
}
"#;
        let report = scan_file("db/edges_test.go", src, &ScanConfig::default());
        assert!(
            report.findings.is_empty(),
            "expected no findings in _test.go, got {:?}",
            report.findings
        );
    }

    #[test]
    fn skips_go_test_function_in_non_test_file_by_default() {
        // Test func name detection (Test*/Benchmark*/Example*/Fuzz*) should skip
        // even when accidentally placed in a non-_test.go file.
        let src = r#"
package db

func TestThing(t *testing.T) {
    data, _ := os.ReadFile("big.fixture")
    _ = data
}
"#;
        let report = scan_file("db/edges.go", src, &ScanConfig::default());
        assert!(
            report.findings.iter().all(|f| f.function != "TestThing"),
            "expected TestThing to be skipped, got {:?}",
            report.findings
        );
    }
}
