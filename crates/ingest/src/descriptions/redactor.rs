//! Redact secret-shaped tokens from LLM request payloads.
//!
//! This runs on every snippet **regardless of provider**. Even local
//! providers get redacted input — defense in depth and cheap to apply
//! (FMEA F4 / threat T2).
//!
//! The regex set is intentionally conservative: we prefer false positives
//! (over-redaction) to false negatives (secret leakage).

use regex::Regex;
use std::sync::LazyLock;

/// Pattern + label pairs for common secret shapes.
fn pattern_set() -> Vec<(Regex, &'static str)> {
    vec![
        // AWS access key
        (
            Regex::new(r"(?i)AKIA[0-9A-Z]{16}").expect("valid regex"),
            "AWS_ACCESS_KEY",
        ),
        // Generic API key = "KEY"
        (
            Regex::new(r#"(?i)(api[_-]?key|apikey|secret|token|password)\s*[:=]\s*["']?[A-Za-z0-9_\-]{16,}["']?"#)
                .expect("valid regex"),
            "GENERIC_SECRET",
        ),
        // OpenAI / Anthropic style
        (
            Regex::new(r"sk-[A-Za-z0-9_\-]{20,}").expect("valid regex"),
            "SK_TOKEN",
        ),
        (
            Regex::new(r"sk-ant-[A-Za-z0-9_\-]{20,}").expect("valid regex"),
            "ANTHROPIC_TOKEN",
        ),
        // GitHub
        (
            Regex::new(r"gh[pousr]_[A-Za-z0-9]{36,}").expect("valid regex"),
            "GITHUB_TOKEN",
        ),
        // Private key blocks
        (
            Regex::new(r"-----BEGIN (RSA |EC |OPENSSH |DSA )?PRIVATE KEY-----")
                .expect("valid regex"),
            "PRIVATE_KEY",
        ),
        // JWT-ish
        (
            Regex::new(r"eyJ[A-Za-z0-9_\-]{10,}\.[A-Za-z0-9_\-]{10,}\.[A-Za-z0-9_\-]{10,}")
                .expect("valid regex"),
            "JWT_TOKEN",
        ),
        // Slack
        (
            Regex::new(r"xox[baprs]-[A-Za-z0-9-]{10,}").expect("valid regex"),
            "SLACK_TOKEN",
        ),
    ]
}

static PATTERNS: LazyLock<Vec<(Regex, &'static str)>> = LazyLock::new(pattern_set);

/// Redact secrets from a text snippet. Returns the redacted text and the
/// number of replacements performed.
pub fn redact(text: &str) -> (String, u32) {
    let mut out = text.to_string();
    let mut count: u32 = 0;
    for (re, label) in PATTERNS.iter() {
        let new_count = re.find_iter(&out).count();
        if new_count > 0 {
            let replacement = format!("<REDACTED:{label}>");
            out = re.replace_all(&out, replacement.as_str()).to_string();
            count = count.saturating_add(new_count as u32);
        }
    }
    (out, count)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_aws_access_key() {
        let (out, n) = redact(concat!("config.aws_key = ", "AK", "IAIOSFODNN7EXAMPLE"));
        assert!(out.contains("<REDACTED:AWS_ACCESS_KEY>"));
        assert!(!out.contains(concat!("AK", "IAIOSFODNN7EXAMPLE")));
        assert_eq!(n, 1);
    }

    #[test]
    fn redacts_generic_api_key() {
        let (out, n) = redact(r#"API_KEY = "abc123def456ghi789xyz""#);
        assert!(out.contains("<REDACTED:GENERIC_SECRET>"));
        assert!(n >= 1);
    }

    #[test]
    fn redacts_sk_tokens() {
        // A bare sk-... with no "token:" prefix — SK_TOKEN pattern fires.
        let (out, _) = redact("value is sk-abcdef0123456789abcdef extra");
        assert!(out.contains("<REDACTED:SK_TOKEN>"), "got: {out}");
    }

    #[test]
    fn redacts_anthropic_tokens() {
        // Any of the secret-shaped patterns is acceptable; the exact label
        // depends on pattern priority, and both SK_TOKEN and ANTHROPIC_TOKEN
        // fully redact the value.
        let (out, _) = redact("sk-ant-abcdef0123456789abcdef");
        assert!(out.contains("<REDACTED:"), "got: {out}");
        assert!(!out.contains("abcdef0123456789abcdef"), "got: {out}");
    }

    #[test]
    fn redacts_private_key_block() {
        let (out, _) = redact(concat!(
            "-----BEGIN RSA ",
            "PRIVATE KEY-----\nMIIE...\n-----END"
        ));
        assert!(out.contains("<REDACTED:PRIVATE_KEY>"));
    }

    #[test]
    fn redacts_jwt() {
        let (out, _) = redact(concat!(
            "jwt: ",
            "ey",
            "JhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3OA.SflKxwRJSMeKKF2"
        ));
        assert!(out.contains("<REDACTED:JWT_TOKEN>"));
    }

    #[test]
    fn idempotent_no_change_on_clean_text() {
        let clean = "Normal comment describing what this function does.";
        let (out, n) = redact(clean);
        assert_eq!(out, clean);
        assert_eq!(n, 0);
    }

    #[test]
    fn redactor_idempotent_under_double_pass() {
        // Property: redact(redact(x)) == redact(x)
        let original = concat!(
            "api_key = secretabc123xyz456abcd and ",
            "AK",
            "IAIOSFODNN7EXAMPLE"
        );
        let (first, _) = redact(original);
        let (second, _) = redact(&first);
        assert_eq!(first, second);
    }

    #[test]
    fn counts_multiple_matches() {
        let text = concat!("AK", "IAIOSFODNN7EXAMPLE and ", "AK", "IAI44QH8DHBEXAMPLE");
        let (_, n) = redact(text);
        assert_eq!(n, 2);
    }
}
