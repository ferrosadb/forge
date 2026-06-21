//! Deterministic secret / credential scanner.
//!
//! Walks a directory (respecting `.gitignore`) and matches each text file
//! against a fixed set of high-confidence credential patterns: AWS keys,
//! GCP keys, GitHub PATs, Slack tokens, Stripe keys, private-key headers,
//! JWTs, and generic password assignments. Emits structured findings with
//! the secret body masked (first 4 + last 4 characters visible).
//!
//! Replaces hand-rolled `grep -E 'AKIA|BEGIN RSA'` patterns used by
//! `secure-review`, `pipeline-defense`, and `cloud-audit` skills.
//!
//! Entropy-based generic detection is a v2 feature: the [`Options`] flags
//! are accepted but not yet acted upon.

use anyhow::Result;
use ignore::WalkBuilder;
use regex::Regex;
use serde::Serialize;
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// Public API types
// ---------------------------------------------------------------------------

/// Options controlling the scan.
#[derive(Debug, Clone, Default)]
pub struct Options {
    /// Shannon entropy threshold for generic high-entropy strings (v2).
    pub min_entropy: Option<f64>,
    /// Enable generic high-entropy detection (v2).
    pub include_entropy: bool,
}

/// Severity level assigned to a detection rule.
#[derive(Debug, Serialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Critical,
    High,
    Medium,
}

impl Severity {
    fn as_str(&self) -> &'static str {
        match self {
            Severity::Critical => "critical",
            Severity::High => "high",
            Severity::Medium => "medium",
        }
    }
}

/// A single detection rule — an ID, label, severity, and compiled regex.
#[derive(Debug)]
pub struct DetectionRule {
    pub id: &'static str,
    pub label: &'static str,
    pub severity: Severity,
    pub regex: Regex,
}

/// A single secret finding.
#[derive(Debug, Serialize, PartialEq)]
pub struct Finding {
    pub id: String,
    pub severity: &'static str,
    pub file: String,
    pub line: usize,
    /// Full source line with the secret body masked.
    pub snippet: String,
    pub label: String,
}

/// Summary counts by severity bucket.
#[derive(Debug, Serialize, Default, PartialEq)]
pub struct Summary {
    pub critical: usize,
    pub high: usize,
    pub medium: usize,
}

/// Top-level scan report returned by [`scan`].
#[derive(Debug, Serialize)]
pub struct Report {
    pub path: PathBuf,
    pub files_scanned: usize,
    pub findings: Vec<Finding>,
    pub summary: Summary,
}

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// Scan a file or directory for credential patterns.
pub fn scan(path: &Path, _opts: &Options) -> Result<Report> {
    let rules = build_rules();
    let mut findings = Vec::new();
    let mut files_scanned = 0usize;

    let walker = WalkBuilder::new(path).standard_filters(true).build();

    for entry in walker.flatten() {
        let p = entry.path();
        if !p.is_file() {
            continue;
        }
        let Ok(raw) = std::fs::read(p) else {
            continue;
        };
        if is_binary(&raw) {
            continue;
        }
        let Ok(content) = std::str::from_utf8(&raw) else {
            continue;
        };
        files_scanned += 1;
        scan_content(p, content, &rules, &mut findings);
    }

    let mut summary = Summary::default();
    for f in &findings {
        match f.severity {
            "critical" => summary.critical += 1,
            "high" => summary.high += 1,
            "medium" => summary.medium += 1,
            _ => {}
        }
    }

    Ok(Report {
        path: path.to_path_buf(),
        files_scanned,
        findings,
        summary,
    })
}

// ---------------------------------------------------------------------------
// Private helpers
// ---------------------------------------------------------------------------

/// Treat a file as binary if its first 1024 bytes contain a NUL.
fn is_binary(buf: &[u8]) -> bool {
    let sample = &buf[..buf.len().min(1024)];
    sample.contains(&0u8)
}

/// Build the v1 detection rule set.
fn build_rules() -> Vec<DetectionRule> {
    let specs: &[(&'static str, &'static str, Severity, &'static str)] = &[
        (
            "AWS_ACCESS_KEY",
            "AWS access key ID",
            Severity::Critical,
            r"AKIA[0-9A-Z]{16}",
        ),
        (
            "AWS_SECRET",
            "AWS secret access key",
            Severity::Critical,
            r#"(?i)aws.{0,20}['"][0-9a-zA-Z/+]{40}['"]"#,
        ),
        (
            "GCP_KEY",
            "Google API key",
            Severity::Critical,
            r"AIza[0-9A-Za-z_\-]{35}",
        ),
        (
            "GCP_OAUTH",
            "GCP OAuth client",
            Severity::Critical,
            r"[0-9]+-[0-9A-Za-z_]{32}\.apps\.googleusercontent\.com",
        ),
        (
            "GITHUB_PAT",
            "GitHub personal access token",
            Severity::High,
            r"ghp_[0-9A-Za-z]{36}",
        ),
        (
            "GITHUB_OAUTH",
            "GitHub OAuth token",
            Severity::High,
            r"gho_[0-9A-Za-z]{36}",
        ),
        (
            "SLACK_TOKEN",
            "Slack token",
            Severity::High,
            r"xox[baprs]-[0-9A-Za-z\-]{10,48}",
        ),
        (
            "STRIPE_KEY",
            "Stripe secret key",
            Severity::High,
            r"sk_(live|test)_[0-9A-Za-z]{24,}",
        ),
        (
            "PRIVATE_KEY",
            "Private key header",
            Severity::Critical,
            r"-----BEGIN (RSA|DSA|EC|OPENSSH|PGP) PRIVATE KEY-----",
        ),
        (
            "JWT",
            "JWT (context-dependent)",
            Severity::High,
            r"eyJ[A-Za-z0-9_\-]+\.eyJ[A-Za-z0-9_\-]+\.[A-Za-z0-9_\-]+",
        ),
        (
            "GENERIC_PASSWORD",
            "Password assignment",
            Severity::Medium,
            r#"(?i)(password|passwd|secret)\s*[:=]\s*['"][^'"]{6,}['"]"#,
        ),
    ];

    specs
        .iter()
        .map(|(id, label, sev, pat)| DetectionRule {
            id,
            label,
            severity: *sev,
            regex: Regex::new(pat).expect("built-in secret regex is valid"),
        })
        .collect()
}

/// Scan a text buffer (not a file on disk) and return all findings.
///
/// `label` is used as the `file` field in each finding so callers can
/// point to an in-memory origin (e.g. the SKILL.md being ingested).
pub fn scan_text(label: &str, content: &str) -> Vec<Finding> {
    let rules = build_rules();
    let mut findings = Vec::new();
    let label_path = Path::new(label);
    scan_content(label_path, content, &rules, &mut findings);
    findings
}

fn scan_content(file: &Path, content: &str, rules: &[DetectionRule], findings: &mut Vec<Finding>) {
    for (idx, line) in content.lines().enumerate() {
        for rule in rules {
            if let Some(m) = rule.regex.find(line) {
                let masked_line = mask_match(line, m.start(), m.end());
                findings.push(Finding {
                    id: rule.id.to_string(),
                    severity: rule.severity.as_str(),
                    file: file.to_string_lossy().to_string(),
                    line: idx + 1,
                    snippet: masked_line,
                    label: rule.label.to_string(),
                });
            }
        }
    }
}

/// Replace the matched secret body within `line` with `*` characters,
/// keeping the first 4 and last 4 characters visible. If the secret is
/// <= 8 characters, mask the entire body.
fn mask_match(line: &str, start: usize, end: usize) -> String {
    let prefix = &line[..start];
    let secret = &line[start..end];
    let suffix = &line[end..];
    let masked = mask_secret(secret);
    format!("{prefix}{masked}{suffix}")
}

fn mask_secret(secret: &str) -> String {
    let chars: Vec<char> = secret.chars().collect();
    let n = chars.len();
    if n <= 8 {
        return "*".repeat(n);
    }
    let head: String = chars.iter().take(4).collect();
    let tail: String = chars.iter().skip(n - 4).collect();
    let middle = "*".repeat(n - 8);
    format!("{head}{middle}{tail}")
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

    fn scan_tmp(body: &str, filename: &str) -> Report {
        let tmp = TempDir::new().unwrap();
        write(tmp.path(), filename, body);
        scan(tmp.path(), &Options::default()).unwrap()
    }

    #[test]
    fn detects_aws_access_key() {
        let r = scan_tmp(concat!("AK", "IAIOSFODNN7EXAMPLE", "\n"), "a.txt");
        assert_eq!(r.findings.len(), 1);
        assert_eq!(r.findings[0].id, "AWS_ACCESS_KEY");
        assert_eq!(r.findings[0].severity, "critical");
    }

    #[test]
    fn detects_aws_secret() {
        let body = concat!(
            "aws_secret = \"",
            "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY",
            "\"\n"
        );
        let r = scan_tmp(body, "creds.cfg");
        assert!(r.findings.iter().any(|f| f.id == "AWS_SECRET"));
    }

    #[test]
    fn detects_gcp_key() {
        let body = concat!(
            "export GOOGLE_API_KEY=",
            "AI",
            "zaSyA-1234567890abcdefghijklmnopqrstuvw",
            "\n"
        );
        let r = scan_tmp(body, "env.sh");
        assert!(r.findings.iter().any(|f| f.id == "GCP_KEY"));
    }

    #[test]
    fn detects_gcp_oauth_client() {
        let body = concat!(
            "client_id: 123456789012-",
            "abcdefghijklmnopqrstuvwxyz012345",
            ".apps.googleusercontent.com\n"
        );
        let r = scan_tmp(body, "oauth.yaml");
        assert!(r.findings.iter().any(|f| f.id == "GCP_OAUTH"));
    }

    #[test]
    fn detects_github_pat() {
        let body = concat!(
            "token = ",
            "gh",
            "p_abcdefghijklmnopqrstuvwxyz0123456789",
            "\n"
        );
        let r = scan_tmp(body, "config.toml");
        assert!(r.findings.iter().any(|f| f.id == "GITHUB_PAT"));
        assert_eq!(
            r.findings
                .iter()
                .find(|f| f.id == "GITHUB_PAT")
                .unwrap()
                .severity,
            "high"
        );
    }

    #[test]
    fn detects_github_oauth_token() {
        let body = concat!("gh", "o_abcdefghijklmnopqrstuvwxyz0123456789", "\n");
        let r = scan_tmp(body, "token.env");
        assert!(r.findings.iter().any(|f| f.id == "GITHUB_OAUTH"));
    }

    #[test]
    fn detects_slack_token() {
        let body = concat!("SLACK=", "xo", "xb-1234567890-abcdefghijKLMNOPQRSTUV", "\n");
        let r = scan_tmp(body, "slack.env");
        assert!(r.findings.iter().any(|f| f.id == "SLACK_TOKEN"));
    }

    #[test]
    fn detects_stripe_key() {
        let body = concat!("STRIPE = ", "sk", "_live_abcdefghijklmnopqrstuvwx", "\n");
        let r = scan_tmp(body, "stripe.env");
        assert!(r.findings.iter().any(|f| f.id == "STRIPE_KEY"));
    }

    #[test]
    fn detects_private_key_header() {
        let body = concat!(
            "-----BEGIN RSA ",
            "PRIVATE KEY-----\n",
            "MIIE...\n",
            "-----END RSA ",
            "PRIVATE KEY-----\n"
        );
        let r = scan_tmp(body, "id_rsa");
        assert!(r.findings.iter().any(|f| f.id == "PRIVATE_KEY"));
        let pk = r.findings.iter().find(|f| f.id == "PRIVATE_KEY").unwrap();
        assert_eq!(pk.severity, "critical");
    }

    #[test]
    fn detects_jwt() {
        let body = concat!("auth: ", "ey", "JhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.SflKxwRJSMeKKF2QT4fwpMeJf36POk6yJV_adQssw5c", "\n");
        let r = scan_tmp(body, "a.txt");
        assert!(r.findings.iter().any(|f| f.id == "JWT"));
    }

    #[test]
    fn detects_generic_password_assignment() {
        let body = concat!("pass", "word = \"hunter2moosegoose\"\n");
        let r = scan_tmp(body, "a.txt");
        assert!(r.findings.iter().any(|f| f.id == "GENERIC_PASSWORD"));
        let p = r
            .findings
            .iter()
            .find(|f| f.id == "GENERIC_PASSWORD")
            .unwrap();
        assert_eq!(p.severity, "medium");
    }

    #[test]
    fn does_not_match_uuid_or_git_sha() {
        // Random UUID
        let uuid = "550e8400-e29b-41d4-a716-446655440000";
        // 40-hex git SHA
        let sha = "da39a3ee5e6b4b0d3255bfef95601890afd80709";
        let body = format!("uuid = {uuid}\nsha = {sha}\n");
        let r = scan_tmp(&body, "a.txt");
        assert!(
            r.findings.is_empty(),
            "unexpected findings: {:?}",
            r.findings
        );
    }

    #[test]
    fn does_not_match_base64_image() {
        // Minimal 1x1 PNG base64 body, the kind often embedded in HTML/CSS.
        let body = "<img src=\"data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==\" />\n";
        let r = scan_tmp(body, "index.html");
        assert!(
            r.findings.is_empty(),
            "unexpected findings: {:?}",
            r.findings
        );
    }

    #[test]
    fn masking_hides_full_secret_body() {
        let secret = concat!("AK", "IAIOSFODNN7EXAMPLE"); // 20 chars
        let body = format!("key = {secret}\n");
        let r = scan_tmp(&body, "a.txt");
        assert_eq!(r.findings.len(), 1);
        let snippet = &r.findings[0].snippet;
        assert!(
            !snippet.contains(secret),
            "masked snippet must not contain plaintext secret: {snippet}"
        );
        // First 4 and last 4 should still be visible per the spec.
        assert!(snippet.contains("AKIA"));
        assert!(snippet.contains("MPLE"));
        assert!(snippet.contains('*'));
    }

    #[test]
    fn mask_short_secret_fully_obscured() {
        assert_eq!(mask_secret("abcd1234"), "********");
        assert_eq!(mask_secret("short"), "*****");
    }

    #[test]
    fn mask_long_secret_keeps_ends() {
        assert_eq!(mask_secret("abcdXXXXXXwxyz"), "abcd******wxyz");
    }

    #[test]
    fn binary_file_skipped() {
        let tmp = TempDir::new().unwrap();
        // Embed a NUL byte to mark the file as "binary".
        let mut bytes = concat!("AK", "IAIOSFODNN7EXAMPLE").as_bytes().to_vec();
        bytes.insert(0, 0u8);
        fs::write(tmp.path().join("a.bin"), &bytes).unwrap();
        let r = scan(tmp.path(), &Options::default()).unwrap();
        assert_eq!(r.files_scanned, 0);
        assert!(r.findings.is_empty());
    }

    #[test]
    fn summary_counts_by_severity() {
        let tmp = TempDir::new().unwrap();
        write(
            tmp.path(),
            "a.txt",
            concat!(
                "AK",
                "IAIOSFODNN7EXAMPLE\n",
                "gh",
                "p_abcdefghijklmnopqrstuvwxyz0123456789\n",
                "pass",
                "word = \"hunter2moose\"\n"
            ),
        );
        let r = scan(tmp.path(), &Options::default()).unwrap();
        assert_eq!(r.summary.critical, 1);
        assert_eq!(r.summary.high, 1);
        assert_eq!(r.summary.medium, 1);
    }
}
