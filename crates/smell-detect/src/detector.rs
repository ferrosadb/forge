//! Code smell detection using line/brace/regex heuristics.
//!
//! Operates on individual source files. Use with `ignore` crate for directory walks.

use regex::Regex;
use serde::Serialize;

#[derive(Debug, Serialize, PartialEq)]
pub struct SmellReport {
    pub file: String,
    pub total_lines: usize,
    pub smells: Vec<Smell>,
}

#[derive(Debug, Serialize, PartialEq, Clone)]
pub struct Smell {
    pub kind: SmellKind,
    pub line: usize,
    pub function: String,
    pub detail: String,
}

#[derive(Debug, Serialize, PartialEq, Clone)]
pub enum SmellKind {
    LongFunction,
    HighComplexity,
    DeepNesting,
    LargeParameterList,
    TodoFixme,
    /// Multi-line string that looks like an LLM prompt embedded in code.
    HardcodedPrompt,
    /// Dated or specific model ID that should be configurable.
    HardcodedModelId,
    /// Any non-localhost URL hardcoded instead of externalized.
    HardcodedUrl,
    /// Tunable parameter (timeout, retry count, threshold, max limit, port, etc.).
    HardcodedTunable,
    /// Email address embedded in source code.
    HardcodedEmail,
    /// Pattern that looks like an API key, token, or secret.
    HardcodedSecret,
}

#[derive(Debug, Clone)]
pub struct DetectConfig {
    pub max_function_lines: usize,
    pub max_cc: usize,
    pub max_nesting: usize,
    pub max_params: usize,
}

impl Default for DetectConfig {
    fn default() -> Self {
        Self {
            max_function_lines: 60,
            max_cc: 15,
            max_nesting: 4,
            max_params: 5,
        }
    }
}

/// Detect smells in a single source file.
pub fn detect(filename: &str, source: &str, config: &DetectConfig) -> SmellReport {
    let lines: Vec<&str> = source.lines().collect();
    let total_lines = lines.len();
    let mut smells = Vec::new();

    let functions = find_functions(&lines);

    for func in &functions {
        let func_lines = func.end_line - func.start_line + 1;

        // Long function
        if func_lines > config.max_function_lines {
            smells.push(Smell {
                kind: SmellKind::LongFunction,
                line: func.start_line,
                function: func.name.clone(),
                detail: format!("{func_lines} lines (max {})", config.max_function_lines),
            });
        }

        // High complexity
        let func_source: String = lines[func.start_line - 1..func.end_line]
            .to_vec()
            .join("\n");
        let cc = compute_cc(&func_source);
        if cc > config.max_cc {
            smells.push(Smell {
                kind: SmellKind::HighComplexity,
                line: func.start_line,
                function: func.name.clone(),
                detail: format!("CC={cc} (max {})", config.max_cc),
            });
        }

        // Deep nesting
        let max_depth = compute_max_nesting(&func_source);
        if max_depth > config.max_nesting {
            smells.push(Smell {
                kind: SmellKind::DeepNesting,
                line: func.start_line,
                function: func.name.clone(),
                detail: format!("depth={max_depth} (max {})", config.max_nesting),
            });
        }

        // Large parameter list
        if func.param_count > config.max_params {
            smells.push(Smell {
                kind: SmellKind::LargeParameterList,
                line: func.start_line,
                function: func.name.clone(),
                detail: format!("{} params (max {})", func.param_count, config.max_params),
            });
        }
    }

    // TODO/FIXME markers
    for (i, line) in lines.iter().enumerate() {
        let upper = line.to_uppercase();
        if upper.contains("TODO") || upper.contains("FIXME") || upper.contains("HACK") {
            smells.push(Smell {
                kind: SmellKind::TodoFixme,
                line: i + 1,
                function: String::new(),
                detail: line.trim().to_string(),
            });
        }
    }

    // 12-factor config smells: hardcoded prompts, model IDs, API URLs
    detect_config_smells(&lines, &mut smells);

    SmellReport {
        file: filename.to_string(),
        total_lines,
        smells,
    }
}

#[derive(Debug)]
struct FunctionInfo {
    name: String,
    start_line: usize,
    end_line: usize,
    param_count: usize,
}

fn find_functions(lines: &[&str]) -> Vec<FunctionInfo> {
    // Matches function definitions across Rust, Python, Go, JS/TS, Elixir
    let func_re = Regex::new(
        r"(?x)
        (?:pub\s+)?(?:async\s+)?(?:fn|func|def|function)\s+(\w+)\s*\(([^)]*)\)  # rust/python/go/js
        | (?:defp?\s+)(\w+)\s*\(([^)]*)\)  # elixir
    ",
    )
    .unwrap();

    // C#/Java-style: access modifier + optional modifiers + return type + name(params)
    let csharp_re = Regex::new(
        r"(?x)
        ^\s*(?:public|private|internal|protected)\s+
        (?:(?:static|virtual|override|abstract|async|sealed|new)\s+)*
        (?:[\w<>\[\]?,]+\s+)  # return type
        (\w+)\s*\(([^)]*)\)   # name and params
    ",
    )
    .unwrap();

    let mut functions = Vec::new();

    for (i, line) in lines.iter().enumerate() {
        if let Some(cap) = func_re.captures(line) {
            let name = cap
                .get(1)
                .or(cap.get(3))
                .map_or("unknown", |m| m.as_str())
                .to_string();
            let params_str = cap.get(2).or(cap.get(4)).map_or("", |m| m.as_str());

            let param_count = if params_str.trim().is_empty() {
                0
            } else {
                params_str.split(',').count()
            };

            // Find end of function by brace matching or indentation
            let end_line = find_function_end(lines, i);

            functions.push(FunctionInfo {
                name,
                start_line: i + 1,
                end_line: end_line + 1,
                param_count,
            });
        } else if let Some(cap) = csharp_re.captures(line) {
            let name = cap[1].to_string();
            // Skip type declarations (class, struct, interface, etc.)
            if matches!(
                name.as_str(),
                "class"
                    | "struct"
                    | "interface"
                    | "record"
                    | "enum"
                    | "namespace"
                    | "if"
                    | "for"
                    | "while"
                    | "switch"
                    | "catch"
                    | "return"
            ) {
                continue;
            }
            let params_str = cap.get(2).map_or("", |m| m.as_str());
            let param_count = if params_str.trim().is_empty() {
                0
            } else {
                params_str.split(',').count()
            };
            let end_line = find_function_end(lines, i);
            functions.push(FunctionInfo {
                name,
                start_line: i + 1,
                end_line: end_line + 1,
                param_count,
            });
        }
    }

    functions
}

fn find_function_end(lines: &[&str], start: usize) -> usize {
    let mut brace_depth = 0;
    let mut found_open = false;

    for (i, line) in lines.iter().enumerate().skip(start) {
        for ch in line.chars() {
            if ch == '{' {
                brace_depth += 1;
                found_open = true;
            } else if ch == '}' {
                brace_depth -= 1;
                if found_open && brace_depth == 0 {
                    return i;
                }
            }
        }
    }

    // For indentation-based languages (Python, Elixir), use indentation
    if !found_open && start < lines.len() {
        let base_indent = lines[start].len() - lines[start].trim_start().len();
        for (i, line) in lines.iter().enumerate().skip(start + 1) {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let indent = line.len() - trimmed.len();
            if indent <= base_indent {
                return i.saturating_sub(1);
            }
        }
    }

    // Fallback: end of file
    lines.len().saturating_sub(1)
}

fn compute_cc(source: &str) -> usize {
    let branch_re = Regex::new(
        r"(?x)
        \b(if|else\s+if|elif|while|for|case|catch|except|rescue)\b
        | \?\?
        | &&
        | \|\|
    ",
    )
    .unwrap();

    1 + branch_re.captures_iter(source).count()
}

fn compute_max_nesting(source: &str) -> usize {
    let mut max_depth: usize = 0;
    let mut current_depth: usize = 0;

    for ch in source.chars() {
        if ch == '{' {
            current_depth += 1;
            if current_depth > max_depth {
                max_depth = current_depth;
            }
        } else if ch == '}' {
            current_depth = current_depth.saturating_sub(1);
        }
    }

    max_depth
}

// ---------------------------------------------------------------------------
// 12-factor config smell detection
//
// Principle: code should contain NO deployment-specific or tunable values.
// All configuration belongs in environment variables (.env), config files,
// or a database config table.
// ---------------------------------------------------------------------------

/// Returns true if a line looks like a comment (simple heuristic).
fn is_comment(trimmed: &str) -> bool {
    trimmed.starts_with("//")
        || trimmed.starts_with('#')
        || trimmed.starts_with('%')
        || trimmed.starts_with('*')
        || trimmed.starts_with("/*")
        || trimmed.starts_with("<!--")
}

/// Detect hardcoded configuration that should be externalized per 12-factor.
fn detect_config_smells(lines: &[&str], smells: &mut Vec<Smell>) {
    detect_hardcoded_model_ids(lines, smells);
    detect_hardcoded_urls(lines, smells);
    detect_hardcoded_prompts(lines, smells);
    detect_hardcoded_tunables(lines, smells);
    detect_hardcoded_emails(lines, smells);
    detect_hardcoded_secrets(lines, smells);
}

/// Detect dated LLM model IDs (e.g., `claude-sonnet-4-5-20250929`).
fn detect_hardcoded_model_ids(lines: &[&str], smells: &mut Vec<Smell>) {
    let model_re =
        Regex::new(r#"["'](?:claude|gpt|gemini|o[1-4]|codex)[-\w]*?-\d{8}["']"#).unwrap();

    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        if is_comment(trimmed) {
            continue;
        }
        if let Some(m) = model_re.find(trimmed) {
            smells.push(Smell {
                kind: SmellKind::HardcodedModelId,
                line: i + 1,
                function: String::new(),
                detail: format!(
                    "Dated model ID {} — move to env var (e.g. MODEL_ID) or .env file",
                    m.as_str()
                ),
            });
        }
    }
}

/// Detect any non-localhost URL hardcoded in source.
///
/// Catches: API endpoints, deployment URLs, CDN URLs, webhook URLs, WebSocket URLs.
/// Ignores: localhost, 127.0.0.1, example.com, schema.org, w3.org, and similar.
fn detect_hardcoded_urls(lines: &[&str], smells: &mut Vec<Smell>) {
    let url_re = Regex::new(r#"["'](https?://[^\s"']+|wss?://[^\s"']+)"#).unwrap();

    // Allowlist: URLs that are safe to hardcode (specs, schemas, localhost)
    let allowlist = [
        "localhost",
        "127.0.0.1",
        "0.0.0.0",
        "::1",
        "example.com",
        "example.org",
        "schema.org",
        "w3.org",
        "www.w3.org",
        "json-schema.org",
        "xml.org",
        "xmlns.com",
        "purl.org",
        "schemas.microsoft.com",
        "tools.ietf.org",
        "semver.org",
        "spdx.org",
        "creativecommons.org",
        "opensource.org",
        "spec.openapis.org",
        "github.com",
        "raw.githubusercontent.com",
        "hex.pm",
        "crates.io",
        "npmjs.com",
        "pypi.org",
        "pkg.go.dev",
        "docs.rs",
        "hexdocs.pm",
        "rubygems.org",
        "mvnrepository.com",
    ];

    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        if is_comment(trimmed) {
            continue;
        }
        for cap in url_re.captures_iter(trimmed) {
            let url = cap.get(1).unwrap().as_str();
            // Skip allowlisted domains
            let is_allowed = allowlist.iter().any(|domain| url.contains(domain));
            if is_allowed {
                continue;
            }
            smells.push(Smell {
                kind: SmellKind::HardcodedUrl,
                line: i + 1,
                function: String::new(),
                detail: format!(
                    "Hardcoded URL \"{}\" — externalize to env var or .env file",
                    url
                ),
            });
        }
    }
}

/// Detect tunable numeric parameters assigned to config-like variable names.
///
/// Catches: timeouts, retry counts, max limits, thresholds, batch sizes,
/// port numbers, scoring weights, token limits, temperatures, intervals.
fn detect_hardcoded_tunables(lines: &[&str], smells: &mut Vec<Smell>) {
    // Pattern: config-like variable name followed by assignment to a number.
    // Covers: `timeout = 30`, `@max_tokens 4096`, `MAX_RETRIES: 3`, `let threshold = 0.7`
    let tunable_re = Regex::new(
        r"(?xi)
        (?:
            (?:let|const|var|val|def|@|pub\s+(?:const|static))\s+  # declaration keyword
            |                                                       # or just a bare assignment
            ^[ \t]*@?                                               # module attribute or start
        )
        (?P<name>[A-Za-z_]\w*)                                     # variable name
        \s*[=:\s]\s*                                                # assignment (= : or space for @attr)
        (?P<value>-?\d+(?:\.\d+)?)                                 # numeric literal
    ",
    )
    .unwrap();

    // Names that suggest tunables (case-insensitive substrings)
    let tunable_names = [
        "timeout",
        "retry",
        "retries",
        "max_",
        "min_",
        "limit",
        "threshold",
        "batch",
        "size",
        "count",
        "interval",
        "delay",
        "duration",
        "port",
        "rate",
        "weight",
        "score",
        "temperature",
        "penalty",
        "tokens",
        "chunk",
        "page_size",
        "pool",
        "ttl",
        "expire",
        "expiry",
        "capacity",
        "concurrency",
        "parallelism",
        "workers",
        "attempts",
        "backoff",
        "jitter",
        "cooldown",
        "frequency",
        "priority",
        "depth",
        "width",
        "height",
        "radius",
        "factor",
        "multiplier",
        "ratio",
        "percent",
        "overlap",
        "top_p",
        "top_k",
    ];

    // Also match ALL_CAPS_NAMES that look like constants (e.g., BATCH_SIZE = 100)
    let const_re = Regex::new(
        r"(?m)^[ \t]*(?:pub\s+)?(?:const|static|let|var)?\s*(?P<name>[A-Z][A-Z0-9_]{2,})\s*[=:]\s*(?P<value>-?\d+(?:\.\d+)?)\s*[,;)}\]#/\n]?"
    ).unwrap();

    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        if is_comment(trimmed) {
            continue;
        }

        // Check named tunable pattern
        if let Some(cap) = tunable_re.captures(trimmed) {
            let name = cap.name("name").unwrap().as_str();
            let value = cap.name("value").unwrap().as_str();
            let name_lower = name.to_lowercase();

            let is_tunable = tunable_names.iter().any(|kw| name_lower.contains(kw));

            if is_tunable {
                // Skip trivially obvious non-config values (0, 1 for flags)
                if value == "0" || value == "1" {
                    continue;
                }
                smells.push(Smell {
                    kind: SmellKind::HardcodedTunable,
                    line: i + 1,
                    function: String::new(),
                    detail: format!(
                        "Tunable `{name} = {value}` — externalize to env var \
                         (e.g. {}) or .env file",
                        name.to_uppercase()
                    ),
                });
                continue; // Don't double-match on const pattern
            }
        }

        // Check ALL_CAPS constant assignment with numeric value
        if let Some(cap) = const_re.captures(trimmed) {
            let name = cap.name("name").unwrap().as_str();
            let value = cap.name("value").unwrap().as_str();
            // Skip trivial 0/1
            if value == "0" || value == "1" {
                continue;
            }
            // Skip things that are clearly not config (EXIT_SUCCESS, etc.)
            let name_lower = name.to_lowercase();
            if name_lower.contains("exit") || name_lower.contains("version") {
                continue;
            }
            smells.push(Smell {
                kind: SmellKind::HardcodedTunable,
                line: i + 1,
                function: String::new(),
                detail: format!(
                    "Constant `{name} = {value}` — consider env var or .env file",
                    name = name,
                    value = value
                ),
            });
        }
    }
}

/// Detect hardcoded email addresses in source code.
fn detect_hardcoded_emails(lines: &[&str], smells: &mut Vec<Smell>) {
    let email_re = Regex::new(r#"["'][a-zA-Z0-9._%+-]+@[a-zA-Z0-9.-]+\.[a-zA-Z]{2,}["']"#).unwrap();

    // Ignore test/example emails
    let ignore_domains = ["example.com", "example.org", "test.com", "placeholder"];

    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        if is_comment(trimmed) {
            continue;
        }
        if let Some(m) = email_re.find(trimmed) {
            let email = m.as_str();
            let is_ignored = ignore_domains.iter().any(|d| email.contains(d));
            if !is_ignored {
                smells.push(Smell {
                    kind: SmellKind::HardcodedEmail,
                    line: i + 1,
                    function: String::new(),
                    detail: format!(
                        "Hardcoded email {} — externalize to env var or .env file",
                        email
                    ),
                });
            }
        }
    }
}

/// Detect patterns that look like API keys, tokens, or secrets.
///
/// Catches: long hex/base64 strings assigned to key/token/secret variables,
/// and strings that look like API key formats (sk-..., pk_..., etc.).
fn detect_hardcoded_secrets(lines: &[&str], smells: &mut Vec<Smell>) {
    // Known API key prefixes
    let key_prefix_re = Regex::new(
        r#"["'](?:sk-|pk_|sk_|rk_|whsec_|xoxb-|xoxp-|ghp_|gho_|glpat-|AKIA)[A-Za-z0-9_-]{10,}["']"#,
    )
    .unwrap();

    // Variable named key/token/secret/password assigned a non-empty string
    let secret_var_re = Regex::new(
        r#"(?xi)
        (?:api_?key|secret_?key|auth_?token|access_?token|password|api_?secret|private_?key)
        \s*[=:]\s*
        ["'][^"']{8,}["']
    "#,
    )
    .unwrap();

    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        if is_comment(trimmed) {
            continue;
        }
        // Skip lines that read from env (the correct pattern)
        let lower = trimmed.to_lowercase();
        if lower.contains("env")
            || lower.contains("getenv")
            || lower.contains("os.environ")
            || lower.contains("system.get_env")
            || lower.contains("process.env")
            || lower.contains("dotenv")
        {
            continue;
        }

        if let Some(m) = key_prefix_re.find(trimmed) {
            smells.push(Smell {
                kind: SmellKind::HardcodedSecret,
                line: i + 1,
                function: String::new(),
                detail: format!(
                    "Possible API key {} — NEVER commit secrets; use env vars",
                    m.as_str()
                ),
            });
        } else if secret_var_re.is_match(trimmed) {
            smells.push(Smell {
                kind: SmellKind::HardcodedSecret,
                line: i + 1,
                function: String::new(),
                detail: "Secret assigned inline — use env var or secrets manager".to_string(),
            });
        }
    }
}

/// Detect multi-line strings that look like LLM prompts embedded in code.
///
/// Heuristic: a string spanning 5+ lines containing prompt-like keywords
/// (e.g., "you are", "system prompt", "instructions", "respond", "assistant").
fn detect_hardcoded_prompts(lines: &[&str], smells: &mut Vec<Smell>) {
    let prompt_keywords = [
        "you are",
        "you must",
        "your role",
        "system prompt",
        "instructions:",
        "respond with",
        "as an ai",
        "as a helpful",
        "do not ",
        "always ",
    ];

    // Track multi-line strings: heredocs, triple-quotes, Elixir ~S/sigils
    let mut in_multiline = false;
    let mut multiline_start: usize = 0;
    let mut multiline_content = String::new();
    let mut keyword_hits = 0;

    let heredoc_start_re = Regex::new(r#"(?:"""|'''|~[sS]"""|\\"\\"\\")"#).unwrap();
    let heredoc_end_re = Regex::new(r#"(?:"""|''')"#).unwrap();

    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim();

        if !in_multiline {
            if heredoc_start_re.is_match(trimmed) {
                in_multiline = true;
                multiline_start = i;
                multiline_content.clear();
                keyword_hits = 0;
            }
        } else {
            multiline_content.push_str(trimmed);
            multiline_content.push('\n');
            let lower = trimmed.to_lowercase();
            for kw in &prompt_keywords {
                if lower.contains(kw) {
                    keyword_hits += 1;
                }
            }

            if heredoc_end_re.is_match(trimmed) && i > multiline_start {
                let line_count = i - multiline_start + 1;
                if line_count >= 5 && keyword_hits >= 2 {
                    smells.push(Smell {
                        kind: SmellKind::HardcodedPrompt,
                        line: multiline_start + 1,
                        function: String::new(),
                        detail: format!(
                            "{line_count}-line string with {keyword_hits} prompt keywords — \
                             externalize to config, .env file, or database"
                        ),
                    });
                }
                in_multiline = false;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // === Test list (TDD) ===
    // [x] empty file produces no smells
    // [x] detects long function
    // [x] does not flag short function
    // [x] detects high complexity
    // [x] detects deep nesting
    // [x] detects large parameter list
    // [x] detects TODO/FIXME comments
    // [x] handles multiple smells in one file
    // [x] handles Python-style functions
    // [x] configurable thresholds

    #[test]
    fn empty_file() {
        let result = detect("test.rs", "", &DetectConfig::default());
        assert!(result.smells.is_empty());
    }

    #[test]
    fn detects_long_function() {
        let mut source = String::from("fn long_func() {\n");
        for i in 0..70 {
            source.push_str(&format!("    let x{i} = {i};\n"));
        }
        source.push_str("}\n");

        let result = detect("test.rs", &source, &DetectConfig::default());
        let long = result
            .smells
            .iter()
            .find(|s| s.kind == SmellKind::LongFunction);
        assert!(long.is_some());
    }

    #[test]
    fn short_function_no_smell() {
        let source = "fn short() {\n    println!(\"hello\");\n}\n";
        let result = detect("test.rs", source, &DetectConfig::default());
        let long = result
            .smells
            .iter()
            .find(|s| s.kind == SmellKind::LongFunction);
        assert!(long.is_none());
    }

    #[test]
    fn detects_high_complexity() {
        let mut source = String::from("fn complex(x: i32) {\n");
        for i in 0..20 {
            source.push_str(&format!("    if x == {i} {{ return; }}\n"));
        }
        source.push_str("}\n");

        let result = detect("test.rs", &source, &DetectConfig::default());
        let complex = result
            .smells
            .iter()
            .find(|s| s.kind == SmellKind::HighComplexity);
        assert!(complex.is_some());
    }

    #[test]
    fn detects_deep_nesting() {
        let source = r#"fn deep() {
    if true {
        if true {
            if true {
                if true {
                    if true {
                        println!("deep");
                    }
                }
            }
        }
    }
}"#;
        let result = detect("test.rs", source, &DetectConfig::default());
        let deep = result
            .smells
            .iter()
            .find(|s| s.kind == SmellKind::DeepNesting);
        assert!(deep.is_some());
    }

    #[test]
    fn detects_large_params() {
        let source =
            "fn many_params(a: i32, b: i32, c: i32, d: i32, e: i32, f: i32, g: i32) {\n}\n";
        let result = detect("test.rs", source, &DetectConfig::default());
        let params = result
            .smells
            .iter()
            .find(|s| s.kind == SmellKind::LargeParameterList);
        assert!(params.is_some());
    }

    #[test]
    fn detects_todo_fixme() {
        let source = "fn foo() {\n    // TODO: fix this later\n    // FIXME: broken\n}\n";
        let result = detect("test.rs", source, &DetectConfig::default());
        let todos: Vec<_> = result
            .smells
            .iter()
            .filter(|s| s.kind == SmellKind::TodoFixme)
            .collect();
        assert_eq!(todos.len(), 2);
    }

    #[test]
    fn multiple_smells() {
        let mut source = String::from("fn bad(a: i32, b: i32, c: i32, d: i32, e: i32, f: i32) {\n");
        for i in 0..70 {
            source.push_str(&format!("    if true {{ let x{i} = {i}; }}\n"));
        }
        source.push_str("    // TODO: refactor\n}\n");

        let result = detect("test.rs", &source, &DetectConfig::default());
        assert!(result.smells.len() >= 3); // long + complex + params + todo
    }

    #[test]
    fn python_function() {
        let source = "def process(a, b, c, d, e, f, g):\n    pass\n";
        let result = detect("test.py", source, &DetectConfig::default());
        let params = result
            .smells
            .iter()
            .find(|s| s.kind == SmellKind::LargeParameterList);
        assert!(params.is_some());
    }

    #[test]
    fn csharp_detects_long_method() {
        let mut source = String::from("public void LongMethod(int x)\n{\n");
        for i in 0..70 {
            source.push_str(&format!("    var x{i} = {i};\n"));
        }
        source.push_str("}\n");
        let result = detect("test.cs", &source, &DetectConfig::default());
        let long = result
            .smells
            .iter()
            .find(|s| s.kind == SmellKind::LongFunction);
        assert!(long.is_some(), "should detect long C# method");
        assert_eq!(long.unwrap().function, "LongMethod");
    }

    #[test]
    fn csharp_detects_large_params() {
        let source = "public void Process(int a, int b, int c, int d, int e, int f, int g)\n{\n}\n";
        let result = detect("test.cs", source, &DetectConfig::default());
        let params = result
            .smells
            .iter()
            .find(|s| s.kind == SmellKind::LargeParameterList);
        assert!(params.is_some(), "should detect large C# parameter list");
    }

    #[test]
    fn csharp_short_method_no_smell() {
        let source = "public int GetCount()\n{\n    return 42;\n}\n";
        let result = detect("test.cs", source, &DetectConfig::default());
        let long = result
            .smells
            .iter()
            .find(|s| s.kind == SmellKind::LongFunction);
        assert!(long.is_none());
    }

    #[test]
    fn configurable_thresholds() {
        let source = "fn small() {\n    if true { println!(\"a\"); }\n}\n";
        let strict = DetectConfig {
            max_function_lines: 1,
            max_cc: 1,
            max_nesting: 0,
            max_params: 0,
        };
        let result = detect("test.rs", source, &strict);
        assert!(!result.smells.is_empty());
    }

    // === 12-factor config smell tests ===

    #[test]
    fn detects_hardcoded_model_id() {
        let source = r#"
let model = "claude-sonnet-4-5-20250929";
"#;
        let result = detect("test.rs", source, &DetectConfig::default());
        let found = result
            .smells
            .iter()
            .find(|s| s.kind == SmellKind::HardcodedModelId);
        assert!(found.is_some(), "should detect dated model ID");
        assert!(found.unwrap().detail.contains("claude-sonnet"));
    }

    #[test]
    fn ignores_model_alias_without_date() {
        let source = r#"
let model = "claude-sonnet-4-5";
"#;
        let result = detect("test.rs", source, &DetectConfig::default());
        let found = result
            .smells
            .iter()
            .find(|s| s.kind == SmellKind::HardcodedModelId);
        assert!(
            found.is_none(),
            "model alias without date should not be flagged"
        );
    }

    // --- URL detection ---

    #[test]
    fn detects_hardcoded_api_url() {
        let source = r#"
@base_url "https://api.openai.com/v1"
"#;
        let result = detect("test.ex", source, &DetectConfig::default());
        let found = result
            .smells
            .iter()
            .find(|s| s.kind == SmellKind::HardcodedUrl);
        assert!(found.is_some(), "should detect hardcoded API URL");
    }

    #[test]
    fn detects_hardcoded_wss_url() {
        let source = r#"
url = "wss://api.sprites.dev/ws"
"#;
        let result = detect("test.ex", source, &DetectConfig::default());
        let found = result
            .smells
            .iter()
            .find(|s| s.kind == SmellKind::HardcodedUrl);
        assert!(found.is_some(), "should detect hardcoded WebSocket URL");
    }

    #[test]
    fn detects_deployment_url() {
        let source = r#"
base_url = "https://coach.consultkoala.com/api"
"#;
        let result = detect("test.ex", source, &DetectConfig::default());
        let found = result
            .smells
            .iter()
            .find(|s| s.kind == SmellKind::HardcodedUrl);
        assert!(found.is_some(), "should detect deployment URL");
    }

    #[test]
    fn detects_cdn_url() {
        let source = r#"
const CDN = "https://cdn.myapp.io/assets"
"#;
        let result = detect("test.ts", source, &DetectConfig::default());
        let found = result
            .smells
            .iter()
            .find(|s| s.kind == SmellKind::HardcodedUrl);
        assert!(found.is_some(), "should detect CDN URL");
    }

    #[test]
    fn ignores_localhost_url() {
        let source = r#"
url = "http://localhost:4000/api"
url2 = "http://127.0.0.1:3000"
"#;
        let result = detect("test.rs", source, &DetectConfig::default());
        let found = result
            .smells
            .iter()
            .find(|s| s.kind == SmellKind::HardcodedUrl);
        assert!(found.is_none(), "localhost should not be flagged");
    }

    #[test]
    fn ignores_url_in_comments() {
        let source = r#"
// Base URL: "https://api.openai.com/v1"
# See "https://api.notion.com/v1" for docs
"#;
        let result = detect("test.rs", source, &DetectConfig::default());
        let found = result
            .smells
            .iter()
            .find(|s| s.kind == SmellKind::HardcodedUrl);
        assert!(found.is_none(), "comments should not be flagged");
    }

    #[test]
    fn ignores_github_url() {
        let source = r#"
repo = "https://github.com/user/project"
"#;
        let result = detect("test.rs", source, &DetectConfig::default());
        let found = result
            .smells
            .iter()
            .find(|s| s.kind == SmellKind::HardcodedUrl);
        assert!(found.is_none(), "github.com should not be flagged");
    }

    // --- Tunable detection ---

    #[test]
    fn detects_timeout_value() {
        let source = "let timeout = 30;\n";
        let result = detect("test.rs", source, &DetectConfig::default());
        let found = result
            .smells
            .iter()
            .find(|s| s.kind == SmellKind::HardcodedTunable);
        assert!(found.is_some(), "should detect hardcoded timeout");
        assert!(found.unwrap().detail.contains("timeout"));
    }

    #[test]
    fn detects_max_retries() {
        let source = "@max_retries 3\n";
        let result = detect("test.ex", source, &DetectConfig::default());
        let found = result
            .smells
            .iter()
            .find(|s| s.kind == SmellKind::HardcodedTunable);
        assert!(found.is_some(), "should detect max_retries");
    }

    #[test]
    fn detects_token_limit() {
        let source = "const max_tokens = 4096;\n";
        let result = detect("test.ts", source, &DetectConfig::default());
        let found = result
            .smells
            .iter()
            .find(|s| s.kind == SmellKind::HardcodedTunable);
        assert!(found.is_some(), "should detect token limit");
    }

    #[test]
    fn detects_temperature() {
        let source = "let temperature = 0.7;\n";
        let result = detect("test.rs", source, &DetectConfig::default());
        let found = result
            .smells
            .iter()
            .find(|s| s.kind == SmellKind::HardcodedTunable);
        assert!(found.is_some(), "should detect temperature");
    }

    #[test]
    fn detects_allcaps_constant() {
        let source = "BATCH_SIZE = 100\n";
        let result = detect("test.py", source, &DetectConfig::default());
        let found = result
            .smells
            .iter()
            .find(|s| s.kind == SmellKind::HardcodedTunable);
        assert!(found.is_some(), "should detect ALL_CAPS constant");
    }

    #[test]
    fn detects_port_number() {
        let source = "const port = 8080;\n";
        let result = detect("test.ts", source, &DetectConfig::default());
        let found = result
            .smells
            .iter()
            .find(|s| s.kind == SmellKind::HardcodedTunable);
        assert!(found.is_some(), "should detect hardcoded port");
    }

    #[test]
    fn ignores_zero_one_tunables() {
        let source = "let timeout = 0;\nlet retries = 1;\n";
        let result = detect("test.rs", source, &DetectConfig::default());
        let found = result
            .smells
            .iter()
            .find(|s| s.kind == SmellKind::HardcodedTunable);
        assert!(found.is_none(), "0 and 1 should not be flagged as tunables");
    }

    // --- Email detection ---

    #[test]
    fn detects_hardcoded_email() {
        let source = r#"
from = "admin@consultkoala.com"
"#;
        let result = detect("test.ex", source, &DetectConfig::default());
        let found = result
            .smells
            .iter()
            .find(|s| s.kind == SmellKind::HardcodedEmail);
        assert!(found.is_some(), "should detect hardcoded email");
    }

    #[test]
    fn ignores_example_email() {
        let source = r#"
email = "test@example.com"
"#;
        let result = detect("test.rs", source, &DetectConfig::default());
        let found = result
            .smells
            .iter()
            .find(|s| s.kind == SmellKind::HardcodedEmail);
        assert!(found.is_none(), "example.com emails should not be flagged");
    }

    // --- Secret detection ---

    #[test]
    fn detects_api_key_prefix() {
        let source = r#"
key = "sk-proj-abc123def456ghi789"
"#;
        let result = detect("test.py", source, &DetectConfig::default());
        let found = result
            .smells
            .iter()
            .find(|s| s.kind == SmellKind::HardcodedSecret);
        assert!(found.is_some(), "should detect sk- prefixed key");
    }

    #[test]
    fn detects_secret_variable_assignment() {
        let source = r#"
api_key = "my-super-long-secret-value-here"
"#;
        let result = detect("test.py", source, &DetectConfig::default());
        let found = result
            .smells
            .iter()
            .find(|s| s.kind == SmellKind::HardcodedSecret);
        assert!(found.is_some(), "should detect secret variable assignment");
    }

    #[test]
    fn ignores_secret_from_env() {
        let source = r#"
api_key = System.get_env("API_KEY")
api_key = os.environ["API_KEY"]
api_key = process.env.API_KEY
"#;
        let result = detect("test.py", source, &DetectConfig::default());
        let found = result
            .smells
            .iter()
            .find(|s| s.kind == SmellKind::HardcodedSecret);
        assert!(found.is_none(), "env-sourced secrets should not flag");
    }

    // --- Prompt detection ---

    #[test]
    fn detects_hardcoded_prompt() {
        let source = r#"
prompt = """
You are a helpful coding assistant.
You must follow these instructions carefully.
Always respond with valid JSON.
Do not include any extra text.
Your role is to analyze code.
"""
"#;
        let result = detect("test.py", source, &DetectConfig::default());
        let found = result
            .smells
            .iter()
            .find(|s| s.kind == SmellKind::HardcodedPrompt);
        assert!(found.is_some(), "should detect hardcoded prompt string");
        assert!(found.unwrap().detail.contains("prompt keywords"));
    }

    #[test]
    fn ignores_short_multiline_string() {
        let source = r#"
msg = """
Hello world
"""
"#;
        let result = detect("test.py", source, &DetectConfig::default());
        let found = result
            .smells
            .iter()
            .find(|s| s.kind == SmellKind::HardcodedPrompt);
        assert!(
            found.is_none(),
            "short strings without prompt keywords should not flag"
        );
    }
}
