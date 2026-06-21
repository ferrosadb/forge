//! No-op provider. Used when the user selects `provider = "skip"` or
//! when probe fails and the user elects to continue without descriptions.

use crate::descriptions::provider::{DescriptionProvider, ProbeInfo, ProviderError, Snippet};
use crate::descriptions::schema::Description;

pub struct SkipProvider;

impl DescriptionProvider for SkipProvider {
    fn label(&self) -> &str {
        "skip"
    }

    fn model(&self) -> &str {
        "none"
    }

    fn probe(&self) -> Result<ProbeInfo, ProviderError> {
        Ok(ProbeInfo {
            provider_label: "skip".into(),
            model: "none".into(),
            available_models: vec![],
            selected_available: true,
        })
    }

    fn extract(&self, _snippet: &Snippet) -> Result<Description, ProviderError> {
        // Intentional: skip provider never produces descriptions.
        Err(ProviderError::MalformedResponse(
            "skip provider does not extract".into(),
        ))
    }
}
