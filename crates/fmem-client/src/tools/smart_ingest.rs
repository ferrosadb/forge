//! Typed wrapper over fmem's `smart_ingest` MCP tool.
//!
//! `smart_ingest` is fmem's prediction-error-gated write path. It decides
//! whether incoming content should create a new entity, update an existing one,
//! supersede stale content, or be skipped as redundant. Forge uses this for
//! untrusted paper/corpus ingestion where deduplication and memory evolution are
//! preferable to blind bulk upserts.

use serde::{Deserialize, Serialize};

use crate::error::Error;
use crate::transport::Transport;

/// Request body for fmem's `smart_ingest` tool.
#[derive(Debug, Clone, Serialize)]
pub struct SmartIngestArgs {
    /// Session UUID. When omitted, fmem uses its configured/default session.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// The memory content/context to ingest. fmem's schema caps this at 8192
    /// chars; callers should pre-truncate rather than relying on server reject.
    pub content: String,
    /// Entity type (e.g. `document`, `concept`, `person`).
    pub entity_type: String,
    /// Stable clean entity name. When omitted, fmem may infer one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entity_name: Option<String>,
    /// Optional embedding vector. Usually omitted so fmem can generate it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub embedding: Option<Vec<f32>>,
    /// Optional fold UUID for provenance.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_fold_id: Option<String>,
}

/// Parsed response from fmem's `smart_ingest` tool.
#[derive(Debug, Clone, Deserialize)]
pub struct SmartIngestResponse {
    pub action: String,
    #[serde(default)]
    pub entity_id: Option<String>,
    #[serde(default)]
    pub new_entity_id: Option<String>,
    #[serde(default)]
    pub old_entity_id: Option<String>,
    #[serde(default)]
    pub existing_entity_id: Option<String>,
    #[serde(default)]
    pub similarity: Option<f64>,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub hint: Option<String>,
    #[serde(default, rename = "_hint")]
    pub progressive_hint: Option<String>,
}

impl SmartIngestResponse {
    /// Return the entity id that downstream edge creation should point at.
    ///
    /// Created/Updated responses use `entity_id`; Superseded uses
    /// `new_entity_id`; Skipped uses `existing_entity_id`.
    pub fn resolved_entity_id(&self) -> Option<String> {
        self.entity_id
            .clone()
            .or_else(|| self.new_entity_id.clone())
            .or_else(|| self.existing_entity_id.clone())
    }
}

/// Call fmem's `smart_ingest` MCP tool and parse the response.
pub fn smart_ingest(
    transport: &dyn Transport,
    args: SmartIngestArgs,
) -> Result<SmartIngestResponse, Error> {
    let args_value = serde_json::to_value(&args)
        .map_err(|e| Error::Schema(format!("failed to serialize SmartIngestArgs: {e}")))?;
    let raw = transport.call_tool("smart_ingest", args_value)?;
    parse_response(raw)
}

fn parse_response(raw: serde_json::Value) -> Result<SmartIngestResponse, Error> {
    serde_json::from_value(raw.clone()).map_err(|e| {
        Error::Schema(format!(
            "smart_ingest response did not match expected shape: {e}; raw={raw}"
        ))
    })
}
