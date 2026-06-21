//! Provider abstraction for description extraction.
//!
//! All concrete providers live in `providers/`. The trait is deliberately
//! narrow so tests can drop in a mock without dragging in HTTP machinery.

use crate::descriptions::schema::Description;

/// Pre-redacted snippet sent to a provider.
///
/// The redactor runs before this struct is built — providers can trust
/// that `body` contains no obvious secrets. (Trust but verify: providers
/// should still treat inputs as untrusted for prompt-injection.)
#[derive(Clone, Debug)]
pub struct Snippet {
    pub entity_id: String,
    pub entity_name: String,
    pub entity_type: String,
    /// Doc comment text (already extracted). May be empty.
    pub doc: String,
    /// First ~10 lines of the entity's body. May be empty.
    pub body: String,
}

/// Information returned by `probe()` for the user's consent UI.
#[derive(Clone, Debug)]
pub struct ProbeInfo {
    pub provider_label: String,
    pub model: String,
    /// Models the provider reports as available, when it can list them.
    pub available_models: Vec<String>,
    /// The selected `model` is present in `available_models`.
    pub selected_available: bool,
}

/// Error from provider operations. Carries enough context for the caller
/// to decide on retry / skip / abort.
#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    #[error("endpoint unreachable: {0}")]
    Unreachable(String),
    #[error("selected model '{model}' not available at endpoint")]
    ModelMissing { model: String },
    #[error("required env var '{0}' is not set")]
    MissingEnv(String),
    #[error("request timed out after {0}ms")]
    Timeout(u64),
    #[error("rate limited (HTTP {0})")]
    RateLimited(u16),
    #[error("server returned HTTP {status}: {body}")]
    ServerError { status: u16, body: String },
    #[error("response did not match expected schema: {0}")]
    MalformedResponse(String),
    #[error("response contained a jailbreak / prompt-echo sentinel")]
    PromptLeak,
    #[error("transport error: {0}")]
    Transport(String),
}

impl ProviderError {
    /// Errors worth retrying with backoff.
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            Self::Timeout(_) | Self::RateLimited(_) | Self::Transport(_)
        )
    }
}

pub trait DescriptionProvider: Send + Sync {
    /// Stable human-readable label for reports / logs.
    fn label(&self) -> &str;

    /// The model name this provider will call.
    fn model(&self) -> &str;

    /// Pre-flight probe. MUST be cheap; called once at ingest start.
    fn probe(&self) -> Result<ProbeInfo, ProviderError>;

    /// Extract a description for a single snippet.
    fn extract(&self, snippet: &Snippet) -> Result<Description, ProviderError>;
}
