//! Mock provider for hermetic tests.
//!
//! Drives deterministic behavior without network I/O. Construct with
//! `MockProvider::always_ok("desc")` for happy-path tests, or
//! `MockProvider::scripted(...)` to assert retry / failure paths.

use crate::descriptions::provider::{DescriptionProvider, ProbeInfo, ProviderError, Snippet};
use crate::descriptions::schema::{self, Description, Provenance, RawDescription};
use std::sync::Mutex;

/// Scripted behavior for a single call.
#[derive(Clone, Debug)]
pub enum MockBehavior {
    Ok(String),
    Error(&'static str),
    PromptLeak,
    Timeout,
    RateLimit,
}

pub struct MockProvider {
    script: Mutex<Vec<MockBehavior>>,
    /// When script is empty, this default fires.
    default: MockBehavior,
    probe_result: Mutex<Result<ProbeInfo, ProviderError>>,
}

impl MockProvider {
    pub fn always_ok(description: &str) -> Self {
        Self {
            script: Mutex::new(Vec::new()),
            default: MockBehavior::Ok(description.to_string()),
            probe_result: Mutex::new(Ok(ProbeInfo {
                provider_label: "mock".into(),
                model: "mock-model".into(),
                available_models: vec!["mock-model".into()],
                selected_available: true,
            })),
        }
    }

    pub fn scripted(sequence: Vec<MockBehavior>) -> Self {
        Self {
            script: Mutex::new(sequence),
            default: MockBehavior::Error("script exhausted"),
            probe_result: Mutex::new(Ok(ProbeInfo {
                provider_label: "mock".into(),
                model: "mock-model".into(),
                available_models: vec!["mock-model".into()],
                selected_available: true,
            })),
        }
    }

    pub fn with_probe_error(self, err: ProviderError) -> Self {
        *self.probe_result.lock().expect("mock probe lock") = Err(err);
        self
    }
}

impl DescriptionProvider for MockProvider {
    fn label(&self) -> &str {
        "mock"
    }
    fn model(&self) -> &str {
        "mock-model"
    }

    fn probe(&self) -> Result<ProbeInfo, ProviderError> {
        match &*self.probe_result.lock().expect("probe lock") {
            Ok(info) => Ok(info.clone()),
            Err(e) => Err(clone_error(e)),
        }
    }

    fn extract(&self, _snippet: &Snippet) -> Result<Description, ProviderError> {
        let behavior = {
            let mut script = self.script.lock().expect("script lock");
            if script.is_empty() {
                self.default.clone()
            } else {
                script.remove(0)
            }
        };
        match behavior {
            MockBehavior::Ok(desc) => {
                let raw = RawDescription {
                    description: desc,
                    confidence: 0.85,
                };
                let provenance = Provenance {
                    provider: "mock".into(),
                    model: "mock-model".into(),
                    extracted_at: "2026-04-17T00:00:00Z".into(),
                    redactions: 0,
                };
                schema::validate(raw, 60, provenance)
            }
            MockBehavior::Error(msg) => Err(ProviderError::Transport(msg.to_string())),
            MockBehavior::PromptLeak => Err(ProviderError::PromptLeak),
            MockBehavior::Timeout => Err(ProviderError::Timeout(1000)),
            MockBehavior::RateLimit => Err(ProviderError::RateLimited(429)),
        }
    }
}

/// ProviderError isn't `Clone` by default; provide a lossy clone for tests.
fn clone_error(e: &ProviderError) -> ProviderError {
    match e {
        ProviderError::Unreachable(s) => ProviderError::Unreachable(s.clone()),
        ProviderError::ModelMissing { model } => ProviderError::ModelMissing {
            model: model.clone(),
        },
        ProviderError::MissingEnv(s) => ProviderError::MissingEnv(s.clone()),
        ProviderError::Timeout(n) => ProviderError::Timeout(*n),
        ProviderError::RateLimited(n) => ProviderError::RateLimited(*n),
        ProviderError::ServerError { status, body } => ProviderError::ServerError {
            status: *status,
            body: body.clone(),
        },
        ProviderError::MalformedResponse(s) => ProviderError::MalformedResponse(s.clone()),
        ProviderError::PromptLeak => ProviderError::PromptLeak,
        ProviderError::Transport(s) => ProviderError::Transport(s.clone()),
    }
}
