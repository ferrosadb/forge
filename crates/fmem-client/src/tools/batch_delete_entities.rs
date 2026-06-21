//! Typed wrapper over fmem's `batch_delete_entities` MCP tool.
//!
//! Hard-delete semantics: existing rows are removed from ferrosa-memory
//! owned storage.  The server handler at
//! `ferrosa-memory-core/dispatch.rs::handle_batch_delete_entities`
//! reports per-row status (`deleted` | `not_found` | `error`).
//!
//! Per-call cap: 100 entities.  Callers split larger sets.
//!
//! No cascade option is defined by the server — edges pointing at a
//! deleted entity become dangling on the graph side.  Consumers of the
//! graph should filter on resident entity id when traversing.

use serde::{Deserialize, Serialize};

use crate::error::Error;
use crate::transport::Transport;

pub const BATCH_DELETE_MAX: usize = 100;

/// One delete target: just the entity id.
#[derive(Debug, Clone, Serialize)]
pub struct WireDeleteTarget {
    pub entity_id: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct BatchDeleteEntitiesArgs {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    pub entities: Vec<WireDeleteTarget>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DeleteResult {
    pub index: usize,
    #[serde(default)]
    pub entity_id: String,
    pub status: String, // "deleted" | "not_found" | "error"
    #[serde(default)]
    pub reason: String,
}

impl DeleteResult {
    pub fn is_ok(&self) -> bool {
        matches!(self.status.as_str(), "deleted" | "not_found")
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct BatchDeleteEntitiesResponse {
    #[serde(default)]
    pub deleted: usize,
    #[serde(default)]
    pub not_found: usize,
    #[serde(default)]
    pub errors: usize,
    #[serde(default)]
    pub total: usize,
    #[serde(default)]
    pub results: Vec<DeleteResult>,
}

impl BatchDeleteEntitiesResponse {
    pub fn accounted(&self) -> usize {
        self.deleted + self.not_found + self.errors
    }
}

/// Call fmem's `batch_delete_entities` MCP tool.
pub fn batch_delete_entities(
    transport: &dyn Transport,
    args: BatchDeleteEntitiesArgs,
) -> Result<BatchDeleteEntitiesResponse, Error> {
    if args.entities.len() > BATCH_DELETE_MAX {
        return Err(Error::Schema(format!(
            "batch_delete_entities: {} entities exceeds cap of {}",
            args.entities.len(),
            BATCH_DELETE_MAX,
        )));
    }
    let value = serde_json::to_value(&args)
        .map_err(|e| Error::Schema(format!("failed to serialize BatchDeleteEntitiesArgs: {e}")))?;
    let raw = transport.call_tool("batch_delete_entities", value)?;
    serde_json::from_value(raw.clone()).map_err(|e| {
        Error::Schema(format!(
            "batch_delete_entities response did not match expected shape: {e}; raw={raw}"
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::mock::{MockTransport, ScriptedResponse};
    use serde_json::json;

    #[test]
    fn parses_mixed_statuses() {
        let m = MockTransport::new();
        m.expect_call(
            "tools/call",
            ScriptedResponse::Ok(json!({
                "deleted": 2,
                "not_found": 1,
                "errors": 0,
                "total": 3,
                "results": [
                    { "index": 0, "entity_id": "e1", "status": "deleted" },
                    { "index": 1, "entity_id": "e2", "status": "deleted" },
                    { "index": 2, "entity_id": "e3", "status": "not_found" }
                ]
            })),
        );
        let args = BatchDeleteEntitiesArgs {
            session_id: None,
            entities: vec![
                WireDeleteTarget {
                    entity_id: "e1".into(),
                },
                WireDeleteTarget {
                    entity_id: "e2".into(),
                },
                WireDeleteTarget {
                    entity_id: "e3".into(),
                },
            ],
        };
        let r = batch_delete_entities(&m, args).unwrap();
        assert_eq!(r.total, 3);
        assert_eq!(r.deleted, 2);
        assert_eq!(r.not_found, 1);
        assert_eq!(r.accounted(), 3);
        assert!(r.results.iter().all(|s| s.is_ok()));
        m.assert_done();
    }

    #[test]
    fn rejects_oversize_batch_locally() {
        let m = MockTransport::panicking();
        let args = BatchDeleteEntitiesArgs {
            session_id: None,
            entities: (0..101)
                .map(|i| WireDeleteTarget {
                    entity_id: format!("e{i}"),
                })
                .collect(),
        };
        let err = batch_delete_entities(&m, args).unwrap_err();
        assert!(matches!(err, Error::Schema(ref msg) if msg.contains("exceeds cap")));
    }
}
