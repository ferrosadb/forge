//! Typed wrapper over fmem's `ingest_entities` MCP tool.
//!
//! `ingest_entities` is the bulk upsert path for code entities and edges.
//! Every call returns a response envelope that MUST satisfy:
//!
//!   `requested == succeeded + skipped + len(failed)`
//!
//! Clients are responsible for asserting this invariant (FMEA F3, RPN 567).
//! This module provides the types only; the reconciliation logic lives in
//! `forge_ingest::graph_loader` so it can be tested and reused independently.
//!
//! Wire format follows the "Common conventions" section of
//! `specs/feat-code-graph-ingest/deps/ferrosa-memory-crud.md`.

use serde::{Deserialize, Serialize};

use crate::error::Error;
use crate::transport::Transport;

// ---------------------------------------------------------------------------
// Request types
// ---------------------------------------------------------------------------

/// Options controlling upsert/conflict behaviour and embedding generation.
///
/// Field names must match fmem's `IngestOptions` exactly; unknown fields
/// trigger `schema_mismatch` per-row failures.
#[derive(Debug, Clone, Serialize)]
pub struct IngestOptions {
    /// `"update"` (default): upsert — overwrite on id conflict.
    /// `"skip"`: ignore rows whose id already exists.
    pub on_conflict: String,

    /// If `true`, fmem rejects edges whose src_id or dst_id is not already
    /// committed in the same or an earlier batch.
    pub strict_edges: bool,

    /// If `true`, fmem generates embeddings server-side.
    /// forge generates embeddings separately; set `false` here.
    pub embed_missing: bool,
}

impl Default for IngestOptions {
    fn default() -> Self {
        Self {
            on_conflict: "update".to_string(),
            strict_edges: true,
            embed_missing: false,
        }
    }
}

/// Wire-format entity sent to fmem's `ingest_entities`.
///
/// Flat structure matching the server's expected JSON shape.
/// Fields not known to the server MUST NOT be included; this type
/// is the single authoritative translation layer between forge's internal
/// `Entity` struct and the wire format.
#[derive(Debug, Clone, Serialize)]
pub struct WireEntity {
    pub id: String,
    pub name: String,
    pub entity_type: String,
    pub context: String,

    /// Confidence score in [0.0, 1.0].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f64>,

    /// Lifecycle state: `"active"` | `"stale"` | `"pending"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,

    /// Per-entity-type structured attributes.
    /// Carries all optional fields the server stores under `attrs`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attrs: Option<serde_json::Value>,
}

/// Wire-format edge sent to fmem's `ingest_entities`.
#[derive(Debug, Clone, Serialize)]
pub struct WireEdge {
    pub src_id: String,
    pub dst_id: String,
    pub edge_type: String,
    pub weight: f64,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
}

/// Full request body for `ingest_entities`.
#[derive(Debug, Clone, Serialize)]
pub struct IngestEntitiesArgs {
    pub tenant_id: String,
    pub session_id: String,
    pub entities: Vec<WireEntity>,
    pub edges: Vec<WireEdge>,
    pub options: IngestOptions,
}

// ---------------------------------------------------------------------------
// Response types
// ---------------------------------------------------------------------------

/// One entry in a `failed` array.  Entity failures carry an `id`; edge
/// failures carry `src_id` / `dst_id` / `edge_type` per the server's
/// composite-key shape.  We accept both via serde's flatten/default.
#[derive(Debug, Clone, Deserialize)]
pub struct WireFailedRow {
    /// Entity id when the failure is an entity; empty for edge failures.
    #[serde(default)]
    pub id: String,
    /// Edge src when the failure is an edge.
    #[serde(default)]
    pub src_id: String,
    /// Edge dst when the failure is an edge.
    #[serde(default)]
    pub dst_id: String,
    /// Edge type when the failure is an edge.
    #[serde(default)]
    pub edge_type: String,
    /// Server error reason (e.g. `"schema_mismatch"`, `"endpoint_not_found"`).
    pub reason: String,
}

impl WireFailedRow {
    /// Derive a stable key for logs and set-membership checks: the
    /// `id` for entity failures, or the `(src, type, dst)` composite
    /// for edge failures.
    pub fn key(&self) -> String {
        if !self.id.is_empty() {
            self.id.clone()
        } else {
            format!("{}:{}:{}", self.src_id, self.edge_type, self.dst_id)
        }
    }
}

/// Entity sub-envelope of the `ingest_entities` response.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct EntityStats {
    #[serde(default)]
    pub inserted: usize,
    #[serde(default)]
    pub updated: usize,
    #[serde(default)]
    pub skipped: usize,
    #[serde(default)]
    pub failed: Vec<WireFailedRow>,
}

impl EntityStats {
    /// Rows accounted for: inserted + updated + skipped + failed.len().
    /// Equal to the number of rows sent when the server is honest.
    pub fn accounted(&self) -> usize {
        self.inserted + self.updated + self.skipped + self.failed.len()
    }
}

/// Edge sub-envelope of the `ingest_entities` response.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct EdgeStats {
    #[serde(default)]
    pub inserted: usize,
    /// Per server's response key — dup edges are the "skipped" bucket for edges.
    #[serde(default)]
    pub skipped_duplicate: usize,
    #[serde(default)]
    pub failed: Vec<WireFailedRow>,
}

impl EdgeStats {
    pub fn accounted(&self) -> usize {
        self.inserted + self.skipped_duplicate + self.failed.len()
    }
}

/// Embedding sub-envelope (informational; not reconciled against rows sent).
#[derive(Debug, Clone, Deserialize, Default)]
pub struct EmbeddingStats {
    #[serde(default)]
    pub computed: usize,
    #[serde(default)]
    pub received: usize,
    #[serde(default)]
    pub failed: Vec<WireFailedRow>,
}

/// Parsed response from `ingest_entities`.
///
/// Shape matches the server's `handle_ingest_entities` response exactly:
/// nested `entities` / `edges` / `embeddings` sub-structs.  Callers must
/// check `entities.accounted() == entities_sent` and similarly for edges
/// (FMEA F3, RPN 567) — see `forge_ingest::graph_loader::reconcile`.
#[derive(Debug, Clone, Deserialize)]
pub struct IngestEntitiesResponse {
    #[serde(default)]
    pub entities: EntityStats,
    #[serde(default)]
    pub edges: EdgeStats,
    #[serde(default)]
    pub embeddings: EmbeddingStats,
    /// Server-reported schema version (informational).
    #[serde(default)]
    pub schema_version: String,
    /// Server-reported call latency.
    #[serde(default)]
    pub duration_ms: u64,
}

// ---------------------------------------------------------------------------
// Caller
// ---------------------------------------------------------------------------

/// Call fmem's `ingest_entities` MCP tool and return the typed response.
///
/// Does **not** assert the count-reconciliation invariant — that belongs
/// in `graph_loader::reconcile()` which has the surrounding context needed
/// to produce a useful error message.
///
/// Accepts `&dyn Transport` so callers that hold a trait object (e.g.
/// `GraphLoader` which stores `&'a dyn Transport`) can call this without
/// monomorphisation. Callers that hold a concrete type can pass `&concrete`
/// since every `&T where T: Transport` coerces to `&dyn Transport`.
pub fn ingest_entities(
    transport: &dyn Transport,
    args: IngestEntitiesArgs,
) -> Result<IngestEntitiesResponse, Error> {
    let args_value = serde_json::to_value(&args)
        .map_err(|e| Error::Schema(format!("failed to serialize IngestEntitiesArgs: {e}")))?;
    let raw = transport.call_tool("ingest_entities", args_value)?;
    parse_response(raw)
}

fn parse_response(raw: serde_json::Value) -> Result<IngestEntitiesResponse, Error> {
    serde_json::from_value(raw.clone()).map_err(|e| {
        Error::Schema(format!(
            "ingest_entities response did not match expected shape: {e}; raw={raw}"
        ))
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::mock::{MockTransport, ScriptedResponse};
    use serde_json::json;

    fn minimal_args(n_entities: usize, n_edges: usize) -> IngestEntitiesArgs {
        let entities = (0..n_entities)
            .map(|i| WireEntity {
                id: format!("entity-{i}"),
                name: format!("Entity{i}"),
                entity_type: "function".to_string(),
                context: format!("context {i}"),
                confidence: None,
                state: None,
                attrs: None,
            })
            .collect();
        let edges = (0..n_edges)
            .map(|i| WireEdge {
                src_id: format!("entity-{i}"),
                dst_id: format!("entity-{}", i + 1),
                edge_type: "calls".to_string(),
                weight: 1.0,
                metadata: None,
            })
            .collect();
        IngestEntitiesArgs {
            tenant_id: "00000000-0000-0000-0000-000000000001".to_string(),
            session_id: "00000000-0000-0000-0000-000000000002".to_string(),
            entities,
            edges,
            options: IngestOptions::default(),
        }
    }

    #[test]
    fn parses_success_response() {
        let m = MockTransport::new();
        m.expect_call(
            "tools/call",
            ScriptedResponse::Ok(json!({
                "entities": { "inserted": 3, "updated": 0, "skipped": 0, "failed": [] },
                "edges": { "inserted": 0, "skipped_duplicate": 0, "failed": [] },
                "embeddings": { "computed": 0, "received": 0, "failed": [] },
                "schema_version": "2026-03-01",
                "duration_ms": 12,
            })),
        );
        let resp = ingest_entities(&m, minimal_args(3, 0)).unwrap();
        assert_eq!(resp.entities.inserted, 3);
        assert_eq!(resp.entities.skipped, 0);
        assert!(resp.entities.failed.is_empty());
        assert_eq!(resp.entities.accounted(), 3);
        m.assert_done();
    }

    #[test]
    fn parses_partial_failure_response() {
        let m = MockTransport::new();
        m.expect_call(
            "tools/call",
            ScriptedResponse::Ok(json!({
                "entities": {
                    "inserted": 2, "updated": 0, "skipped": 0,
                    "failed": [
                        { "id": "entity-2", "reason": "schema_mismatch: unknown field 'extra'" }
                    ]
                },
                "edges": { "inserted": 0, "skipped_duplicate": 0, "failed": [] },
                "embeddings": { "computed": 0, "received": 0, "failed": [] },
                "schema_version": "2026-03-01",
                "duration_ms": 8,
            })),
        );
        let resp = ingest_entities(&m, minimal_args(3, 0)).unwrap();
        assert_eq!(resp.entities.inserted, 2);
        assert_eq!(resp.entities.failed.len(), 1);
        assert_eq!(resp.entities.failed[0].id, "entity-2");
        assert_eq!(resp.entities.accounted(), 3);
        m.assert_done();
    }

    #[test]
    fn tool_error_propagates_as_error_tool() {
        let m = MockTransport::new();
        m.expect_call(
            "tools/call",
            ScriptedResponse::ToolError {
                code: -32602,
                message: "payload exceeds max_payload_bytes".into(),
            },
        );
        let err = ingest_entities(&m, minimal_args(1, 0)).unwrap_err();
        assert!(matches!(err, crate::error::Error::Tool { .. }));
    }

    #[test]
    fn default_options_sets_upsert_strict_no_embed() {
        let opts = IngestOptions::default();
        assert_eq!(opts.on_conflict, "update");
        assert!(opts.strict_edges);
        assert!(!opts.embed_missing);
    }

    #[test]
    fn wire_entity_omits_none_fields() {
        let e = WireEntity {
            id: "id-1".into(),
            name: "Foo".into(),
            entity_type: "struct".into(),
            context: "some ctx".into(),
            confidence: None,
            state: None,
            attrs: None,
        };
        let v = serde_json::to_value(&e).unwrap();
        assert!(v.get("confidence").is_none());
        assert!(v.get("state").is_none());
        assert!(v.get("attrs").is_none());
    }

    #[test]
    fn wire_edge_omits_none_metadata() {
        let e = WireEdge {
            src_id: "a".into(),
            dst_id: "b".into(),
            edge_type: "calls".into(),
            weight: 0.5,
            metadata: None,
        };
        let v = serde_json::to_value(&e).unwrap();
        assert!(v.get("metadata").is_none());
    }
}
