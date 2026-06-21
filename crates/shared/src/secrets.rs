//! Filename-level denylist for files that typically contain secrets.
//!
//! This list is **authoritative** for any forge tool that enumerates files
//! (e.g. `frg glob`) and must never be overridable from the CLI. It exists
//! to prevent accidental exposure of common credential files to agents.
//!
//! The list targets filenames and extensions, not file contents. Content-
//! level secret detection lives in the `secret-scan` crate.
//!
//! # Invariants
//!
//! 1. Every pattern here must match a filename or extension that is
//!    **overwhelmingly likely** to contain secrets. False positives are
//!    acceptable; false negatives are not.
//! 2. Patterns are glob-style (`*`, `?`) and matched against the base
//!    filename (not the full path) unless the pattern itself contains `/`.
//! 3. This const is part of the public API — external callers may rely on
//!    its contents. Additions are always safe; removals are breaking.

/// Filename patterns that indicate likely-secret files.
///
/// Applied as a hard denylist after user-supplied excludes; no CLI flag
/// may remove entries. See FMEA F7 in `feat-glob-stats` for rationale.
pub const SECRET_FILENAMES: &[&str] = &[
    // Environment files
    ".env",
    ".env.*",
    "*.env",
    // Private keys & certificates
    "*.pem",
    "*.key",
    "*.p12",
    "*.pfx",
    "id_rsa",
    "id_rsa.*",
    "id_dsa",
    "id_dsa.*",
    "id_ecdsa",
    "id_ecdsa.*",
    "id_ed25519",
    "id_ed25519.*",
    // Cloud provider credentials
    "credentials",
    "credentials.json",
    "credentials.yml",
    "credentials.yaml",
    "service-account*.json",
    "gcloud-key*.json",
    // Kubernetes & docker
    "kubeconfig",
    ".kube/config",
    ".dockercfg",
    ".docker/config.json",
    // Generic
    "secrets.yml",
    "secrets.yaml",
    "secrets.json",
    ".netrc",
    ".pgpass",
    ".my.cnf",
];

/// Returns true if the given filename matches any SECRET_FILENAMES pattern.
///
/// The match is glob-style: `*` matches any sequence of non-slash chars,
/// `?` matches any single char. Case-sensitive (matches typical unix
/// filesystems; users on case-insensitive filesystems get the same
/// coverage because the patterns target lowercase conventional names).
pub fn is_secret_filename(name: &str) -> bool {
    SECRET_FILENAMES.iter().any(|pat| glob_match(pat, name))
}

/// Minimal glob matcher for filename patterns (no `/`, no `**`, no `[...]`).
/// Suitable for the fixed SECRET_FILENAMES list; not a general glob engine.
fn glob_match(pattern: &str, text: &str) -> bool {
    let pat = pattern.as_bytes();
    let txt = text.as_bytes();
    glob_match_impl(pat, 0, txt, 0)
}

fn glob_match_impl(pat: &[u8], mut pi: usize, txt: &[u8], mut ti: usize) -> bool {
    while pi < pat.len() {
        match pat[pi] {
            b'*' => {
                // Skip consecutive stars
                while pi + 1 < pat.len() && pat[pi + 1] == b'*' {
                    pi += 1;
                }
                // If * is the last pattern char, match the rest
                if pi + 1 == pat.len() {
                    return true;
                }
                // Try to match the rest at every position in txt
                for skip in ti..=txt.len() {
                    if glob_match_impl(pat, pi + 1, txt, skip) {
                        return true;
                    }
                }
                return false;
            }
            b'?' => {
                if ti >= txt.len() {
                    return false;
                }
                pi += 1;
                ti += 1;
            }
            c => {
                if ti >= txt.len() || txt[ti] != c {
                    return false;
                }
                pi += 1;
                ti += 1;
            }
        }
    }
    ti == txt.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_dotenv() {
        assert!(is_secret_filename(".env"));
        assert!(is_secret_filename(".env.local"));
        assert!(is_secret_filename(".env.production"));
        assert!(is_secret_filename("app.env"));
    }

    #[test]
    fn matches_private_keys() {
        assert!(is_secret_filename("id_rsa"));
        assert!(is_secret_filename("id_rsa.pub"));
        assert!(is_secret_filename("server.pem"));
        assert!(is_secret_filename("private.key"));
    }

    #[test]
    fn matches_credentials() {
        assert!(is_secret_filename("credentials"));
        assert!(is_secret_filename("credentials.json"));
        assert!(is_secret_filename("service-account.json"));
        assert!(is_secret_filename("service-account-prod.json"));
    }

    #[test]
    fn does_not_match_benign() {
        assert!(!is_secret_filename("README.md"));
        assert!(!is_secret_filename("main.rs"));
        assert!(!is_secret_filename("Cargo.toml"));
        assert!(!is_secret_filename("env_var_list.txt"));
        assert!(!is_secret_filename("keyboard.rs"));
    }

    #[test]
    fn pattern_anchoring() {
        // .env.* must not match just ".env"
        // but ".env" matches the exact ".env" entry
        // Regression: ensure "env" alone does NOT match
        assert!(!is_secret_filename("env"));
        assert!(!is_secret_filename("environment"));
    }

    #[test]
    fn denylist_is_nonempty() {
        // Regression guard against accidental deletion
        assert!(!SECRET_FILENAMES.is_empty());
        assert!(SECRET_FILENAMES.contains(&".env"));
    }
}
