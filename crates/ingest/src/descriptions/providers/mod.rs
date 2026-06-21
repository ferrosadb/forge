//! Concrete provider implementations.
//!
//! v1 ships `Ollama`, `Skip`, and a `Mock` for tests. OpenAI and Anthropic
//! are deferred — add a new file here and wire it into
//! `build_provider()` in `mod.rs` when implementing.

pub mod mock;
pub mod ollama;
pub mod skip;

use crate::descriptions::config::{DescriptionsConfig, Provider};
use crate::descriptions::provider::DescriptionProvider;
use anyhow::{bail, Result};
use std::sync::Arc;

/// Construct a provider from config.
pub fn build_provider(cfg: &DescriptionsConfig) -> Result<Arc<dyn DescriptionProvider>> {
    match cfg.provider {
        Provider::Local => Ok(Arc::new(ollama::OllamaProvider::new(
            cfg.local_endpoint.clone(),
            cfg.local_model.clone(),
            cfg.local_timeout_ms,
        ))),
        Provider::Skip => Ok(Arc::new(skip::SkipProvider)),
        Provider::Openai | Provider::Anthropic => bail!(
            "provider '{:?}' not yet implemented in v1; use 'local' or 'skip' \
             (OpenAI / Anthropic are a followup — see feat-ingest-function-descriptions)",
            cfg.provider
        ),
    }
}
