//! AST-backed scanner for code that hides incomplete or failed behavior.
//!
//! The rule set is intentionally narrow. A finding is emitted only when a
//! tree-sitter syntax node proves that code is in an error-handling or runtime
//! return position and the local syntax converts that path into success,
//! default data, or mock data. Comments and identifier names are supporting
//! evidence; they do not produce findings by themselves.

use anyhow::{anyhow, Result};
use ignore::WalkBuilder;
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use tree_sitter::{Language, Node, Parser};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Medium,
    High,
    Critical,
}

impl Severity {
    fn tag(self) -> &'static str {
        match self {
            Self::Medium => "medium",
            Self::High => "high",
            Self::Critical => "critical",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Confidence {
    Medium,
    High,
}

impl Confidence {
    pub fn parse(value: &str) -> Result<Self> {
        match value.to_ascii_lowercase().as_str() {
            "medium" | "med" => Ok(Self::Medium),
            "high" => Ok(Self::High),
            other => Err(anyhow!("unknown confidence: {other}")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    SwallowedError,
    FakeSuccess,
    MockLeak,
    PlaceholderImpl,
    OptimisticStatus,
}

impl Category {
    pub fn parse(value: &str) -> Result<Self> {
        match value.to_ascii_lowercase().as_str() {
            "swallowed_error" | "swallowed" => Ok(Self::SwallowedError),
            "fake_success" | "fake" => Ok(Self::FakeSuccess),
            "mock_leak" | "mock" => Ok(Self::MockLeak),
            "placeholder_impl" | "placeholder" => Ok(Self::PlaceholderImpl),
            "optimistic_status" | "optimistic" => Ok(Self::OptimisticStatus),
            other => Err(anyhow!("unknown fail-loud category: {other}")),
        }
    }

    fn tag(self) -> &'static str {
        match self {
            Self::SwallowedError => "swallowed_error",
            Self::FakeSuccess => "fake_success",
            Self::MockLeak => "mock_leak",
            Self::PlaceholderImpl => "placeholder_impl",
            Self::OptimisticStatus => "optimistic_status",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Options {
    pub categories: Vec<Category>,
    pub min_confidence: Confidence,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            categories: Vec::new(),
            min_confidence: Confidence::High,
        }
    }
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct Finding {
    pub id: &'static str,
    pub category: Category,
    pub severity: Severity,
    pub confidence: Confidence,
    pub language: &'static str,
    pub file: String,
    pub line: usize,
    pub function: Option<String>,
    pub snippet: String,
    pub evidence: Vec<String>,
    pub recommendation: &'static str,
}

/// Static fingerprint of a finding kind.  Holds everything that is identical
/// across every instance of a given pattern, so the `push` helper only needs
/// the per-call AST node and evidence strings.
struct FindingKind {
    id: &'static str,
    category: Category,
    severity: Severity,
    confidence: Confidence,
    recommendation: &'static str,
}

const RUST_ERR_OK_DEFAULT: FindingKind = FindingKind {
    id: "FAILLOUD-RUST-ERR-OK-DEFAULT",
    category: Category::FakeSuccess,
    severity: Severity::High,
    confidence: Confidence::High,
    recommendation: "Propagate the original error or return an explicit failure; do not convert Err into successful default data.",
};

const RUST_IFERR_OK_DEFAULT: FindingKind = FindingKind {
    id: "FAILLOUD-RUST-IFERR-OK-DEFAULT",
    category: Category::FakeSuccess,
    severity: Severity::High,
    confidence: Confidence::High,
    recommendation:
        "Propagate the original error or return an explicit failure from the Err branch.",
};

const RUST_IGNORED_FALLIBLE: FindingKind = FindingKind {
    id: "FAILLOUD-RUST-IGNORED-FALLIBLE",
    category: Category::SwallowedError,
    severity: Severity::High,
    confidence: Confidence::High,
    recommendation:
        "Handle the Result explicitly or propagate it with context instead of assigning it to `_`.",
};

const RUST_RESULT_ERASURE: FindingKind = FindingKind {
    id: "FAILLOUD-RUST-RESULT-ERASURE",
    category: Category::SwallowedError,
    severity: Severity::High,
    confidence: Confidence::High,
    recommendation: "Preserve the error path; use `?`, `map_err`, or an explicit fallback status instead of erasing the error.",
};

const PY_EXCEPT_DEFAULT: FindingKind = FindingKind {
    id: "FAILLOUD-PY-EXCEPT-DEFAULT",
    category: Category::SwallowedError,
    severity: Severity::High,
    confidence: Confidence::High,
    recommendation:
        "Raise, propagate, or return an explicit failure response from the exception handler.",
};

const JS_CATCH_DEFAULT: FindingKind = FindingKind {
    id: "FAILLOUD-JS-CATCH-DEFAULT",
    category: Category::SwallowedError,
    severity: Severity::High,
    confidence: Confidence::High,
    recommendation:
        "Throw, reject, or return an explicit failure/degraded response from the catch block.",
};

const JS_PROMISE_CATCH_DEFAULT: FindingKind = FindingKind {
    id: "FAILLOUD-JS-PROMISE-CATCH-DEFAULT",
    category: Category::SwallowedError,
    severity: Severity::High,
    confidence: Confidence::High,
    recommendation: "Do not hide rejected promises behind default data; surface the failure or mark the fallback explicitly.",
};

const GO_ERR_NIL_SUCCESS: FindingKind = FindingKind {
    id: "FAILLOUD-GO-ERR-NIL-SUCCESS",
    category: Category::FakeSuccess,
    severity: Severity::High,
    confidence: Confidence::High,
    recommendation: "Return the error or wrap it; do not return nil/default success values from an err != nil branch.",
};

const ELIXIR_ERROR_OK_DEFAULT: FindingKind = FindingKind {
    id: "FAILLOUD-ELIXIR-ERROR-OK-DEFAULT",
    category: Category::FakeSuccess,
    severity: Severity::High,
    confidence: Confidence::High,
    recommendation:
        "Return `{:error, reason}` or raise; do not convert an error tuple into `{:ok, default}`.",
};

const ELIXIR_RESCUE_OK_DEFAULT: FindingKind = FindingKind {
    id: "FAILLOUD-ELIXIR-RESCUE-OK-DEFAULT",
    category: Category::SwallowedError,
    severity: Severity::High,
    confidence: Confidence::High,
    recommendation: "Return an explicit error tuple or re-raise with context from rescue blocks.",
};

const JVM_DOTNET_CATCH_DEFAULT: FindingKind = FindingKind {
    id: "FAILLOUD-JVM-DOTNET-CATCH-DEFAULT",
    category: Category::SwallowedError,
    severity: Severity::High,
    confidence: Confidence::High,
    recommendation: "Throw, return an explicit error result, or surface degraded mode instead of returning success/default data.",
};

const MOCK_LEAK_RUNTIME: FindingKind = FindingKind {
    id: "FAILLOUD-MOCK-LEAK-RUNTIME",
    category: Category::MockLeak,
    severity: Severity::High,
    confidence: Confidence::High,
    recommendation: "Move mock data into test fixtures or return an explicit not-implemented/error result from runtime code.",
};

const PLACEHOLDER_SUCCESS: FindingKind = FindingKind {
    id: "FAILLOUD-PLACEHOLDER-SUCCESS",
    category: Category::PlaceholderImpl,
    severity: Severity::High,
    confidence: Confidence::High,
    recommendation: "Use an explicit not-implemented error until the function has real behavior.",
};

#[derive(Debug, Serialize)]
pub struct Report {
    pub files_scanned: usize,
    pub files_skipped: usize,
    pub findings: Vec<Finding>,
    pub summary_by_category: BTreeMap<String, usize>,
    pub summary_by_severity: BTreeMap<String, usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Lang {
    Rust,
    Python,
    JavaScript,
    TypeScript,
    Go,
    Elixir,
    CSharp,
    Java,
}

impl Lang {
    fn detect(path: &Path) -> Option<Self> {
        let name = path.file_name()?.to_string_lossy();
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        match ext {
            "rs" => Some(Self::Rust),
            "py" => Some(Self::Python),
            "js" | "jsx" | "mjs" | "cjs" => Some(Self::JavaScript),
            "ts" | "tsx" | "mts" | "cts" => Some(Self::TypeScript),
            "go" => Some(Self::Go),
            "ex" | "exs" => Some(Self::Elixir),
            "cs" => Some(Self::CSharp),
            "java" => Some(Self::Java),
            _ if name == "mix.exs" => Some(Self::Elixir),
            _ => None,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Rust => "rust",
            Self::Python => "python",
            Self::JavaScript => "javascript",
            Self::TypeScript => "typescript",
            Self::Go => "go",
            Self::Elixir => "elixir",
            Self::CSharp => "csharp",
            Self::Java => "java",
        }
    }

    fn language(self, path: &Path) -> Language {
        match self {
            Self::Rust => tree_sitter_rust::LANGUAGE.into(),
            Self::Python => tree_sitter_python::LANGUAGE.into(),
            Self::JavaScript => tree_sitter_javascript::LANGUAGE.into(),
            Self::TypeScript if path.extension().and_then(|e| e.to_str()) == Some("tsx") => {
                tree_sitter_typescript::LANGUAGE_TSX.into()
            }
            Self::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            Self::Go => tree_sitter_go::LANGUAGE.into(),
            Self::Elixir => tree_sitter_elixir::LANGUAGE.into(),
            Self::CSharp => tree_sitter_c_sharp::LANGUAGE.into(),
            Self::Java => tree_sitter_java::LANGUAGE.into(),
        }
    }
}

pub fn scan(paths: &[PathBuf], opts: &Options) -> Result<Report> {
    if paths.is_empty() {
        return Err(anyhow!("no scan roots provided"));
    }

    let mut files_scanned = 0usize;
    let mut files_skipped = 0usize;
    let mut findings = Vec::new();

    for root in paths {
        if !root.exists() {
            return Err(anyhow!("scan root does not exist: {}", root.display()));
        }

        if root.is_file() {
            match scan_file(root, opts)? {
                Some(file_findings) => {
                    files_scanned += 1;
                    findings.extend(file_findings);
                }
                None => files_skipped += 1,
            }
            continue;
        }

        for entry in WalkBuilder::new(root)
            .standard_filters(true)
            .build()
            .flatten()
        {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            match scan_file(path, opts)? {
                Some(file_findings) => {
                    files_scanned += 1;
                    findings.extend(file_findings);
                }
                None => files_skipped += 1,
            }
        }
    }

    findings.retain(|f| {
        f.confidence >= opts.min_confidence
            && (opts.categories.is_empty() || opts.categories.contains(&f.category))
    });

    let mut summary_by_category = BTreeMap::new();
    let mut summary_by_severity = BTreeMap::new();
    for finding in &findings {
        *summary_by_category
            .entry(finding.category.tag().to_string())
            .or_insert(0) += 1;
        *summary_by_severity
            .entry(finding.severity.tag().to_string())
            .or_insert(0) += 1;
    }

    Ok(Report {
        files_scanned,
        files_skipped,
        findings,
        summary_by_category,
        summary_by_severity,
    })
}

fn scan_file(path: &Path, opts: &Options) -> Result<Option<Vec<Finding>>> {
    let Some(lang) = Lang::detect(path) else {
        return Ok(None);
    };
    if is_test_or_fixture_path(path) {
        return Ok(None);
    }
    let Ok(source) = std::fs::read_to_string(path) else {
        return Ok(None);
    };
    let mut parser = Parser::new();
    parser.set_language(&lang.language(path))?;
    let Some(tree) = parser.parse(&source, None) else {
        return Ok(None);
    };
    if tree.root_node().has_error() {
        return Ok(None);
    }

    let mut findings = Vec::new();
    let mut scanner = AstScanner {
        lang,
        file: path.display().to_string(),
        source: &source,
        findings: &mut findings,
    };
    scanner.visit(tree.root_node());

    findings.retain(|f| {
        f.confidence >= opts.min_confidence
            && (opts.categories.is_empty() || opts.categories.contains(&f.category))
    });
    Ok(Some(findings))
}

struct AstScanner<'a> {
    lang: Lang,
    file: String,
    source: &'a str,
    findings: &'a mut Vec<Finding>,
}

impl AstScanner<'_> {
    fn visit(&mut self, node: Node<'_>) {
        self.check_node(node);

        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if child.is_named() {
                self.visit(child);
            }
        }
    }

    fn check_node(&mut self, node: Node<'_>) {
        let text = self.text(node).to_string();
        if self.inside_test_context(node) {
            return;
        }
        if contains_fail_loud_signal(&text) {
            return;
        }

        match self.lang {
            Lang::Rust => self.check_rust(node, &text),
            Lang::Python => self.check_python(node, &text),
            Lang::JavaScript | Lang::TypeScript => self.check_js_ts(node, &text),
            Lang::Go => self.check_go(node, &text),
            Lang::Elixir => self.check_elixir(node, &text),
            Lang::CSharp | Lang::Java => self.check_csharp_java(node, &text),
        }

        self.check_runtime_mock(node, &text);
        self.check_placeholder_success(node, &text);
    }

    fn check_rust(&mut self, node: Node<'_>, text: &str) {
        match node.kind() {
            "match_arm"
                if rust_match_arm_pattern_is_err(text) && returns_rust_success_default(text) =>
            {
                self.push(
                    &RUST_ERR_OK_DEFAULT,
                    node,
                    vec![
                        "match arm handles Err".into(),
                        "Err branch returns Ok/default data".into(),
                    ],
                );
            }
            "if_expression" if text.contains("Err") && returns_rust_success_default(text) => {
                self.push(
                    &RUST_IFERR_OK_DEFAULT,
                    node,
                    vec![
                        "if branch checks an Err path".into(),
                        "branch returns Ok/default data".into(),
                    ],
                );
            }
            "let_declaration" if is_rust_ignored_fallible(text) => {
                self.push(
                    &RUST_IGNORED_FALLIBLE,
                    node,
                    vec![
                        "let _ discards a fallible-looking result".into(),
                        "call result is not inspected or propagated".into(),
                    ],
                );
            }
            "call_expression" if is_rust_result_erasure(text) => {
                self.push(
                    &RUST_RESULT_ERASURE,
                    node,
                    vec![
                        "Result-like value is converted with .ok() or unwrap_or_default()".into(),
                        "error information is discarded".into(),
                    ],
                );
            }
            _ => {}
        }
    }

    fn check_python(&mut self, node: Node<'_>, text: &str) {
        if node.kind() != "except_clause" {
            return;
        }
        if text_contains_any(text, &["pass"]) || returns_dynamic_default(text) {
            self.push(
                &PY_EXCEPT_DEFAULT,
                node,
                vec![
                    "except clause catches an error".into(),
                    "handler passes or returns default/success data".into(),
                ],
            );
        }
    }

    fn check_js_ts(&mut self, node: Node<'_>, text: &str) {
        match node.kind() {
            "catch_clause" if returns_dynamic_default(text) || returns_mock_data(text) => {
                self.push(
                    &JS_CATCH_DEFAULT,
                    node,
                    vec![
                        "catch clause catches an error".into(),
                        "handler returns default, success, or mock data".into(),
                    ],
                );
            }
            "call_expression" if is_js_catch_default(text) => {
                self.push(
                    &JS_PROMISE_CATCH_DEFAULT,
                    node,
                    vec![
                        "Promise catch handler is present".into(),
                        "handler returns default/success data".into(),
                    ],
                );
            }
            _ => {}
        }
    }

    fn check_go(&mut self, node: Node<'_>, text: &str) {
        if node.kind() == "if_statement"
            && is_go_err_branch(text)
            && returns_go_success_default(text)
        {
            self.push(
                &GO_ERR_NIL_SUCCESS,
                node,
                vec![
                    "if branch checks err != nil".into(),
                    "error branch returns nil/default success values".into(),
                ],
            );
        }
    }

    fn check_elixir(&mut self, node: Node<'_>, text: &str) {
        if text.contains("{:error") && returns_elixir_ok_default(text) {
            self.push(
                &ELIXIR_ERROR_OK_DEFAULT,
                node,
                vec![
                    "branch handles an {:error, _} tuple".into(),
                    "branch returns {:ok, ...} with default data".into(),
                ],
            );
        } else if text.contains("rescue") && returns_elixir_ok_default(text) {
            self.push(
                &ELIXIR_RESCUE_OK_DEFAULT,
                node,
                vec![
                    "rescue block catches an exception".into(),
                    "rescue returns {:ok, ...} with default data".into(),
                ],
            );
        }
    }

    fn check_csharp_java(&mut self, node: Node<'_>, text: &str) {
        if node.kind() == "catch_clause"
            && (returns_csharp_java_default(text) || returns_mock_data(text))
        {
            self.push(
                &JVM_DOTNET_CATCH_DEFAULT,
                node,
                vec![
                    "catch clause catches an exception".into(),
                    "handler returns default, success, or mock data".into(),
                ],
            );
        }
    }

    fn check_runtime_mock(&mut self, node: Node<'_>, text: &str) {
        if !matches!(
            node.kind(),
            "return_statement"
                | "return_expression"
                | "lexical_declaration"
                | "variable_declaration"
                | "const_declaration"
                | "short_var_declaration"
                | "let_declaration"
                | "field_declaration"
                | "assignment_expression"
                | "call"
        ) {
            return;
        }
        if returns_mock_data(text) || declares_mock_static_data(text) {
            self.push(
                &MOCK_LEAK_RUNTIME,
                node,
                vec![
                    "runtime code references mock/fake/sample data".into(),
                    "path is not under a test, fixture, story, or example tree".into(),
                ],
            );
        }
    }

    fn check_placeholder_success(&mut self, node: Node<'_>, text: &str) {
        if !is_function_like(node.kind()) {
            return;
        }
        if has_local_placeholder_success(text) {
            self.push(
                &PLACEHOLDER_SUCCESS,
                node,
                vec![
                    "function contains TODO/stub/placeholder language".into(),
                    "function returns success/default/mock data".into(),
                ],
            );
        }
    }

    fn push(&mut self, kind: &FindingKind, node: Node<'_>, evidence: Vec<String>) {
        self.findings.push(Finding {
            id: kind.id,
            category: kind.category,
            severity: kind.severity,
            confidence: kind.confidence,
            language: self.lang.name(),
            file: self.file.clone(),
            line: node.start_position().row + 1,
            function: enclosing_function_name(node, self.source),
            snippet: first_non_empty_line(self.text(node)),
            evidence,
            recommendation: kind.recommendation,
        });
    }

    fn text(&self, node: Node<'_>) -> &str {
        &self.source[node.byte_range()]
    }

    fn inside_test_context(&self, mut node: Node<'_>) -> bool {
        loop {
            let text = self.text(node);
            match self.lang {
                Lang::Rust => {
                    if text.contains("#[test]")
                        || (node.kind() == "mod_item" && text.trim_start().starts_with("mod tests"))
                    {
                        return true;
                    }
                }
                Lang::JavaScript | Lang::TypeScript => {
                    if matches!(node.kind(), "call_expression")
                        && text_contains_any(text, &["describe(", "it(", "test("])
                    {
                        return true;
                    }
                }
                Lang::Python => {
                    if is_function_like(node.kind())
                        && first_identifier(node, self.source)
                            .is_some_and(|name| name.starts_with("test_"))
                    {
                        return true;
                    }
                }
                Lang::Go | Lang::Elixir | Lang::CSharp | Lang::Java => {}
            }

            let Some(parent) = node.parent() else {
                return false;
            };
            node = parent;
        }
    }
}

fn enclosing_function_name(mut node: Node<'_>, source: &str) -> Option<String> {
    if is_function_like(node.kind()) {
        return first_identifier(node, source).or_else(|| Some(node.kind().to_string()));
    }
    while let Some(parent) = node.parent() {
        if is_function_like(parent.kind()) {
            if let Some(name) = first_identifier(parent, source) {
                return Some(name);
            }
            return Some(parent.kind().to_string());
        }
        node = parent;
    }
    None
}

fn first_identifier(node: Node<'_>, source: &str) -> Option<String> {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if matches!(
            child.kind(),
            "identifier" | "property_identifier" | "field_identifier"
        ) {
            return Some(source[child.byte_range()].to_string());
        }
        if child.is_named() {
            if let Some(id) = first_identifier(child, source) {
                return Some(id);
            }
        }
    }
    None
}

fn is_function_like(kind: &str) -> bool {
    matches!(
        kind,
        "function_item"
            | "function_definition"
            | "function_declaration"
            | "method_definition"
            | "method_declaration"
            | "method_elem"
            | "public_method_definition"
            | "private_method_definition"
            | "anonymous_function"
            | "arrow_function"
            | "lambda_expression"
            | "constructor_declaration"
    )
}

fn is_test_or_fixture_path(path: &Path) -> bool {
    let path_s = path.to_string_lossy().to_ascii_lowercase();
    let file = path
        .file_name()
        .map(|f| f.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    let suppressed_segments = [
        "/test/",
        "/tests/",
        "/spec/",
        "/specs/",
        "/fixture/",
        "/fixtures/",
        "/__mocks__/",
        "/stories/",
        "/storybook/",
        "/examples/",
        "/sandbox/",
        "/dev/",
    ];
    suppressed_segments
        .iter()
        .any(|segment| path_s.contains(segment))
        || file.ends_with("_test.go")
        || file.ends_with("_test.rs")
        || file.ends_with(".spec.ts")
        || file.ends_with(".spec.tsx")
        || file.ends_with(".test.ts")
        || file.ends_with(".test.tsx")
        || file.ends_with(".spec.js")
        || file.ends_with(".test.js")
        || file.ends_with("_test.exs")
        || file.ends_with("test.exs")
}

fn first_non_empty_line(text: &str) -> String {
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("")
        .chars()
        .take(220)
        .collect()
}

fn text_contains_any(text: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| text.contains(needle))
}

fn lower_contains_any(text: &str, needles: &[&str]) -> bool {
    let lower = text.to_ascii_lowercase();
    needles.iter().any(|needle| lower.contains(needle))
}

fn contains_fail_loud_signal(text: &str) -> bool {
    lower_contains_any(
        text,
        &[
            "notimplemented",
            "not implemented",
            "unsupportedoperation",
            "todo!()",
            "unimplemented!()",
            "panic!(\"not implemented",
            "raise notimplementederror",
            "throw new error(\"not implemented",
            "throw new notimplemented",
            "errors.new(\"not implemented",
            "{:error, :not_implemented}",
        ],
    )
}

fn has_placeholder_comment(text: &str) -> bool {
    text.lines().any(|line| {
        let trimmed = line.trim_start();
        let is_comment = trimmed.starts_with("//")
            || trimmed.starts_with('#')
            || trimmed.starts_with("/*")
            || trimmed.starts_with('*')
            || trimmed.starts_with("--");
        is_comment && contains_placeholder_word(trimmed)
    })
}

fn contains_placeholder_word(text: &str) -> bool {
    text.split(|ch: char| !ch.is_ascii_alphanumeric() && ch != '_')
        .any(|word| {
            matches!(
                word.to_ascii_lowercase().as_str(),
                "todo" | "fixme" | "stub" | "placeholder" | "temporary"
            )
        })
}

fn has_local_placeholder_success(text: &str) -> bool {
    let lines: Vec<&str> = text.lines().collect();
    for (idx, line) in lines.iter().enumerate() {
        if !has_placeholder_comment(line) {
            continue;
        }
        let start = idx.saturating_sub(3);
        let end = (idx + 8).min(lines.len());
        let window = lines[start..end].join("\n");
        if returns_dynamic_default(&window)
            || returns_rust_success_default(&window)
            || returns_go_success_default(&window)
            || returns_elixir_ok_default(&window)
            || returns_csharp_java_default(&window)
            || returns_mock_data(&window)
        {
            return true;
        }
    }
    false
}

fn returns_dynamic_default(text: &str) -> bool {
    lower_contains_any(
        text,
        &[
            "return []",
            "return {}",
            "return none",
            "return null",
            "return true",
            "return {\"success\": true",
            "return {'success': true",
            "return { success: true",
            "return promise.resolve([]",
            "return promise.resolve({",
            "pass",
        ],
    )
}

fn returns_rust_success_default(text: &str) -> bool {
    text_contains_any(
        text,
        &[
            "Ok(())",
            "Ok(vec![])",
            "Ok(Vec::new())",
            "Ok(Default::default())",
            "Ok(None)",
            "Ok([])",
            "Ok(true)",
            "return Ok(",
        ],
    ) && lower_contains_any(
        text,
        &[
            "err",
            "error",
            "vec![]",
            "vec::new",
            "default::default",
            "none",
            "ok(())",
        ],
    )
}

fn rust_match_arm_pattern_is_err(text: &str) -> bool {
    text.split("=>")
        .next()
        .is_some_and(|pattern| pattern.contains("Err(") || pattern.trim_start().starts_with("Err"))
}

fn is_rust_ignored_fallible(text: &str) -> bool {
    text.trim_start().starts_with("let _")
        && text.contains('=')
        && text.contains('(')
        && lower_contains_any(
            text,
            &[
                ".send(",
                ".write",
                ".flush",
                ".sync",
                ".commit",
                ".rollback",
                ".execute",
                ".request",
                ".call",
                ".save",
                ".delete",
                ".remove",
                ".insert",
                ".update",
            ],
        )
}

fn is_rust_result_erasure(text: &str) -> bool {
    text_contains_any(text, &[".ok()", ".unwrap_or_default()"])
        && lower_contains_any(
            text,
            &[
                ".send(", ".write", ".flush", ".sync", ".commit", ".execute", ".request", ".call",
                ".save", ".delete", ".insert", ".update",
            ],
        )
}

fn is_js_catch_default(text: &str) -> bool {
    text.contains(".catch")
        && (returns_dynamic_default(text)
            || text_contains_any(text, &["=> []", "=> ({", "=> null", "=> true"]))
}

fn is_go_err_branch(text: &str) -> bool {
    text_contains_any(text, &["err != nil", "nil != err"])
}

fn returns_go_success_default(text: &str) -> bool {
    lower_contains_any(
        text,
        &[
            "return nil",
            "return nil, nil",
            "return []",
            "return true, nil",
            "return false, nil",
            "return \"\", nil",
            "return 0, nil",
        ],
    )
}

fn returns_elixir_ok_default(text: &str) -> bool {
    text_contains_any(
        text,
        &[
            "{:ok, []}",
            "{:ok, %{}}",
            "{:ok, nil}",
            "{:ok, true}",
            "{:ok, \"\"}",
        ],
    )
}

fn returns_csharp_java_default(text: &str) -> bool {
    lower_contains_any(
        text,
        &[
            "return ok(",
            "return new list",
            "return collections.emptylist",
            "return enumerable.empty",
            "return task.completedtask",
            "return null",
            "return true",
            "return false",
        ],
    )
}

fn returns_mock_data(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.contains("return")
        && lower_contains_any(
            &lower,
            &[
                "mock",
                "fake",
                "sample",
                "dummy",
                "placeholder",
                "stub",
                "demo",
            ],
        )
}

fn declares_mock_static_data(text: &str) -> bool {
    lower_contains_any(
        text,
        &[
            "mockdata",
            "mock_data",
            "fakeusers",
            "fake_users",
            "sampleorders",
            "sample_orders",
            "dummydata",
            "dummy_data",
            "placeholderdata",
            "placeholder_data",
        ],
    ) && text_contains_any(text, &["[", "{", "vec![", "new List"])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scan_one(name: &str, src: &str, min_confidence: Confidence) -> Vec<Finding> {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join(name);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, src).unwrap();
        let report = scan(
            &[file],
            &Options {
                min_confidence,
                ..Options::default()
            },
        )
        .unwrap();
        report.findings
    }

    #[test]
    fn rust_err_to_ok_default_is_reported() {
        let src = r#"
fn load() -> Result<Vec<String>, Error> {
    match db.fetch() {
        Ok(v) => Ok(v),
        Err(_) => Ok(vec![]),
    }
}
"#;
        let findings = scan_one("src/lib.rs", src, Confidence::High);
        assert!(findings
            .iter()
            .any(|f| f.id == "FAILLOUD-RUST-ERR-OK-DEFAULT"));
    }

    #[test]
    fn rust_outer_match_arm_is_not_blamed_for_nested_err_arm() {
        let src = r#"
fn hook() -> Result<(), Error> {
    match command {
        Commands::Hook => {
            match parse() {
                Ok(v) => v,
                Err(_) => return Ok(()),
            };
        }
    }
    Ok(())
}
"#;
        let findings = scan_one("src/lib.rs", src, Confidence::High);
        assert_eq!(
            findings
                .iter()
                .filter(|f| f.id == "FAILLOUD-RUST-ERR-OK-DEFAULT")
                .count(),
            1
        );
        assert_eq!(findings[0].snippet, "Err(_) => return Ok(()),");
    }

    #[test]
    fn python_except_default_is_reported() {
        let src = r#"
def users():
    try:
        return client.fetch()
    except Exception:
        return []
"#;
        let findings = scan_one("app.py", src, Confidence::High);
        assert!(findings
            .iter()
            .any(|f| f.id == "FAILLOUD-PY-EXCEPT-DEFAULT"));
    }

    #[test]
    fn javascript_catch_default_is_reported() {
        let src = r#"
export async function users() {
  try {
    return await api.users()
  } catch (e) {
    console.error(e)
    return []
  }
}
"#;
        let findings = scan_one("app.ts", src, Confidence::High);
        assert!(findings.iter().any(|f| f.id == "FAILLOUD-JS-CATCH-DEFAULT"));
    }

    #[test]
    fn go_err_nil_success_is_reported() {
        let src = r#"
package main
func users() ([]string, error) {
    rows, err := db.Users()
    if err != nil {
        return []string{}, nil
    }
    return rows, nil
}
"#;
        let findings = scan_one("main.go", src, Confidence::High);
        assert!(findings
            .iter()
            .any(|f| f.id == "FAILLOUD-GO-ERR-NIL-SUCCESS"));
    }

    #[test]
    fn csharp_catch_default_is_reported() {
        let src = r#"
class Users {
  public List<string> Load() {
    try { return repo.Load(); }
    catch (Exception e) { return new List<string>(); }
  }
}
"#;
        let findings = scan_one("Users.cs", src, Confidence::High);
        assert!(findings
            .iter()
            .any(|f| f.id == "FAILLOUD-JVM-DOTNET-CATCH-DEFAULT"));
    }

    #[test]
    fn fail_loud_not_implemented_is_not_reported() {
        let src = r#"
def users():
    raise NotImplementedError("wire real store")
"#;
        let findings = scan_one("app.py", src, Confidence::High);
        assert!(findings.is_empty(), "{findings:?}");
    }

    #[test]
    fn placeholder_strings_do_not_count_as_comment_evidence() {
        let src = r#"
fn words() -> Vec<&'static str> {
    vec!["todo", "mock", "placeholder"]
}
"#;
        let findings = scan_one("src/lib.rs", src, Confidence::High);
        assert!(findings.is_empty(), "{findings:?}");
    }

    #[test]
    fn placeholder_comment_with_success_return_is_reported() {
        let src = r#"
def users():
    # TODO: wire the database
    return []
"#;
        let findings = scan_one("app.py", src, Confidence::High);
        assert!(findings
            .iter()
            .any(|f| f.id == "FAILLOUD-PLACEHOLDER-SUCCESS"));
    }

    #[test]
    fn mock_data_in_tests_is_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("tests/users_test.ts");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, "export function users() { return mockUsers }").unwrap();
        let report = scan(&[dir.path().to_path_buf()], &Options::default()).unwrap();
        assert!(report.findings.is_empty());
        assert_eq!(report.files_scanned, 0);
    }

    #[test]
    fn rust_inline_tests_are_ignored() {
        let src = r#"
#[cfg(test)]
mod tests {
    #[test]
    fn placeholder_fixture() {
        // TODO: fixture data
        let _value = vec!["mock"];
    }
}
"#;
        let findings = scan_one("src/lib.rs", src, Confidence::High);
        assert!(findings.is_empty(), "{findings:?}");
    }
}
