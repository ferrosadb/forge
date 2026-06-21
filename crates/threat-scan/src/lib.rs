//! STRIDE-oriented pattern scanner.
//!
//! Mirrors `forge-concurrency-scan`: a static rule catalog of regex
//! patterns tagged with STRIDE category, severity, confidence, and the
//! languages each rule applies to. Walks a tree with `ignore::WalkBuilder`
//! (respecting .gitignore), runs every applicable rule line-by-line, and
//! emits a report grouped by category and severity.
//!
//! The scanner is intentionally coarse: it gives `threat-model` and
//! `secure-review` skills a first-pass target list, not a verdict.
//! High-confidence rules are calibrated to zero-FP on a small fixture
//! corpus; medium-confidence rules accept false positives in exchange
//! for recall and are meant for human review.

use anyhow::{anyhow, Result};
use ignore::WalkBuilder;
use regex::Regex;
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// STRIDE threat categories.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    Spoofing,
    Tampering,
    Repudiation,
    InfoDisclosure,
    Dos,
    Elevation,
}

impl Category {
    pub fn parse(s: &str) -> Result<Self> {
        match s.to_ascii_lowercase().as_str() {
            "spoofing" => Ok(Category::Spoofing),
            "tampering" => Ok(Category::Tampering),
            "repudiation" => Ok(Category::Repudiation),
            "info_disclosure" | "information_disclosure" | "info" => Ok(Category::InfoDisclosure),
            "dos" | "denial_of_service" => Ok(Category::Dos),
            "elevation" | "elevation_of_privilege" | "eop" => Ok(Category::Elevation),
            other => Err(anyhow!("unknown STRIDE category: {other}")),
        }
    }

    fn tag(&self) -> &'static str {
        match self {
            Category::Spoofing => "spoofing",
            Category::Tampering => "tampering",
            Category::Repudiation => "repudiation",
            Category::InfoDisclosure => "info_disclosure",
            Category::Dos => "dos",
            Category::Elevation => "elevation",
        }
    }
}

/// Severity levels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Low,
    Medium,
    High,
    Critical,
}

/// Confidence levels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Confidence {
    Low,
    Medium,
    High,
}

impl Confidence {
    pub fn parse(s: &str) -> Result<Self> {
        match s.to_ascii_lowercase().as_str() {
            "low" => Ok(Confidence::Low),
            "medium" | "med" => Ok(Confidence::Medium),
            "high" => Ok(Confidence::High),
            other => Err(anyhow!("unknown confidence: {other}")),
        }
    }
}

/// Configuration for `scan`.
#[derive(Debug, Clone)]
pub struct Options {
    /// Categories to scan for. Empty means "all".
    pub categories: Vec<Category>,
    /// Drop findings below this confidence level.
    pub min_confidence: Confidence,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            categories: Vec::new(),
            min_confidence: Confidence::Medium,
        }
    }
}

/// A single match.
#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct Finding {
    pub id: &'static str,
    pub category: Category,
    pub severity: Severity,
    pub confidence: Confidence,
    pub file: String,
    pub line: usize,
    pub snippet: String,
    pub recommendation: &'static str,
}

/// Aggregate report across all scanned files.
#[derive(Debug, Serialize)]
pub struct Report {
    pub files_scanned: usize,
    pub findings: Vec<Finding>,
    pub summary_by_category: BTreeMap<String, usize>,
    pub summary_by_severity: BTreeMap<String, usize>,
}

// ---------------------------------------------------------------------------
// Rule definition
// ---------------------------------------------------------------------------

/// A single pattern-based detection rule.
pub struct Rule {
    pub id: &'static str,
    pub category: Category,
    pub severity: Severity,
    pub confidence: Confidence,
    pub pattern: Regex,
    /// File extensions this rule applies to. Empty slice means "all".
    pub languages: &'static [&'static str],
    pub recommendation: &'static str,
}

/// Build the v1 rule catalog. Called once per `scan` invocation; cheap
/// enough that we do not bother with `lazy_static`.
pub fn rules() -> Vec<Rule> {
    let all_langs: &[&str] = &[];
    let py_langs: &[&str] = &["py"];
    let py_js_langs: &[&str] = &["py", "js", "jsx", "ts", "tsx", "mjs", "cjs"];
    let rust_langs: &[&str] = &["rs"];
    let rust_py_go_langs: &[&str] = &["rs", "py", "go"];
    let shell_docker_langs: &[&str] = &["sh", "bash", "zsh", "Dockerfile", "dockerfile"];

    vec![
        // --- Spoofing ---
        Rule {
            id: "SPOOF-002",
            category: Category::Spoofing,
            severity: Severity::High,
            confidence: Confidence::High,
            pattern: Regex::new(r"jwt\.decode\s*\([^)]*\)").unwrap(),
            languages: py_js_langs,
            recommendation: "Use jwt.verify / jwt.decode(..., verify=True, key=...) so the signature is checked.",
        },
        Rule {
            id: "SPOOF-003",
            category: Category::Spoofing,
            severity: Severity::High,
            confidence: Confidence::Medium,
            // Matches: `password == ...`, `password.eq(`, `password.equals(`
            pattern: Regex::new(
                r"(?i)(password|passwd|secret|token|hmac|api_key)\s*(==|\.eq\b|\.equals\s*\()",
            )
            .unwrap(),
            languages: all_langs,
            recommendation: "Use a constant-time comparison (e.g. hmac.compare_digest, subtle::ConstantTimeEq).",
        },
        // --- Tampering ---
        Rule {
            id: "TAMPER-001",
            category: Category::Tampering,
            severity: Severity::High,
            confidence: Confidence::High,
            // SQL keyword in a string literal followed by '+' concatenation
            // with an identifier — strong signal of unparameterized SQL.
            pattern: Regex::new(
                r#"(?i)"\s*(SELECT|INSERT|UPDATE|DELETE)\b[^"]*"\s*\+\s*[A-Za-z_][A-Za-z0-9_\.]*"#,
            )
            .unwrap(),
            languages: all_langs,
            recommendation: "Use a parameterized query / prepared statement instead of string concatenation.",
        },
        Rule {
            id: "TAMPER-002",
            category: Category::Tampering,
            severity: Severity::Critical,
            confidence: Confidence::High,
            pattern: Regex::new(r"\beval\s*\(").unwrap(),
            languages: py_js_langs,
            recommendation: "Avoid eval; parse structured input (JSON, ast.literal_eval) instead.",
        },
        Rule {
            id: "TAMPER-003",
            category: Category::Tampering,
            severity: Severity::High,
            confidence: Confidence::High,
            pattern: Regex::new(r"(shell\s*=\s*True|os\.system\s*\()").unwrap(),
            languages: py_langs,
            recommendation: "Use subprocess.run([...], shell=False) with an argv list.",
        },
        Rule {
            id: "TAMPER-004",
            category: Category::Tampering,
            severity: Severity::Medium,
            confidence: Confidence::Medium,
            // Mass assignment: something like User(**request.json) or User.objects.create(**data)
            pattern: Regex::new(r"\b[A-Z][A-Za-z0-9_]*\s*\(\s*\*\*\s*(request\.|params|data)")
                .unwrap(),
            languages: py_langs,
            recommendation: "Use an allowlist schema (pydantic / serializer) instead of splatting user input.",
        },
        // --- Repudiation ---
        Rule {
            id: "REPUD-001",
            category: Category::Repudiation,
            severity: Severity::Medium,
            confidence: Confidence::Medium,
            // HTTP mutation route decorators — actual "no audit within 20 lines"
            // check is applied below in `scan_lines`.
            pattern: Regex::new(
                r#"@(app|router|bp|blueprint)\.(post|put|delete|patch)\s*\(|\brouter\.(post|put|delete|patch)\s*\(|Router::new\(\)\.(post|put|delete|patch)\s*\("#,
            )
            .unwrap(),
            languages: all_langs,
            recommendation: "Add an audit log call in mutation handlers (who / what / when).",
        },
        // --- Information disclosure ---
        Rule {
            id: "INFO-001",
            category: Category::InfoDisclosure,
            severity: Severity::Medium,
            confidence: Confidence::Medium,
            pattern: Regex::new(
                r#"(traceback\.format_exc\s*\(|err\.stack|error\.stack|panic::catch_unwind)"#,
            )
            .unwrap(),
            languages: all_langs,
            recommendation: "Log stack traces server-side; return opaque error IDs to clients.",
        },
        Rule {
            id: "INFO-003",
            category: Category::InfoDisclosure,
            severity: Severity::High,
            confidence: Confidence::High,
            // println!/console.log/print of a `password|token|secret|api_key` identifier.
            pattern: Regex::new(
                r#"(?i)(println!|console\.log|print|printf|log\.(info|debug|warn|error))\s*\([^)]*\b(password|passwd|secret|token|api[_-]?key|private[_-]?key)\b"#,
            )
            .unwrap(),
            languages: all_langs,
            recommendation: "Never log credentials; redact before logging.",
        },
        Rule {
            id: "INFO-004",
            category: Category::InfoDisclosure,
            severity: Severity::High,
            confidence: Confidence::High,
            pattern: Regex::new(
                r#"Access-Control-Allow-Origin["']?\s*[,:]\s*["']\*["']"#,
            )
            .unwrap(),
            languages: all_langs,
            recommendation: "Do not combine wildcard CORS with credentialed requests; allowlist origins.",
        },
        // --- Denial of service ---
        Rule {
            id: "DOS-001",
            category: Category::Dos,
            severity: Severity::Medium,
            confidence: Confidence::Low,
            pattern: Regex::new(r"while\s+True\s*:\s*.*\binput\s*\(").unwrap(),
            languages: py_langs,
            recommendation: "Cap iteration count and validate input length.",
        },
        Rule {
            id: "DOS-002",
            category: Category::Dos,
            severity: Severity::High,
            confidence: Confidence::High,
            // Classic catastrophic-backtracking shape: nested quantifiers.
            pattern: Regex::new(r"\([^)]*[+*]\)[+*]").unwrap(),
            languages: all_langs,
            recommendation: "Rewrite to avoid nested quantifiers; prefer possessive/atomic groups.",
        },
        Rule {
            id: "DOS-003",
            category: Category::Dos,
            severity: Severity::Medium,
            confidence: Confidence::Medium,
            pattern: Regex::new(r"axum::Server::bind|TcpListener::bind\s*\(").unwrap(),
            languages: rust_langs,
            recommendation: "Set request timeouts via tower::timeout or an equivalent middleware.",
        },
        Rule {
            id: "DOS-004",
            category: Category::Dos,
            severity: Severity::Medium,
            confidence: Confidence::Medium,
            pattern: Regex::new(r"app\.listen\s*\(|uvicorn\.run\s*\(").unwrap(),
            languages: py_js_langs,
            recommendation: "Front the app with a rate-limiting middleware (express-rate-limit, slowapi).",
        },
        // --- Elevation of privilege ---
        Rule {
            id: "ELEV-001",
            category: Category::Elevation,
            severity: Severity::Medium,
            confidence: Confidence::Medium,
            // role == 'admin' / role == "admin" / role.eq("admin")
            pattern: Regex::new(
                r#"(?i)\brole\s*(==|\.eq\b|\.equals\s*\()\s*['"]admin['"]"#,
            )
            .unwrap(),
            languages: all_langs,
            recommendation: "Model roles as an enum and dispatch on the enum variant.",
        },
        Rule {
            id: "ELEV-002",
            category: Category::Elevation,
            severity: Severity::High,
            confidence: Confidence::High,
            pattern: Regex::new(r"(chmod\s+777|setuid\b|RUN\s+sudo\b)").unwrap(),
            languages: shell_docker_langs,
            recommendation: "Drop privileges; avoid world-writable permissions and setuid binaries.",
        },
        Rule {
            id: "ELEV-003",
            category: Category::Elevation,
            severity: Severity::Critical,
            confidence: Confidence::High,
            // We match the broad shape here; the `yaml.load` false-positive
            // for `yaml.safe_load` is handled by a contextual filter below.
            pattern: Regex::new(r"(pickle\.loads\s*\(|yaml\.load\s*\()").unwrap(),
            languages: py_langs,
            recommendation: "Use yaml.safe_load; never unpickle untrusted input.",
        },
        // --- C# / .NET rules ---
        Rule {
            id: "TAMPER-001-CS",
            category: Category::Tampering,
            severity: Severity::High,
            confidence: Confidence::High,
            pattern: Regex::new(r#"(?i)FromSqlRaw\s*\(\s*\$"|\.ExecuteSqlRaw\s*\(\s*\$""#).unwrap(),
            languages: &["cs"],
            recommendation: "Use FromSqlInterpolated/ExecuteSqlInterpolated with parameterized queries. String interpolation in FromSqlRaw is SQL injection.",
        },
        Rule {
            id: "TAMPER-002-CS",
            category: Category::Tampering,
            severity: Severity::High,
            confidence: Confidence::Medium,
            pattern: Regex::new(r#"(?i)new\s+SqlCommand\s*\(\s*(\$"|[^"]*\+)"#).unwrap(),
            languages: &["cs"],
            recommendation: "Use parameterized SqlCommand with Parameters.AddWithValue. String concatenation is SQL injection.",
        },
        Rule {
            id: "ELEV-002-CS",
            category: Category::Elevation,
            severity: Severity::High,
            confidence: Confidence::Medium,
            pattern: Regex::new(r"\[AllowAnonymous\]").unwrap(),
            languages: &["cs"],
            recommendation: "Verify [AllowAnonymous] is intentional on this endpoint. Prefer [Authorize] by default.",
        },
        Rule {
            id: "INFO-004-CS",
            category: Category::InfoDisclosure,
            severity: Severity::Medium,
            confidence: Confidence::High,
            pattern: Regex::new(r"(?i)\.UseDeveloperExceptionPage\(\)").unwrap(),
            languages: &["cs"],
            recommendation: "UseDeveloperExceptionPage exposes stack traces and source. Only use in Development environment.",
        },
        Rule {
            id: "TAMPER-003-CS",
            category: Category::Tampering,
            severity: Severity::Critical,
            confidence: Confidence::High,
            pattern: Regex::new(r"(?i)AllowPartiallyTrustedCallers|unsafe\s*\{").unwrap(),
            languages: &["cs"],
            recommendation: "Unsafe code and APTCA bypass CAS protections. Audit carefully and justify.",
        },
        Rule {
            id: "SPOOF-004-CS",
            category: Category::Spoofing,
            severity: Severity::High,
            confidence: Confidence::Medium,
            pattern: Regex::new(r"(?i)ServerCertificateCustomValidationCallback\s*=.*(?:true|=>)").unwrap(),
            languages: &["cs"],
            recommendation: "Disabling SSL certificate validation allows MITM attacks. Never do this in production.",
        },
        Rule {
            id: "INFO-005-CS",
            category: Category::InfoDisclosure,
            severity: Severity::Medium,
            confidence: Confidence::High,
            pattern: Regex::new(r"(?i)EnableSensitiveDataLogging\s*\(\s*true\s*\)").unwrap(),
            languages: &["cs"],
            recommendation: "EnableSensitiveDataLogging exposes SQL parameter values in logs. Restrict to Development.",
        },
        // Rust-specific tag keeps warnings quiet if the constants go unused.
        Rule {
            id: "TAMPER-001-RS",
            category: Category::Tampering,
            severity: Severity::High,
            confidence: Confidence::Medium,
            pattern: Regex::new(r#"format!\s*\(\s*"[^"]*(SELECT|INSERT|UPDATE|DELETE)"#).unwrap(),
            languages: rust_py_go_langs,
            recommendation: "Use parameterized queries (sqlx bind, diesel) rather than format! on SQL.",
        },
    ]
}

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// Scan a set of roots for threat patterns.
pub fn scan(paths: &[PathBuf], opts: &Options) -> Result<Report> {
    if paths.is_empty() {
        return Err(anyhow!("no scan roots provided"));
    }
    let all_rules = rules();
    let active_rules: Vec<&Rule> = all_rules
        .iter()
        .filter(|r| opts.categories.is_empty() || opts.categories.contains(&r.category))
        .filter(|r| r.confidence >= opts.min_confidence)
        .collect();

    let mut findings: Vec<Finding> = Vec::new();
    let mut files_scanned = 0usize;

    for root in paths {
        if !root.exists() {
            return Err(anyhow!("scan root does not exist: {}", root.display()));
        }
        let walker = WalkBuilder::new(root)
            .hidden(false)
            .git_ignore(true)
            .git_global(false)
            .build();
        for entry in walker.flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            files_scanned += 1;
            let content = match std::fs::read_to_string(path) {
                Ok(s) => s,
                Err(_) => continue, // binary/unreadable — skip, consistent with other scanners
            };
            let file_display = path.display().to_string();
            scan_content(&content, &file_display, &active_rules, &mut findings);
        }
    }

    let mut summary_by_category: BTreeMap<String, usize> = BTreeMap::new();
    let mut summary_by_severity: BTreeMap<String, usize> = BTreeMap::new();
    for f in &findings {
        *summary_by_category
            .entry(f.category.tag().to_string())
            .or_insert(0) += 1;
        let sev_key = match f.severity {
            Severity::Critical => "critical",
            Severity::High => "high",
            Severity::Medium => "medium",
            Severity::Low => "low",
        };
        *summary_by_severity.entry(sev_key.to_string()).or_insert(0) += 1;
    }

    Ok(Report {
        files_scanned,
        findings,
        summary_by_category,
        summary_by_severity,
    })
}

/// Scan a single file's content. Exposed for unit tests.
pub fn scan_content(
    content: &str,
    file_display: &str,
    rules: &[&Rule],
    findings: &mut Vec<Finding>,
) {
    let ext = extension_for(file_display);
    let lines: Vec<&str> = content.lines().collect();

    for rule in rules {
        if !rule_applies(rule, ext) {
            continue;
        }
        for (idx, line) in lines.iter().enumerate() {
            if !rule.pattern.is_match(line) {
                continue;
            }
            if !passes_contextual_filter(rule, &lines, idx) {
                continue;
            }
            findings.push(Finding {
                id: rule.id,
                category: rule.category,
                severity: rule.severity,
                confidence: rule.confidence,
                file: file_display.to_string(),
                line: idx + 1,
                snippet: line.trim_end().to_string(),
                recommendation: rule.recommendation,
            });
        }
    }
}

fn extension_for(path: &str) -> &str {
    // Handle bare filename match first (e.g. "Dockerfile").
    let filename = Path::new(path)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(path);
    if filename.eq_ignore_ascii_case("Dockerfile") {
        return "Dockerfile";
    }
    path.rsplit('.').next().unwrap_or("")
}

fn rule_applies(rule: &Rule, ext: &str) -> bool {
    if rule.languages.is_empty() {
        return true;
    }
    rule.languages.iter().any(|l| l.eq_ignore_ascii_case(ext))
}

/// Apply rule-specific contextual filters that can't be expressed as a
/// single regex. REPUD-001 requires "no audit call within 20 lines".
fn passes_contextual_filter(rule: &Rule, lines: &[&str], idx: usize) -> bool {
    match rule.id {
        "REPUD-001" => {
            let end = (idx + 20).min(lines.len());
            let window = &lines[idx..end];
            let audit_re = Regex::new(r"(?i)\b(audit|log|logger|tracing|slog)\b").unwrap();
            !window.iter().any(|l| audit_re.is_match(l))
        }
        "ELEV-003" => {
            // `yaml.safe_load(` starts with "yaml.safe_load" — our pattern
            // matches the `yaml.load(` substring in `yaml.safe_load(` only
            // when we match against `yaml.load`, which we don't because
            // `yaml.load\s*\(` requires `.load(` not `.safe_load(`. But
            // belt-and-suspenders: if the full line contains `safe_load`
            // and doesn't contain `pickle.loads`, treat it as safe.
            let line = lines[idx];
            if line.contains("safe_load") && !line.contains("pickle.loads") {
                return false;
            }
            true
        }
        _ => true,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn scan_str(filename: &str, source: &str, opts: &Options) -> Vec<Finding> {
        let all = rules();
        let active: Vec<&Rule> = all
            .iter()
            .filter(|r| opts.categories.is_empty() || opts.categories.contains(&r.category))
            .filter(|r| r.confidence >= opts.min_confidence)
            .collect();
        let mut findings = Vec::new();
        scan_content(source, filename, &active, &mut findings);
        findings
    }

    #[test]
    fn spoofing_jwt_decode_positive() {
        let src = r#"let payload = jwt.decode(token);"#;
        let f = scan_str("auth.js", src, &Options::default());
        assert!(f.iter().any(|x| x.id == "SPOOF-002"));
    }

    #[test]
    fn tampering_sql_concat_positive() {
        let src = r#"query = "SELECT * FROM users WHERE id = " + user_id"#;
        let f = scan_str("db.py", src, &Options::default());
        assert!(f.iter().any(|x| x.id == "TAMPER-001"));
    }

    #[test]
    fn tampering_eval_positive() {
        let src = "x = eval(user_code)\n";
        let f = scan_str("run.py", src, &Options::default());
        assert!(f.iter().any(|x| x.id == "TAMPER-002"));
    }

    #[test]
    fn info_disclosure_log_password_positive() {
        let src = concat!(r#"console.log("#, "pass", r#"word=" + password);"#);
        let f = scan_str("login.js", src, &Options::default());
        assert!(f.iter().any(|x| x.id == "INFO-003"));
    }

    #[test]
    fn dos_catastrophic_regex_positive() {
        // Source-code line that constructs a regex with nested quantifiers.
        let src = r#"let re = Regex::new(r"(a+)+");"#;
        let f = scan_str("pattern.rs", src, &Options::default());
        assert!(f.iter().any(|x| x.id == "DOS-002"));
    }

    #[test]
    fn elevation_pickle_loads_positive() {
        let src = "import pickle\nuser = pickle.loads(blob)\n";
        let f = scan_str("boot.py", src, &Options::default());
        assert!(f.iter().any(|x| x.id == "ELEV-003"));
    }

    #[test]
    fn elevation_chmod_777_positive() {
        let src = "RUN chmod 777 /app\n";
        let f = scan_str("Dockerfile", src, &Options::default());
        assert!(f.iter().any(|x| x.id == "ELEV-002"));
    }

    #[test]
    fn repudiation_mutation_without_audit_positive() {
        let src = r#"
@app.post("/users")
def create_user(body):
    db.insert(body)
    return {"ok": True}
"#;
        let f = scan_str("routes.py", src, &Options::default());
        assert!(f.iter().any(|x| x.id == "REPUD-001"));
    }

    // -------- Negative / false-positive resistance ---------------------

    #[test]
    fn elevation_role_id_comparison_negative() {
        // `role_id == admin_id` must NOT trigger ELEV-001.
        let src = "if role_id == admin_id { grant(); }";
        let f = scan_str("rbac.rs", src, &Options::default());
        assert!(
            !f.iter().any(|x| x.id == "ELEV-001"),
            "unexpected ELEV-001 match on role_id vs admin_id: {:?}",
            f
        );
    }

    #[test]
    fn tampering_sql_string_without_concat_negative() {
        // Plain SQL literal without string concatenation must not match TAMPER-001.
        let src = r#"let q = "SELECT * FROM users WHERE id = ?";"#;
        let f = scan_str("db.rs", src, &Options::default());
        assert!(!f.iter().any(|x| x.id == "TAMPER-001"));
    }

    #[test]
    fn info_disclosure_log_plain_message_negative() {
        let src = r#"println!("login succeeded");"#;
        let f = scan_str("main.rs", src, &Options::default());
        assert!(!f.iter().any(|x| x.id == "INFO-003"));
    }

    #[test]
    fn repudiation_mutation_with_audit_negative() {
        let src = r#"
@app.post("/users")
def create_user(body):
    logger.info("create_user called by %s", current_user)
    db.insert(body)
    return {"ok": True}
"#;
        let f = scan_str("routes.py", src, &Options::default());
        assert!(
            !f.iter().any(|x| x.id == "REPUD-001"),
            "audit call should suppress REPUD-001: {:?}",
            f
        );
    }

    // -------- Filter / config tests ------------------------------------

    #[test]
    fn category_filter_restricts_results() {
        let src = r#"
let x = eval(code);
let q = "SELECT * FROM t WHERE id = " + id;
"#;
        let opts = Options {
            categories: vec![Category::Tampering],
            min_confidence: Confidence::Medium,
        };
        let f = scan_str("mix.js", src, &opts);
        // Both matches are tampering — still fine — but no spoofing/elevation rules fire.
        assert!(f.iter().all(|x| x.category == Category::Tampering));
        assert!(f.iter().any(|x| x.id == "TAMPER-002"));
    }

    #[test]
    fn min_confidence_filters_low_confidence_rules() {
        // DOS-001 is Confidence::Low; default min_confidence=Medium excludes it.
        let src = "while True: x = input()\n";
        let default = scan_str("loop.py", src, &Options::default());
        assert!(!default.iter().any(|x| x.id == "DOS-001"));

        let low_opts = Options {
            categories: Vec::new(),
            min_confidence: Confidence::Low,
        };
        let with_low = scan_str("loop.py", src, &low_opts);
        assert!(with_low.iter().any(|x| x.id == "DOS-001"));
    }

    #[test]
    fn scan_aggregates_summary_counts() {
        // Write a tempdir with two files and invoke the top-level `scan`.
        let pid = std::process::id();
        let dir = std::env::temp_dir().join(format!("forge-threat-scan-test-{pid}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("bad.py"), "x = eval(user)\n").unwrap();
        std::fs::write(
            dir.join("db.py"),
            r#"q = "SELECT * FROM users WHERE id = " + user_id
"#,
        )
        .unwrap();

        let report = scan(std::slice::from_ref(&dir), &Options::default()).unwrap();
        assert!(report.files_scanned >= 2);
        assert!(report.findings.len() >= 2);
        assert!(
            report
                .summary_by_category
                .get("tampering")
                .copied()
                .unwrap_or(0)
                >= 2
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unknown_category_parse_errors() {
        assert!(Category::parse("bogus").is_err());
        assert_eq!(Category::parse("Tampering").unwrap(), Category::Tampering);
    }
}
