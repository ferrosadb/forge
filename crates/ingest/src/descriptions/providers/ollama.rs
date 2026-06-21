//! Ollama (OpenAI-compatible `/api/generate` + `/api/tags`) provider.
//!
//! Uses `ureq` for blocking HTTP, consistent with the rest of the ingest
//! pipeline. An `Agent` is built once per provider instance and reused
//! across threads (ureq agents are `Send + Sync`).

use crate::descriptions::provider::{DescriptionProvider, ProbeInfo, ProviderError, Snippet};
use crate::descriptions::schema::{self, Description, Provenance, RawDescription};
use serde::Deserialize;
use std::time::Duration;

pub struct OllamaProvider {
    endpoint: String,
    model: String,
    timeout: Duration,
    agent: ureq::Agent,
}

impl OllamaProvider {
    pub fn new(endpoint: String, model: String, timeout_ms: u64) -> Self {
        // ureq 3 timeout API: via config builder
        let agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_millis(timeout_ms)))
            .build()
            .into();
        Self {
            endpoint: endpoint.trim_end_matches('/').to_string(),
            model,
            timeout: Duration::from_millis(timeout_ms),
            agent,
        }
    }

    fn build_prompt(&self, s: &Snippet) -> String {
        let mut p = String::new();
        p.push_str("You are a Rust code summarizer. Write a one-sentence description ");
        p.push_str("of what this entity does. Use active voice, present tense. ");
        p.push_str("Max 60 words. No code snippets. Respond with ONLY a JSON object ");
        p.push_str("of the form {\"description\": \"...\", \"confidence\": 0.0-1.0}.\n\n");
        p.push_str(&format!("Entity: {} ({})\n", s.entity_name, s.entity_type));
        if !s.doc.is_empty() {
            p.push_str(&format!("Doc: {}\n", s.doc));
        }
        if !s.body.is_empty() {
            p.push_str(&format!("Body (first lines):\n{}\n", s.body));
        }
        p
    }
}

#[derive(Deserialize)]
struct TagsResponse {
    models: Vec<TagsEntry>,
}

#[derive(Deserialize)]
struct TagsEntry {
    name: String,
}

#[derive(Deserialize)]
struct GenerateResponse {
    response: String,
}

impl DescriptionProvider for OllamaProvider {
    fn label(&self) -> &str {
        "local-ollama"
    }

    fn model(&self) -> &str {
        &self.model
    }

    fn probe(&self) -> Result<ProbeInfo, ProviderError> {
        let url = format!("{}/api/tags", self.endpoint);
        let mut response = self
            .agent
            .get(&url)
            .call()
            .map_err(|e| classify_error(e, self.timeout.as_millis() as u64))?;
        let body: TagsResponse = response
            .body_mut()
            .read_json()
            .map_err(|e| ProviderError::MalformedResponse(format!("/api/tags: {e}")))?;
        let available_models: Vec<String> = body.models.into_iter().map(|m| m.name).collect();
        let selected_available = available_models
            .iter()
            .any(|m| model_matches(m, &self.model));
        Ok(ProbeInfo {
            provider_label: self.label().to_string(),
            model: self.model.clone(),
            available_models,
            selected_available,
        })
    }

    fn extract(&self, snippet: &Snippet) -> Result<Description, ProviderError> {
        let url = format!("{}/api/generate", self.endpoint);
        let prompt = self.build_prompt(snippet);
        let payload = serde_json::json!({
            "model": self.model,
            "prompt": prompt,
            "stream": false,
            "format": "json"
        });

        let mut response = self
            .agent
            .post(&url)
            .send_json(payload)
            .map_err(|e| classify_error(e, self.timeout.as_millis() as u64))?;

        let parsed: GenerateResponse = response
            .body_mut()
            .read_json()
            .map_err(|e| ProviderError::MalformedResponse(format!("/api/generate: {e}")))?;

        // Ollama wraps the model output in `response`. With `format: json`,
        // the content is a JSON string itself.
        let raw: RawDescription = serde_json::from_str(&parsed.response).map_err(|e| {
            ProviderError::MalformedResponse(format!(
                "model returned non-JSON or wrong shape: {e} (raw={})",
                truncate_for_log(&parsed.response, 200)
            ))
        })?;

        let provenance = Provenance {
            provider: self.label().into(),
            model: self.model.clone(),
            extracted_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            redactions: 0, // Filled in by orchestrator with snippet's redaction count
        };
        schema::validate(raw, 60, provenance)
    }
}

/// Ollama reports model names like "qwen2.5-coder:7b"; users may configure
/// "qwen2.5-coder" (no tag). Match on the base name if the tag is absent.
fn model_matches(available: &str, requested: &str) -> bool {
    if available == requested {
        return true;
    }
    if !requested.contains(':') {
        // Strip the tag from `available` and compare.
        if let Some(base) = available.split(':').next() {
            return base == requested;
        }
    }
    false
}

fn classify_error(e: ureq::Error, timeout_ms: u64) -> ProviderError {
    let msg = e.to_string();
    let lower = msg.to_ascii_lowercase();
    if lower.contains("timed out") || lower.contains("timeout") {
        return ProviderError::Timeout(timeout_ms);
    }
    // ureq 3 surfaces HTTP status errors with a `.status()` method on the error variant.
    match e {
        ureq::Error::StatusCode(status) => {
            if status == 429 {
                ProviderError::RateLimited(status)
            } else {
                ProviderError::ServerError { status, body: msg }
            }
        }
        _ if lower.contains("refused") || lower.contains("dns") || lower.contains("connection") => {
            ProviderError::Unreachable(msg)
        }
        _ => ProviderError::Transport(msg),
    }
}

fn truncate_for_log(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        let cut: String = s.chars().take(max).collect();
        format!("{cut}…")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_matches_exact() {
        assert!(model_matches("qwen2.5-coder:7b", "qwen2.5-coder:7b"));
    }

    #[test]
    fn model_matches_base_name_only() {
        assert!(model_matches("qwen2.5-coder:7b", "qwen2.5-coder"));
    }

    #[test]
    fn model_does_not_match_different_base() {
        assert!(!model_matches("llama3:8b", "qwen2.5-coder"));
    }

    #[test]
    fn model_does_not_match_if_requested_has_tag_but_mismatch() {
        assert!(!model_matches("qwen2.5-coder:7b", "qwen2.5-coder:1b"));
    }

    #[test]
    fn truncate_for_log_works() {
        assert_eq!(truncate_for_log("hello", 100), "hello");
        assert_eq!(truncate_for_log("abcdefghij", 3), "abc…");
    }
}
