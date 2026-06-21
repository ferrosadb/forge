//! Typed wrapper over fmem's `batch_update_entities` MCP tool.
//!
//! PATCH semantics: each entry carries an `entity_id` and any subset of
//! the mutable fields the server recognises.  Fields not present in the
//! request are left untouched server-side (per the live
//! `batch_update_entities` handler in `ferrosa-memory-core/dispatch.rs`).
//!
//! Per-call cap: 100 entities.  Callers that need more must split.
//!
//! Response envelope carries `updated / unchanged / not_found / errors /
//! total / results[]` — distinct from `ingest_entities`'s nested shape.

use serde::{Deserialize, Serialize};

use crate::error::Error;
use crate::transport::Transport;

/// Maximum entries per call enforced by the server.
pub const BATCH_UPDATE_MAX: usize = 100;

/// One PATCH entry.  `entity_id` is required; every other field is
/// optional — present fields are applied, absent fields stay untouched.
///
/// The `properties` field maps to the server's generic attributes store;
/// it's the write-side analogue of `ingest_entities`'s `attrs`.
#[derive(Debug, Clone, Serialize, Default)]
pub struct WirePatchEntity {
    pub entity_id: String,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub entity_name: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub entity_type: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_snippet: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_fold_id: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f64>,

    /// `"active"` | `"dormant"` | `"silent"` | `"unavailable"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,

    /// Server-side generic attribute object.  Pass `Some(serde_json::Value::Null)`
    /// to clear the attributes store in-place; `None` leaves it untouched.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub properties: Option<serde_json::Value>,

    /// Replacement embedding vector.  `None` leaves untouched; `Some(Vec::new())`
    /// replaces with an empty vector; explicit `Some(vec![...])` replaces.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub embedding: Option<Vec<f32>>,
}

/// Request args for `batch_update_entities`.
#[derive(Debug, Clone, Serialize)]
pub struct BatchUpdateEntitiesArgs {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    pub entities: Vec<WirePatchEntity>,
}

/// One result entry in the `results[]` array.
#[derive(Debug, Clone, Deserialize)]
pub struct PatchResult {
    pub index: usize,
    #[serde(default)]
    pub entity_id: String,
    pub status: String, // "updated" | "unchanged" | "not_found" | "error"
    #[serde(default)]
    pub reason: String, // populated when status == "error"
}

impl PatchResult {
    pub fn is_ok(&self) -> bool {
        matches!(self.status.as_str(), "updated" | "unchanged" | "not_found")
    }
}

/// Parsed response.  The numeric counters are authoritative; `results[]`
/// gives per-row detail.
#[derive(Debug, Clone, Deserialize)]
pub struct BatchUpdateEntitiesResponse {
    #[serde(default)]
    pub updated: usize,
    #[serde(default)]
    pub unchanged: usize,
    #[serde(default)]
    pub not_found: usize,
    #[serde(default)]
    pub errors: usize,
    #[serde(default)]
    pub total: usize,
    #[serde(default)]
    pub results: Vec<PatchResult>,
}

impl BatchUpdateEntitiesResponse {
    /// Rows accounted for in the response numbers — MUST equal `total`.
    pub fn accounted(&self) -> usize {
        self.updated + self.unchanged + self.not_found + self.errors
    }
}

/// Call fmem's `batch_update_entities` MCP tool.
///
/// Caller MUST split input into batches ≤ `BATCH_UPDATE_MAX`; we fail
/// loud here rather than silently truncate.
pub fn batch_update_entities(
    transport: &dyn Transport,
    args: BatchUpdateEntitiesArgs,
) -> Result<BatchUpdateEntitiesResponse, Error> {
    if args.entities.len() > BATCH_UPDATE_MAX {
        return Err(Error::Schema(format!(
            "batch_update_entities: {} entities exceeds cap of {}",
            args.entities.len(),
            BATCH_UPDATE_MAX,
        )));
    }
    let value = serde_json::to_value(&args)
        .map_err(|e| Error::Schema(format!("failed to serialize BatchUpdateEntitiesArgs: {e}")))?;
    let raw = transport.call_tool("batch_update_entities", value)?;
    serde_json::from_value(raw.clone()).map_err(|e| {
        Error::Schema(format!(
            "batch_update_entities response did not match expected shape: {e}; raw={raw}"
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::mock::{MockTransport, ScriptedResponse};
    use serde_json::json;

    fn one_patch() -> BatchUpdateEntitiesArgs {
        BatchUpdateEntitiesArgs {
            session_id: Some("00000000-0000-0000-0000-000000000001".into()),
            entities: vec![WirePatchEntity {
                entity_id: "e1".into(),
                entity_name: Some("renamed".into()),
                ..Default::default()
            }],
        }
    }

    #[test]
    fn parses_mixed_statuses() {
        let m = MockTransport::new();
        m.expect_call(
            "tools/call",
            ScriptedResponse::Ok(json!({
                "updated": 1,
                "unchanged": 1,
                "not_found": 1,
                "errors": 1,
                "total": 4,
                "results": [
                    { "index": 0, "entity_id": "e1", "status": "updated" },
                    { "index": 1, "entity_id": "e2", "status": "unchanged" },
                    { "index": 2, "entity_id": "e3", "status": "not_found" },
                    { "index": 3, "entity_id": "e4", "status": "error", "reason": "entity_name must be a string" }
                ]
            })),
        );
        let mut args = one_patch();
        // Expand to 4 patches so total=4 makes sense
        args.entities = (0..4)
            .map(|i| WirePatchEntity {
                entity_id: format!("e{}", i + 1),
                ..Default::default()
            })
            .collect();
        let r = batch_update_entities(&m, args).unwrap();
        assert_eq!(r.total, 4);
        assert_eq!(r.updated, 1);
        assert_eq!(r.unchanged, 1);
        assert_eq!(r.not_found, 1);
        assert_eq!(r.errors, 1);
        assert_eq!(r.accounted(), 4);
        assert_eq!(r.results[3].reason, "entity_name must be a string");
        m.assert_done();
    }

    #[test]
    fn rejects_oversize_batch_locally() {
        let m = MockTransport::panicking();
        let args = BatchUpdateEntitiesArgs {
            session_id: None,
            entities: (0..101)
                .map(|i| WirePatchEntity {
                    entity_id: format!("e{i}"),
                    ..Default::default()
                })
                .collect(),
        };
        let err = batch_update_entities(&m, args).unwrap_err();
        assert!(matches!(err, Error::Schema(ref msg) if msg.contains("exceeds cap")));
    }

    #[test]
    fn patch_only_emits_requested_fields() {
        let e = WirePatchEntity {
            entity_id: "e1".into(),
            entity_name: Some("new-name".into()),
            ..Default::default()
        };
        let v = serde_json::to_value(&e).unwrap();
        // Only entity_id + entity_name; other Option fields omitted.
        assert_eq!(v["entity_id"], "e1");
        assert_eq!(v["entity_name"], "new-name");
        assert!(v.get("entity_type").is_none());
        assert!(v.get("confidence").is_none());
    }
}
