//! Dependency lockfile audit for known-vulnerable versions.
//!
//! Parses lockfiles across multiple ecosystems (Rust, Node, Python, Elixir,
//! Go) and flags packages whose versions match an embedded allowlist of
//! known-bad releases (supply-chain attacks, critical CVEs, yanked
//! packages). Offline-only in v1 — network-backed advisory lookups are
//! future work gated behind an `--online` flag.
//!
//! Philosophy: fail loud. Parse errors are surfaced; unknown lockfile
//! formats are skipped with the ecosystem count reflecting reality.

use anyhow::{anyhow, Context, Result};
use regex::Regex;
use serde::Serialize;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Severity levels ordered lowest to highest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Low,
    Medium,
    High,
    Critical,
}

impl Severity {
    pub fn parse(s: &str) -> Result<Self> {
        match s.to_ascii_lowercase().as_str() {
            "low" => Ok(Severity::Low),
            "medium" | "med" => Ok(Severity::Medium),
            "high" => Ok(Severity::High),
            "critical" | "crit" => Ok(Severity::Critical),
            other => Err(anyhow!("unknown severity: {other}")),
        }
    }
}

/// Configuration for `audit`.
#[derive(Debug, Clone)]
pub struct Options {
    /// If true, skip network lookups. v1 is always offline regardless.
    pub offline: bool,
    /// Exclude findings strictly below this severity.
    pub min_severity: Severity,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            offline: true,
            min_severity: Severity::Medium,
        }
    }
}

/// A discovered package that matches a vulnerability pattern.
#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct Finding {
    pub ecosystem: String,
    pub package: String,
    pub version: String,
    pub severity: Severity,
    pub source: &'static str,
    pub advisory: String,
    pub recommendation: String,
    pub file: String,
}

/// Severity-keyed counts plus a total package count.
#[derive(Debug, Serialize, Default, PartialEq, Eq)]
pub struct Summary {
    pub critical: usize,
    pub high: usize,
    pub medium: usize,
    pub low: usize,
    pub total_packages: usize,
}

/// Full audit report.
#[derive(Debug, Serialize)]
pub struct Report {
    pub ecosystems: Vec<String>,
    pub files_parsed: Vec<String>,
    pub package_count: usize,
    pub findings: Vec<Finding>,
    pub summary: Summary,
}

// ---------------------------------------------------------------------------
// Vulnerability database (embedded)
// ---------------------------------------------------------------------------

/// A static entry in the embedded vulnerability list.
#[derive(Debug, Clone, Copy)]
struct VulnerableVersion {
    ecosystem: &'static str,
    package: &'static str,
    /// Simple comparator: "< X.Y.Z" or "== X.Y.Z".
    comparator: &'static str,
    severity: Severity,
    advisory: &'static str,
    recommendation: &'static str,
}

const VULN_DB: &[VulnerableVersion] = &[
    // --- Node supply-chain attacks ---
    VulnerableVersion {
        ecosystem: "node",
        package: "event-stream",
        comparator: "== 3.3.6",
        severity: Severity::Critical,
        advisory: "event-stream 3.3.6 shipped a malicious dependency (flatmap-stream) in 2018.",
        recommendation: "remove event-stream; audit downstream usage.",
    },
    VulnerableVersion {
        ecosystem: "node",
        package: "node-ipc",
        comparator: "== 10.1.1",
        severity: Severity::Critical,
        advisory: "node-ipc 10.1.1 (2022) contained politically-motivated sabotage payload.",
        recommendation: "pin to a known-good version such as 9.2.1.",
    },
    VulnerableVersion {
        ecosystem: "node",
        package: "node-ipc",
        comparator: "== 10.1.2",
        severity: Severity::Critical,
        advisory: "node-ipc 10.1.2 (2022) contained politically-motivated sabotage payload.",
        recommendation: "pin to a known-good version such as 9.2.1.",
    },
    VulnerableVersion {
        ecosystem: "node",
        package: "colors",
        comparator: "== 1.4.44-liberty-2",
        severity: Severity::High,
        advisory: "colors 1.4.44-liberty-2 (2022) introduced an infinite loop in the library.",
        recommendation: "pin to 1.4.0 or earlier.",
    },
    VulnerableVersion {
        ecosystem: "node",
        package: "ua-parser-js",
        comparator: "== 0.7.29",
        severity: Severity::High,
        advisory: "ua-parser-js 0.7.29 was a malicious supply-chain release (2021).",
        recommendation: "upgrade to >= 0.7.30.",
    },
    VulnerableVersion {
        ecosystem: "node",
        package: "ua-parser-js",
        comparator: "== 0.8.0",
        severity: Severity::High,
        advisory: "ua-parser-js 0.8.0 was a malicious supply-chain release (2021).",
        recommendation: "upgrade to >= 0.8.1.",
    },
    VulnerableVersion {
        ecosystem: "node",
        package: "ua-parser-js",
        comparator: "== 1.0.0",
        severity: Severity::High,
        advisory: "ua-parser-js 1.0.0 was a malicious supply-chain release (2021).",
        recommendation: "upgrade to >= 1.0.1.",
    },
    VulnerableVersion {
        ecosystem: "node",
        package: "log4js",
        comparator: "< 6.4.0",
        severity: Severity::High,
        advisory: "log4js < 6.4.0 is affected by GHSA-82v2-mx6x-wq7q (prototype pollution).",
        recommendation: "upgrade to >= 6.4.0.",
    },
    // --- Java / log4j-core (tracked even though we don't parse Maven yet) ---
    VulnerableVersion {
        ecosystem: "java",
        package: "log4j-core",
        comparator: "< 2.17.1",
        severity: Severity::Critical,
        advisory: "CVE-2021-44228 (Log4Shell) and follow-ups; versions < 2.17.1 are unsafe.",
        recommendation: "upgrade to >= 2.17.1.",
    },
    // --- Rust ---
    VulnerableVersion {
        ecosystem: "rust",
        package: "openssl",
        comparator: "< 0.10.55",
        severity: Severity::High,
        advisory: "openssl < 0.10.55 had memory-safety issues (RUSTSEC-2023-0044).",
        recommendation: "upgrade to >= 0.10.55.",
    },
    VulnerableVersion {
        ecosystem: "rust",
        package: "time",
        comparator: "< 0.2.23",
        severity: Severity::Medium,
        advisory: "time < 0.2.23 segfaults on certain inputs (RUSTSEC-2020-0071).",
        recommendation: "upgrade to >= 0.2.23.",
    },
    // --- Python ---
    VulnerableVersion {
        ecosystem: "python",
        package: "pyyaml",
        comparator: "< 5.4",
        severity: Severity::High,
        advisory: "PyYAML < 5.4 is vulnerable to arbitrary code execution via yaml.load.",
        recommendation: "upgrade to >= 5.4 and use yaml.safe_load.",
    },
    VulnerableVersion {
        ecosystem: "python",
        package: "jinja2",
        comparator: "< 2.11.3",
        severity: Severity::Medium,
        advisory: "jinja2 < 2.11.3 regex DoS (CVE-2020-28493).",
        recommendation: "upgrade to >= 2.11.3.",
    },
    // --- Elixir ---
    VulnerableVersion {
        ecosystem: "elixir",
        package: "plug",
        comparator: "< 1.11.1",
        severity: Severity::Medium,
        advisory: "plug < 1.11.1 cookie handling issue.",
        recommendation: "upgrade to >= 1.11.1.",
    },
    // --- Go ---
    VulnerableVersion {
        ecosystem: "go",
        package: "golang.org/x/crypto",
        comparator: "< 0.17.0",
        severity: Severity::High,
        advisory: "golang.org/x/crypto < 0.17.0 SSH handshake panic (CVE-2023-48795 family).",
        recommendation: "upgrade to >= 0.17.0.",
    },
    // --- NuGet (.NET) ---
    VulnerableVersion {
        ecosystem: "nuget",
        package: "System.Text.RegularExpressions",
        comparator: "< 4.3.1",
        severity: Severity::High,
        advisory: "System.Text.RegularExpressions < 4.3.1 ReDoS vulnerability (CVE-2019-0820).",
        recommendation: "upgrade to >= 4.3.1.",
    },
    VulnerableVersion {
        ecosystem: "nuget",
        package: "System.Net.Http",
        comparator: "< 4.3.4",
        severity: Severity::High,
        advisory: "System.Net.Http < 4.3.4 improper certificate validation (CVE-2018-8292).",
        recommendation: "upgrade to >= 4.3.4.",
    },
    VulnerableVersion {
        ecosystem: "nuget",
        package: "Newtonsoft.Json",
        comparator: "< 13.0.1",
        severity: Severity::High,
        advisory: "Newtonsoft.Json < 13.0.1 insecure defaults vulnerability (CVE-2024-21907).",
        recommendation: "upgrade to >= 13.0.1.",
    },
    VulnerableVersion {
        ecosystem: "nuget",
        package: "Microsoft.Data.SqlClient",
        comparator: "< 5.1.4",
        severity: Severity::High,
        advisory:
            "Microsoft.Data.SqlClient < 5.1.4 SQL injection via connection string (CVE-2024-0056).",
        recommendation: "upgrade to >= 5.1.4.",
    },
];

// ---------------------------------------------------------------------------
// Semver comparator (tiny, enough for the embedded format)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct SemverTriple(u32, u32, u32);

fn parse_semver(raw: &str) -> Option<SemverTriple> {
    // Strip leading 'v'.
    let mut s = raw.trim();
    if let Some(rest) = s.strip_prefix('v') {
        s = rest;
    }
    // Split on '-' or '+' to drop pre-release/build metadata.
    let core = s.split_once(['-', '+']).map(|(core, _)| core).unwrap_or(s);
    let mut parts = core.split('.');
    let major = parts.next()?.parse::<u32>().ok()?;
    let minor = parts.next().unwrap_or("0").parse::<u32>().ok()?;
    let patch_raw = parts.next().unwrap_or("0");
    // Handle patch with non-numeric trailing chars.
    let patch_num: String = patch_raw
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    let patch = if patch_num.is_empty() {
        0
    } else {
        patch_num.parse::<u32>().ok()?
    };
    Some(SemverTriple(major, minor, patch))
}

/// Returns true if `version` satisfies the simple comparator string.
/// Supported forms: "< X.Y.Z" and "== X.Y.Z".
fn matches_comparator(version: &str, comparator: &str) -> bool {
    let comp = comparator.trim();
    let (op, rhs_raw) = if let Some(rhs) = comp.strip_prefix("<=") {
        ("<=", rhs.trim())
    } else if let Some(rhs) = comp.strip_prefix(">=") {
        (">=", rhs.trim())
    } else if let Some(rhs) = comp.strip_prefix("==") {
        ("==", rhs.trim())
    } else if let Some(rhs) = comp.strip_prefix('<') {
        ("<", rhs.trim())
    } else if let Some(rhs) = comp.strip_prefix('>') {
        (">", rhs.trim())
    } else {
        return false;
    };

    let Some(lhs) = parse_semver(version) else {
        return false;
    };
    let Some(rhs) = parse_semver(rhs_raw) else {
        return false;
    };

    match op {
        "<" => lhs < rhs,
        "<=" => lhs <= rhs,
        ">" => lhs > rhs,
        ">=" => lhs >= rhs,
        "==" => lhs == rhs,
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// Parsed-package intermediate type
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
struct ParsedPackage {
    ecosystem: &'static str,
    name: String,
    version: String,
    source_file: String,
}

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// Run a dependency audit rooted at `path`.
///
/// `path` may be a single lockfile or a directory. When it is a directory,
/// every supported lockfile at the top level is parsed (we do not recurse
/// into subdirectories in v1 — lockfiles live at project roots).
pub fn audit(path: &Path, opts: &Options) -> Result<Report> {
    let mut files_parsed = Vec::new();
    let mut ecosystems: BTreeSet<String> = BTreeSet::new();
    let mut packages: Vec<ParsedPackage> = Vec::new();

    let candidates = collect_lockfiles(path).context("failed to collect lockfiles")?;

    for lockfile in candidates {
        let name = lockfile
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
            .to_string();
        let content = std::fs::read_to_string(&lockfile)
            .with_context(|| format!("reading {}", lockfile.display()))?;
        let parsed = match name.as_str() {
            "Cargo.lock" => parse_cargo_lock(&content, &name)?,
            "package-lock.json" => parse_package_lock_json(&content, &name)?,
            "packages.lock.json" => parse_nuget_lock_json(&content, &name)?,
            "mix.lock" => parse_mix_lock(&content, &name),
            "go.sum" => parse_go_sum(&content, &name),
            "requirements.txt" => parse_requirements_txt(&content, &name),
            _ => continue,
        };
        if !parsed.is_empty() {
            ecosystems.insert(parsed[0].ecosystem.to_string());
        }
        files_parsed.push(name);
        packages.extend(parsed);
    }

    // Fail loud if caller pointed at a single file we don't understand.
    if packages.is_empty() && path.is_file() {
        return Err(anyhow!(
            "no recognized lockfile at {} (supported: Cargo.lock, package-lock.json, packages.lock.json, mix.lock, go.sum, requirements.txt)",
            path.display()
        ));
    }

    let findings = apply_vuln_db(&packages, opts.min_severity);
    let mut summary = Summary {
        total_packages: packages.len(),
        ..Default::default()
    };
    for f in &findings {
        match f.severity {
            Severity::Critical => summary.critical += 1,
            Severity::High => summary.high += 1,
            Severity::Medium => summary.medium += 1,
            Severity::Low => summary.low += 1,
        }
    }

    // Signal offline v1 mode — the flag exists for forward compatibility.
    let _ = opts.offline;

    Ok(Report {
        ecosystems: ecosystems.into_iter().collect(),
        files_parsed,
        package_count: packages.len(),
        findings,
        summary,
    })
}

fn collect_lockfiles(path: &Path) -> Result<Vec<PathBuf>> {
    if path.is_file() {
        return Ok(vec![path.to_path_buf()]);
    }
    if !path.is_dir() {
        return Err(anyhow!("path does not exist: {}", path.display()));
    }
    let mut out = Vec::new();
    for candidate in [
        "Cargo.lock",
        "package-lock.json",
        "packages.lock.json",
        "mix.lock",
        "go.sum",
        "requirements.txt",
    ] {
        let joined = path.join(candidate);
        if joined.is_file() {
            out.push(joined);
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Vuln DB application
// ---------------------------------------------------------------------------

fn apply_vuln_db(packages: &[ParsedPackage], min_severity: Severity) -> Vec<Finding> {
    let mut out = Vec::new();
    for pkg in packages {
        for entry in VULN_DB.iter() {
            if entry.ecosystem != pkg.ecosystem {
                continue;
            }
            if entry.package != pkg.name {
                continue;
            }
            if !matches_comparator(&pkg.version, entry.comparator) {
                continue;
            }
            if entry.severity < min_severity {
                continue;
            }
            out.push(Finding {
                ecosystem: pkg.ecosystem.to_string(),
                package: pkg.name.clone(),
                version: pkg.version.clone(),
                severity: entry.severity,
                source: "embedded",
                advisory: entry.advisory.to_string(),
                recommendation: entry.recommendation.to_string(),
                file: pkg.source_file.clone(),
            });
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Parsers
// ---------------------------------------------------------------------------

fn parse_cargo_lock(content: &str, source_file: &str) -> Result<Vec<ParsedPackage>> {
    #[derive(serde::Deserialize)]
    struct CargoLock {
        #[serde(default)]
        package: Vec<CargoPkg>,
    }
    #[derive(serde::Deserialize)]
    struct CargoPkg {
        name: String,
        version: String,
    }

    let lock: CargoLock =
        toml::from_str(content).with_context(|| format!("parsing {source_file} as TOML"))?;
    Ok(lock
        .package
        .into_iter()
        .map(|p| ParsedPackage {
            ecosystem: "rust",
            name: p.name,
            version: p.version,
            source_file: source_file.to_string(),
        })
        .collect())
}

fn parse_package_lock_json(content: &str, source_file: &str) -> Result<Vec<ParsedPackage>> {
    let value: serde_json::Value =
        serde_json::from_str(content).with_context(|| format!("parsing {source_file} as JSON"))?;
    let mut out = Vec::new();

    // lockfileVersion 2/3: `packages` map keyed by path. Root entry key is "".
    if let Some(packages) = value.get("packages").and_then(|v| v.as_object()) {
        for (key, entry) in packages {
            if key.is_empty() {
                continue; // root package
            }
            // Key looks like "node_modules/foo" or "node_modules/foo/node_modules/bar".
            let name = key
                .rsplit("node_modules/")
                .next()
                .unwrap_or(key)
                .to_string();
            let version = entry
                .get("version")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if name.is_empty() || version.is_empty() {
                continue;
            }
            out.push(ParsedPackage {
                ecosystem: "node",
                name,
                version,
                source_file: source_file.to_string(),
            });
        }
        return Ok(out);
    }

    // lockfileVersion 1: `dependencies` map keyed by package name.
    if let Some(deps) = value.get("dependencies").and_then(|v| v.as_object()) {
        walk_v1_deps(deps, source_file, &mut out);
    }
    Ok(out)
}

fn walk_v1_deps(
    deps: &serde_json::Map<String, serde_json::Value>,
    source_file: &str,
    out: &mut Vec<ParsedPackage>,
) {
    for (name, entry) in deps {
        let version = entry
            .get("version")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if !version.is_empty() {
            out.push(ParsedPackage {
                ecosystem: "node",
                name: name.clone(),
                version,
                source_file: source_file.to_string(),
            });
        }
        if let Some(nested) = entry.get("dependencies").and_then(|v| v.as_object()) {
            walk_v1_deps(nested, source_file, out);
        }
    }
}

fn parse_mix_lock(content: &str, source_file: &str) -> Vec<ParsedPackage> {
    // mix.lock entries look like:
    //   "plug": {:hex, :plug, "1.11.1", "hash", ...},
    let re = Regex::new(r#""([a-zA-Z0-9_\-]+)"\s*:\s*\{:hex,\s*:[a-zA-Z0-9_\-]+,\s*"([^"]+)""#)
        .expect("mix.lock regex is valid");
    let mut out = Vec::new();
    for cap in re.captures_iter(content) {
        out.push(ParsedPackage {
            ecosystem: "elixir",
            name: cap[1].to_string(),
            version: cap[2].to_string(),
            source_file: source_file.to_string(),
        });
    }
    out
}

fn parse_go_sum(content: &str, source_file: &str) -> Vec<ParsedPackage> {
    let mut out = Vec::new();
    let mut seen: BTreeSet<(String, String)> = BTreeSet::new();
    for line in content.lines() {
        // go.sum lines: `<module> v<version>[/go.mod] h1:<hash>`
        let mut parts = line.split_whitespace();
        let Some(module) = parts.next() else {
            continue;
        };
        let Some(version_raw) = parts.next() else {
            continue;
        };
        // Strip trailing "/go.mod" marker lines — they duplicate the module entry.
        let version = version_raw.trim_end_matches("/go.mod").to_string();
        let key = (module.to_string(), version.clone());
        if seen.insert(key) {
            out.push(ParsedPackage {
                ecosystem: "go",
                name: module.to_string(),
                version,
                source_file: source_file.to_string(),
            });
        }
    }
    out
}

fn parse_requirements_txt(content: &str, source_file: &str) -> Vec<ParsedPackage> {
    let re = Regex::new(r"^([a-zA-Z0-9_\-\.]+)==([0-9A-Za-z\.\-_+]+)")
        .expect("requirements.txt regex is valid");
    let mut out = Vec::new();
    for raw in content.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(cap) = re.captures(line) {
            out.push(ParsedPackage {
                ecosystem: "python",
                name: cap[1].to_string(),
                version: cap[2].to_string(),
                source_file: source_file.to_string(),
            });
        }
    }
    out
}

fn parse_nuget_lock_json(content: &str, source_file: &str) -> Result<Vec<ParsedPackage>> {
    let value: serde_json::Value =
        serde_json::from_str(content).with_context(|| format!("parsing {source_file} as JSON"))?;
    let mut out = Vec::new();

    // NuGet packages.lock.json structure:
    // { "version": 1, "dependencies": { "<tfm>": { "<PackageName>": { "resolved": "1.0.0", ... } } } }
    let deps = match value.get("dependencies").and_then(|v| v.as_object()) {
        Some(d) => d,
        None => return Ok(out),
    };

    for (_tfm, framework_deps) in deps {
        let framework_map = match framework_deps.as_object() {
            Some(m) => m,
            None => continue,
        };
        for (package_name, entry) in framework_map {
            let version = entry
                .get("resolved")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if version.is_empty() {
                continue;
            }
            out.push(ParsedPackage {
                ecosystem: "nuget",
                name: package_name.clone(),
                version,
                source_file: source_file.to_string(),
            });
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn tmp() -> tempfile_shim::TempDir {
        tempfile_shim::tempdir()
    }

    // We avoid bringing in the `tempfile` crate as a dev-dep to keep the
    // dependency footprint tight; a tiny shim writes into a unique temp dir
    // under std::env::temp_dir().
    mod tempfile_shim {
        use std::path::{Path, PathBuf};
        use std::sync::atomic::{AtomicU64, Ordering};

        static COUNTER: AtomicU64 = AtomicU64::new(0);

        pub struct TempDir {
            path: PathBuf,
        }

        impl TempDir {
            pub fn path(&self) -> &Path {
                &self.path
            }
        }

        impl Drop for TempDir {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.path);
            }
        }

        pub fn tempdir() -> TempDir {
            let id = COUNTER.fetch_add(1, Ordering::SeqCst);
            let pid = std::process::id();
            let path = std::env::temp_dir().join(format!("forge-deps-audit-{pid}-{id}"));
            std::fs::create_dir_all(&path).expect("create tempdir");
            TempDir { path }
        }
    }

    #[test]
    fn severity_ordering_and_parse() {
        assert!(Severity::Critical > Severity::High);
        assert!(Severity::Medium > Severity::Low);
        assert_eq!(Severity::parse("critical").unwrap(), Severity::Critical);
        assert_eq!(Severity::parse("HIGH").unwrap(), Severity::High);
        assert!(Severity::parse("bogus").is_err());
    }

    #[test]
    fn semver_parser_accepts_common_forms() {
        assert_eq!(parse_semver("1.2.3"), Some(SemverTriple(1, 2, 3)));
        assert_eq!(parse_semver("v1.2.3"), Some(SemverTriple(1, 2, 3)));
        assert_eq!(parse_semver("0.10.55"), Some(SemverTriple(0, 10, 55)));
        assert_eq!(parse_semver("1.2.3-beta"), Some(SemverTriple(1, 2, 3)));
        assert_eq!(parse_semver("2.17"), Some(SemverTriple(2, 17, 0)));
    }

    #[test]
    fn comparator_matches() {
        assert!(matches_comparator("2.17.0", "< 2.17.1"));
        assert!(!matches_comparator("2.17.1", "< 2.17.1"));
        assert!(matches_comparator("10.1.1", "== 10.1.1"));
        assert!(!matches_comparator("10.1.3", "== 10.1.1"));
        assert!(matches_comparator("0.10.54", "< 0.10.55"));
    }

    #[test]
    fn cargo_lock_flags_vulnerable_openssl() {
        let dir = tmp();
        let lockfile = dir.path().join("Cargo.lock");
        fs::write(
            &lockfile,
            r#"
[[package]]
name = "serde"
version = "1.0.200"

[[package]]
name = "openssl"
version = "0.10.40"
"#,
        )
        .unwrap();

        let report = audit(dir.path(), &Options::default()).unwrap();
        assert_eq!(report.package_count, 2);
        assert_eq!(report.ecosystems, vec!["rust".to_string()]);
        assert_eq!(report.findings.len(), 1);
        let finding = &report.findings[0];
        assert_eq!(finding.package, "openssl");
        assert_eq!(finding.severity, Severity::High);
    }

    #[test]
    fn package_lock_json_flags_log4js() {
        let dir = tmp();
        let lockfile = dir.path().join("package-lock.json");
        let json = r#"{
  "name": "demo",
  "lockfileVersion": 2,
  "packages": {
    "": { "name": "demo", "version": "0.0.1" },
    "node_modules/log4js": { "version": "6.3.0" },
    "node_modules/left-pad": { "version": "1.3.0" }
  }
}"#;
        fs::write(&lockfile, json).unwrap();
        let report = audit(dir.path(), &Options::default()).unwrap();
        assert_eq!(report.package_count, 2);
        assert_eq!(report.findings.len(), 1);
        assert_eq!(report.findings[0].package, "log4js");
        assert_eq!(report.findings[0].severity, Severity::High);
    }

    #[test]
    fn mix_lock_parses_without_findings() {
        let dir = tmp();
        let lockfile = dir.path().join("mix.lock");
        // plug 1.11.2 is above the < 1.11.1 threshold in the embedded DB.
        let content = r#"%{
  "plug": {:hex, :plug, "1.11.2", "deadbeef", [:mix], [{:mime, "~> 1.0", [hex: :mime, optional: false]}], "hexpm", "cafef00d"},
}
"#;
        fs::write(&lockfile, content).unwrap();
        let report = audit(dir.path(), &Options::default()).unwrap();
        assert_eq!(report.package_count, 1);
        assert_eq!(report.findings.len(), 0);
        assert_eq!(report.ecosystems, vec!["elixir".to_string()]);
    }

    #[test]
    fn requirements_txt_parses_safe_jinja() {
        let dir = tmp();
        let lockfile = dir.path().join("requirements.txt");
        // jinja2 2.11.3 is exactly at the `< 2.11.3` boundary — must NOT match.
        fs::write(&lockfile, "# comment\njinja2==2.11.3\n").unwrap();
        let report = audit(dir.path(), &Options::default()).unwrap();
        assert_eq!(report.package_count, 1);
        assert_eq!(report.findings.len(), 0);
    }

    #[test]
    fn go_sum_dedupes_gomod_lines() {
        let dir = tmp();
        let lockfile = dir.path().join("go.sum");
        let content = "\
golang.org/x/crypto v0.16.0 h1:aaaa\n\
golang.org/x/crypto v0.16.0/go.mod h1:bbbb\n\
github.com/stretchr/testify v1.8.4 h1:cccc\n";
        fs::write(&lockfile, content).unwrap();
        let report = audit(dir.path(), &Options::default()).unwrap();
        assert_eq!(report.package_count, 2);
        assert_eq!(report.findings.len(), 1);
        assert_eq!(report.findings[0].package, "golang.org/x/crypto");
    }

    #[test]
    fn min_severity_filters_out_medium() {
        let dir = tmp();
        let lockfile = dir.path().join("Cargo.lock");
        fs::write(
            &lockfile,
            r#"
[[package]]
name = "time"
version = "0.2.22"
"#,
        )
        .unwrap();
        let opts = Options {
            offline: true,
            min_severity: Severity::High,
        };
        let report = audit(dir.path(), &opts).unwrap();
        assert_eq!(report.package_count, 1);
        assert_eq!(report.findings.len(), 0); // medium filtered out
    }

    #[test]
    fn nuget_lock_flags_vulnerable_newtonsoft() {
        let dir = tmp();
        let lockfile = dir.path().join("packages.lock.json");
        let json = r#"{
  "version": 1,
  "dependencies": {
    "net8.0": {
      "Newtonsoft.Json": {
        "type": "Direct",
        "requested": "[13.0.0, )",
        "resolved": "13.0.0",
        "contentHash": "abc123"
      },
      "Serilog": {
        "type": "Direct",
        "requested": "[3.0.0, )",
        "resolved": "3.0.0",
        "contentHash": "def456"
      }
    }
  }
}"#;
        fs::write(&lockfile, json).unwrap();
        let report = audit(dir.path(), &Options::default()).unwrap();
        assert_eq!(report.package_count, 2);
        assert_eq!(report.ecosystems, vec!["nuget".to_string()]);
        assert_eq!(report.findings.len(), 1);
        assert_eq!(report.findings[0].package, "Newtonsoft.Json");
        assert_eq!(report.findings[0].severity, Severity::High);
    }

    #[test]
    fn nuget_lock_safe_versions_no_findings() {
        let dir = tmp();
        let lockfile = dir.path().join("packages.lock.json");
        let json = r#"{
  "version": 1,
  "dependencies": {
    "net8.0": {
      "Newtonsoft.Json": {
        "type": "Direct",
        "requested": "[13.0.1, )",
        "resolved": "13.0.1",
        "contentHash": "abc123"
      }
    }
  }
}"#;
        fs::write(&lockfile, json).unwrap();
        let report = audit(dir.path(), &Options::default()).unwrap();
        assert_eq!(report.package_count, 1);
        assert_eq!(report.findings.len(), 0);
    }

    #[test]
    fn nuget_lock_multi_framework() {
        let dir = tmp();
        let lockfile = dir.path().join("packages.lock.json");
        let json = r#"{
  "version": 1,
  "dependencies": {
    "net8.0": {
      "System.Net.Http": {
        "type": "Transitive",
        "resolved": "4.3.2"
      }
    },
    "net6.0": {
      "System.Net.Http": {
        "type": "Transitive",
        "resolved": "4.3.2"
      }
    }
  }
}"#;
        fs::write(&lockfile, json).unwrap();
        let report = audit(dir.path(), &Options::default()).unwrap();
        // Both framework targets contribute packages (even if duplicated)
        assert_eq!(report.package_count, 2);
        // Both instances should be flagged as vulnerable
        assert_eq!(report.findings.len(), 2);
        assert!(report
            .findings
            .iter()
            .all(|f| f.package == "System.Net.Http"));
    }
}
