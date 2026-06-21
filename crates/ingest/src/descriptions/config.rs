//! Config for the description-extraction pass.

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    Local,
    Openai,
    Anthropic,
    Skip,
}

impl Provider {
    pub fn parse(s: &str) -> Result<Self> {
        match s.to_ascii_lowercase().as_str() {
            "local" => Ok(Self::Local),
            "openai" => Ok(Self::Openai),
            "anthropic" => Ok(Self::Anthropic),
            "skip" => Ok(Self::Skip),
            other => bail!("unknown provider '{other}' (expected: local, openai, anthropic, skip)"),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DescriptionsConfig {
    pub enabled: bool,
    pub provider: Provider,
    pub local_model: String,
    pub local_endpoint: String,
    pub local_timeout_ms: u64,
    pub remote_model: String,
    pub max_words: u32,
    pub include_private: bool,
    pub min_confidence: f32,
    pub concurrency: usize,
    pub max_desc_calls: u64,
    /// When true, suppress TTY prompts on probe failures — return error.
    pub non_interactive: bool,
}

impl Default for DescriptionsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            provider: Provider::Local,
            local_model: "qwen2.5-coder:7b".to_string(),
            local_endpoint: "http://localhost:11434".to_string(),
            local_timeout_ms: 5000,
            remote_model: "claude-haiku-4-5".to_string(),
            max_words: 60,
            include_private: false,
            min_confidence: 0.7,
            concurrency: 4,
            max_desc_calls: 5000,
            non_interactive: false,
        }
    }
}

impl DescriptionsConfig {
    /// Validate constraints that should never reach runtime.
    pub fn validate(&self) -> Result<()> {
        if self.max_words == 0 {
            bail!("max_words must be > 0");
        }
        if self.max_words > 500 {
            bail!("max_words > 500 is unreasonable; cap is by design");
        }
        if self.concurrency == 0 {
            bail!("concurrency must be >= 1");
        }
        if self.concurrency > 32 {
            bail!("concurrency > 32 would saturate local endpoints; cap is by design");
        }
        if !(0.0..=1.0).contains(&self.min_confidence) {
            bail!("min_confidence must be in [0.0, 1.0]");
        }
        if self.local_timeout_ms == 0 {
            bail!("local_timeout_ms must be > 0");
        }
        validate_local_endpoint(&self.local_endpoint)?;
        Ok(())
    }
}

/// Restrict `local_endpoint` to loopback URLs unless explicitly waived.
/// FMEA F11 / threat T7: prevents a rogue `local` endpoint from
/// exfiltrating snippets under the guise of local processing.
pub fn validate_local_endpoint(endpoint: &str) -> Result<()> {
    let lower = endpoint.to_ascii_lowercase();
    if !(lower.starts_with("http://") || lower.starts_with("https://")) {
        bail!("local_endpoint must use http:// or https:// (got '{endpoint}')");
    }
    let rest = lower
        .trim_start_matches("http://")
        .trim_start_matches("https://");
    let host = rest.split('/').next().unwrap_or("");
    // Handle bracketed IPv6 form: `[::1]:11434` → strip brackets, split on
    // the bracket end. The leading colons inside brackets would otherwise
    // break a naive split(':').
    let host_only = if let Some(stripped) = host.strip_prefix('[') {
        // Up to closing bracket.
        stripped.split(']').next().unwrap_or("")
    } else {
        host.split(':').next().unwrap_or("")
    };
    let is_loopback = matches!(host_only, "localhost" | "127.0.0.1" | "::1");
    if !is_loopback {
        bail!(
            "local_endpoint '{endpoint}' is not loopback (expected localhost, 127.0.0.1, or ::1); \
             pass --desc-allow-remote-local to override"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_provider() {
        assert_eq!(Provider::parse("local").unwrap(), Provider::Local);
        assert_eq!(Provider::parse("OPENAI").unwrap(), Provider::Openai);
        assert!(Provider::parse("garbage").is_err());
    }

    #[test]
    fn validate_loopback_only() {
        assert!(validate_local_endpoint("http://localhost:11434").is_ok());
        assert!(validate_local_endpoint("http://127.0.0.1:11434").is_ok());
        assert!(validate_local_endpoint("http://[::1]:11434").is_ok());
        assert!(validate_local_endpoint("http://10.0.0.5:11434").is_err());
        assert!(validate_local_endpoint("http://evil.example.com").is_err());
    }

    #[test]
    fn validate_scheme() {
        assert!(validate_local_endpoint("ssh://localhost").is_err());
        assert!(validate_local_endpoint("file://localhost").is_err());
        assert!(validate_local_endpoint("localhost:11434").is_err());
    }

    #[test]
    fn validate_rejects_bad_config() {
        let mut cfg = DescriptionsConfig::default();
        cfg.max_words = 0;
        assert!(cfg.validate().is_err());

        let mut cfg = DescriptionsConfig::default();
        cfg.concurrency = 0;
        assert!(cfg.validate().is_err());

        let mut cfg = DescriptionsConfig::default();
        cfg.min_confidence = 1.5;
        assert!(cfg.validate().is_err());
    }
}
