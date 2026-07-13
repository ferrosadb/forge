//! MCP-based code-entity loader for ferrosa-memory.
//!
//! [`GraphLoader`] submits [`Entity`] and [`Edge`] batches to ferrosa-memory via the
//! `ingest_entities` MCP tool and enforces **count reconciliation** on every chunk:
//!
//! ```text
//! response.succeeded + response.skipped + len(response.failed) == rows_sent
//! ```
//!
//! Any mismatch triggers per-row individual retries to localise the dropped rows.
//! If rows remain unaccounted after individual retry the run **hard-fails** with
//! `anyhow::bail!` — no partial `LoadReport` is returned, no silent success.
//! This is the primary mitigation for FMEA F3 (RPN 567).
//!
//! ## Topological ordering (P0-6)
//!
//! Within each chunk entities precede their edges.  For the simple
//! T10 topological placement: an edge is placed in the chunk containing the
//! LATER of its src and dst entities (by batch position).  Edges whose dst
//! is not in the current batch (already resident server-side) are placed
//! with their src.  This guarantees strict_edges always finds both
//! endpoints either in the current chunk or in a prior committed chunk.
//!
//! ## Zero-regression guarantee
//!
//! The existing Python/CQL loader path is the default for all
//! existing callers.  [`GraphLoader`] is ONLY invoked when the caller
//! explicitly opts in via `IngestMode::CodeGraph` (or the `--graph-loader` CLI
//! flag).  No existing behaviour changes.

use std::collections::{HashMap, HashSet};
use std::time::Instant;

use anyhow::{bail, Context, Result};

use forge_fmem_client::transport::Transport;
use forge_fmem_client::{
    ingest_entities, IngestEntitiesArgs, IngestEntitiesResponse, IngestOptions, WireEdge,
    WireEntity,
};

use crate::extractor::{Edge, Entity};

/// Default payload cap: 1 MiB per chunk (server hard-rejects larger payloads).
const DEFAULT_MAX_PAYLOAD_BYTES: usize = 1_000_000;

/// Default row-count cap per chunk (entities + edges).
///
/// Independent of bytes. Each row is a CQL round-trip server-side, so
/// 1 MiB-worth (~500 rows) can trivially exceed any upstream proxy's
/// response window and return HTTP 504 even when the server itself is
/// still working. 100 rows/chunk at ~10 ms/row ≈ 1 s; well within any
/// realistic proxy deadline. Tune via `GraphLoader::with_max_rows_per_chunk`
/// or the `[client] max_rows_per_chunk` config entry.
const DEFAULT_MAX_ROWS_PER_CHUNK: usize = 100;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// A batch of code entities and edges to be submitted to ferrosa-memory.
pub struct GraphBatch {
    pub entities: Vec<Entity>,
    pub edges: Vec<Edge>,
}

/// Per-row failure record.
#[derive(Debug, Clone, serde::Serialize)]
pub struct FailedRow {
    pub id: String,
    pub reason: String,
}

/// Summary of a completed load operation.
///
/// All counts refer to entities only; edges are tracked in separate fields.
#[derive(Debug, Default, serde::Serialize)]
pub struct LoadReport {
    pub entities_sent: usize,
    pub entities_inserted: usize,
    pub entities_updated: usize,
    pub entities_skipped: usize,
    pub entities_failed: Vec<FailedRow>,

    pub edges_sent: usize,
    pub edges_inserted: usize,
    pub edges_skipped: usize,
    pub edges_failed: Vec<FailedRow>,

    pub chunks_submitted: usize,
    pub duration_ms: u64,
}

// ---------------------------------------------------------------------------
// GraphLoader
// ---------------------------------------------------------------------------

/// Loads code-entity batches into ferrosa-memory via the `ingest_entities` MCP tool.
///
/// Enforces count-reconciliation per chunk (FMEA F3) and performs topological
/// co-location of edges with their source entities (P0-6, partial).
///
/// The `T` type parameter allows callers to pass any concrete `Transport`
/// implementation — including mock transports in tests — without boxing.
pub struct GraphLoader<'a> {
    transport: &'a dyn Transport,
    tenant_id: String,
    session_id: String,
    max_payload_bytes: usize,
    max_rows_per_chunk: usize,
}

impl<'a> GraphLoader<'a> {
    /// Create a loader with the default 1 MiB chunk limit.  Accepts any
    /// `&T` where `T: Transport` — the concrete type is erased to
    /// `&dyn Transport` internally so one impl block handles stdio,
    /// HTTP, and mock transports uniformly.
    pub fn new<T: Transport>(transport: &'a T, tenant_id: String, session_id: String) -> Self {
        Self::from_dyn(transport, tenant_id, session_id)
    }

    /// Construct from an already-erased `&dyn Transport`. Used when the
    /// caller holds a trait-object (e.g. behind an enum dispatch over
    /// stdio vs. HTTP transports); the generic `new` can't accept `dyn`
    /// directly because the `&T → &dyn Trait` coercion requires `T: Sized`.
    pub fn from_dyn(transport: &'a dyn Transport, tenant_id: String, session_id: String) -> Self {
        Self {
            transport,
            tenant_id,
            session_id,
            max_payload_bytes: DEFAULT_MAX_PAYLOAD_BYTES,
            max_rows_per_chunk: DEFAULT_MAX_ROWS_PER_CHUNK,
        }
    }

    /// Override the row-count cap per chunk (entities + edges combined).
    /// Smaller chunks are more robust against upstream proxy timeouts
    /// but add round-trip overhead; larger chunks pack more work per
    /// call but can trip HTTP 504 on slow CQL clusters.
    pub fn with_max_rows_per_chunk(mut self, max_rows: usize) -> Self {
        self.max_rows_per_chunk = max_rows.max(1);
        self
    }

    /// Override the payload size cap (used in tests).
    pub fn with_max_payload_bytes(mut self, max_payload_bytes: usize) -> Self {
        self.max_payload_bytes = max_payload_bytes;
        self
    }

    /// Submit `batch` to ferrosa-memory, returning a reconciled [`LoadReport`].
    ///
    /// Hard-fails (returns `Err`) if any chunk has unaccounted rows after
    /// individual retry — see module-level docs.
    pub fn load(&self, batch: GraphBatch) -> Result<LoadReport> {
        let started = Instant::now();
        let mut report = LoadReport::default();

        if batch.entities.is_empty() && batch.edges.is_empty() {
            report.duration_ms = elapsed_ms(started);
            return Ok(report);
        }

        let chunks = self.chunk(batch)?;
        report.chunks_submitted = chunks.len();

        for (chunk_idx, (chunk_entities, chunk_edges)) in chunks.into_iter().enumerate() {
            self.submit_chunk(chunk_idx, chunk_entities, chunk_edges, &mut report)?;
        }

        report.duration_ms = elapsed_ms(started);
        Ok(report)
    }
}

// ---------------------------------------------------------------------------
// Private helpers — chunking
// ---------------------------------------------------------------------------

impl<'a> GraphLoader<'a> {
    /// Split the batch into chunks that fit within `max_payload_bytes`.
    ///
    /// Strategy: greedily pack entities into the current chunk.  Each entity's
    /// edges are placed in the same chunk as the entity (P0-6 co-location).
    /// Edges whose src_id entity lands in a later chunk travel with it; edges
    /// to already-committed dst_ids are safe because strict_edges only checks
    /// the calling batch's entities against the global committed set.
    ///
    /// T10: topological edge placement. Each edge lands in the chunk of the
    /// LATER of its src/dst entities (in batch order).  Edges whose dst is
    /// not in this batch (already resident server-side) are placed with
    /// their src.  The ordering of entities themselves is unchanged — the
    /// caller-supplied order is preserved for deterministic chunk layout.
    fn chunk(&self, batch: GraphBatch) -> Result<Vec<(Vec<WireEntity>, Vec<WireEdge>)>> {
        // Build id → position map for O(1) placement lookups.
        let id_to_pos: HashMap<String, usize> = batch
            .entities
            .iter()
            .enumerate()
            .map(|(i, e)| (e.id.clone(), i))
            .collect();

        // Place each edge at max(pos(src), pos(dst_if_in_batch_else_pos_of_src)).
        // Edges referencing an out-of-batch dst go with their src (assumes dst
        // is already resident server-side — strict_edges enforces that).
        let mut edges_by_placement: HashMap<usize, Vec<WireEdge>> = HashMap::new();
        for edge in batch.edges {
            let wire = entity_to_wire_edge(&edge);
            let src_pos = id_to_pos.get(&edge.src_id).copied();
            let dst_pos = id_to_pos.get(&edge.dst_id).copied();
            let placement = match (src_pos, dst_pos) {
                (Some(s), Some(d)) => s.max(d),
                (Some(s), None) => s,
                (None, Some(d)) => d,
                (None, None) => {
                    // Neither endpoint is in this batch. Send with the last
                    // entity — the server's strict_edges will reject it
                    // loud, preserving the fail-loud contract.
                    batch.entities.len().saturating_sub(1)
                }
            };
            edges_by_placement.entry(placement).or_default().push(wire);
        }

        let mut chunks: Vec<(Vec<WireEntity>, Vec<WireEdge>)> = Vec::new();
        let mut cur_entities: Vec<WireEntity> = Vec::new();
        let mut cur_edges: Vec<WireEdge> = Vec::new();
        let mut cur_bytes: usize = 2; // opening `{}` for the JSON object

        for (pos, entity) in batch.entities.iter().enumerate() {
            let wire_ent = entity_to_wire_entity(entity);
            let placed_edges = edges_by_placement.remove(&pos).unwrap_or_default();

            // Estimate serialised size of this entity + its placed edges.
            let ent_json = serde_json::to_vec(&wire_ent)
                .context("failed to estimate entity serialisation size")?;
            let edges_size: usize = placed_edges
                .iter()
                .map(|e| serde_json::to_vec(e).map(|v| v.len()))
                .collect::<std::result::Result<Vec<_>, _>>()
                .context("failed to estimate edge serialisation size")?
                .into_iter()
                .sum();
            let candidate_bytes = ent_json.len() + edges_size;
            let candidate_rows = 1 + placed_edges.len();

            // Flush current chunk if adding this entity would overflow
            // EITHER the byte cap OR the row-count cap, but only when
            // the chunk is non-empty. Row cap defends against HTTP 504
            // from the server's upstream proxy on slow CQL clusters.
            let cur_rows = cur_entities.len() + cur_edges.len();
            let would_overflow_bytes = cur_bytes + candidate_bytes > self.max_payload_bytes;
            let would_overflow_rows = cur_rows + candidate_rows > self.max_rows_per_chunk;
            if (would_overflow_bytes || would_overflow_rows) && !cur_entities.is_empty() {
                chunks.push((
                    std::mem::take(&mut cur_entities),
                    std::mem::take(&mut cur_edges),
                ));
                cur_bytes = 2;
            }

            cur_bytes += candidate_bytes;
            cur_edges.extend(placed_edges);
            cur_entities.push(wire_ent);
        }

        // Flush remaining entities.
        if !cur_entities.is_empty() {
            chunks.push((cur_entities, cur_edges));
        }

        Ok(chunks)
    }
}

// ---------------------------------------------------------------------------
// Private helpers — submission and reconciliation
// ---------------------------------------------------------------------------

impl<'a> GraphLoader<'a> {
    /// Submit one pre-sized chunk and reconcile the response.
    fn submit_chunk(
        &self,
        chunk_idx: usize,
        entities: Vec<WireEntity>,
        edges: Vec<WireEdge>,
        report: &mut LoadReport,
    ) -> Result<()> {
        let n_entities = entities.len();
        let n_edges = edges.len();
        let payload_bytes = estimate_payload_bytes(&entities, &edges);

        report.entities_sent += n_entities;
        report.edges_sent += n_edges;

        let args = IngestEntitiesArgs {
            tenant_id: self.tenant_id.clone(),
            session_id: self.session_id.clone(),
            entities: entities.clone(),
            edges: edges.clone(),
            options: IngestOptions::default(),
        };

        let resp = ingest_entities(self.transport, args)
            .with_context(|| format!("ingest_entities call failed for chunk {chunk_idx}"))?;

        eprintln!(
            "[graph_loader] chunk={chunk_idx} entities={n_entities} edges={n_edges} \
             inserted={}/{} updated={} skipped={}/{} failed={}/{} bytes={payload_bytes}",
            resp.entities.inserted,
            resp.edges.inserted,
            resp.entities.updated,
            resp.entities.skipped,
            resp.edges.skipped_duplicate,
            resp.entities.failed.len(),
            resp.edges.failed.len(),
        );

        self.reconcile(chunk_idx, &entities, &edges, resp, report)
    }

    /// Assert the count-reconciliation invariant for one chunk response.
    ///
    /// Entity side: `inserted + updated + skipped + len(failed) == entities.len()`.
    /// Edge side:   `inserted + skipped_duplicate + len(failed) == edges.len()`.
    /// On mismatch in either side, perform individual retries to localise.
    fn reconcile(
        &self,
        chunk_idx: usize,
        entities: &[WireEntity],
        edges: &[WireEdge],
        resp: IngestEntitiesResponse,
        report: &mut LoadReport,
    ) -> Result<()> {
        let ent_sent = entities.len();
        let edge_sent = edges.len();
        let ent_accounted = resp.entities.accounted();
        let edge_accounted = resp.edges.accounted();

        // Record the accounted rows in the report before checking.
        record_response(&resp, report);

        let ent_mismatch = ent_accounted != ent_sent;
        let edge_mismatch = edge_accounted != edge_sent;

        if ent_mismatch || edge_mismatch {
            eprintln!(
                "[graph_loader] RECONCILIATION_FAILURE chunk={chunk_idx} \
                 entities sent={ent_sent} accounted={ent_accounted} \
                 edges sent={edge_sent} accounted={edge_accounted}"
            );
            let unaccounted = find_unaccounted_ids(entities, edges, &resp);
            self.retry_individual(chunk_idx, entities, edges, &unaccounted, report)?;
        }

        Ok(())
    }

    /// Retry unaccounted rows individually to localise the failure.
    ///
    /// For each unaccounted id we send a single-row request.  If the server
    /// returns a per-row failure we record it in `report`.  If any row is
    /// STILL unaccounted after individual retry, the run hard-fails.
    fn retry_individual(
        &self,
        chunk_idx: usize,
        entities: &[WireEntity],
        edges: &[WireEdge],
        unaccounted_ids: &HashSet<String>,
        report: &mut LoadReport,
    ) -> Result<()> {
        let mut still_unaccounted: Vec<String> = Vec::new();

        for id in unaccounted_ids {
            if let Some(entity) = entities.iter().find(|e| &e.id == id) {
                let retry_resp = self.retry_single_entity(entity)?;
                if retry_resp.entities.accounted() == 1 {
                    record_response(&retry_resp, report);
                } else {
                    // The server dropped this row even on individual retry.
                    still_unaccounted.push(id.clone());
                }
            } else if let Some(edge) = edges.iter().find(|e| edge_id(e) == *id) {
                let retry_resp = self.retry_single_edge(edge)?;
                if retry_resp.edges.accounted() == 1 {
                    record_response(&retry_resp, report);
                } else {
                    still_unaccounted.push(id.clone());
                }
            }
        }

        if !still_unaccounted.is_empty() {
            bail!(
                "reconciliation failure in chunk {chunk_idx}: {} row(s) unaccounted \
                 after individual retry — ids: {:?}. \
                 This indicates a silent per-row drop in ferrosa-memory (FMEA F3). \
                 Aborting run — no partial LoadReport emitted.",
                still_unaccounted.len(),
                still_unaccounted,
            );
        }

        Ok(())
    }

    /// Retry one entity as a single-row `ingest_entities` call.
    fn retry_single_entity(&self, entity: &WireEntity) -> Result<IngestEntitiesResponse> {
        let args = IngestEntitiesArgs {
            tenant_id: self.tenant_id.clone(),
            session_id: self.session_id.clone(),
            entities: vec![entity.clone()],
            edges: vec![],
            options: IngestOptions::default(),
        };
        ingest_entities(self.transport, args)
            .with_context(|| format!("individual retry failed for entity id={}", entity.id))
    }

    /// Retry one edge as a single-row `ingest_entities` call.
    fn retry_single_edge(&self, edge: &WireEdge) -> Result<IngestEntitiesResponse> {
        let args = IngestEntitiesArgs {
            tenant_id: self.tenant_id.clone(),
            session_id: self.session_id.clone(),
            entities: vec![],
            edges: vec![edge.clone()],
            options: IngestOptions::default(),
        };
        ingest_entities(self.transport, args).with_context(|| {
            format!(
                "individual retry failed for edge src={} dst={}",
                edge.src_id, edge.dst_id
            )
        })
    }
}

// ---------------------------------------------------------------------------
// Wire-format mapping
// ---------------------------------------------------------------------------

/// Map an extractor `Entity` to the `WireEntity` the server expects.
///
/// All optional fields that carry code-graph metadata are packed into the
/// `attrs` JSON object.  Fields the server does not recognise MUST NOT be
/// sent (would trigger `schema_mismatch`).
fn entity_to_wire_entity(e: &Entity) -> WireEntity {
    let mut attrs = serde_json::Map::new();

    if let Some(v) = e.start_byte {
        attrs.insert("start_byte".into(), v.into());
    }
    if let Some(v) = e.end_byte {
        attrs.insert("end_byte".into(), v.into());
    }
    if let Some(v) = e.start_line {
        attrs.insert("start_line".into(), v.into());
    }
    if let Some(v) = e.end_line {
        attrs.insert("end_line".into(), v.into());
    }
    if let Some(v) = &e.visibility {
        attrs.insert("visibility".into(), v.clone().into());
    }
    if let Some(v) = &e.signature {
        attrs.insert("signature".into(), v.clone().into());
    }
    if let Some(v) = &e.doc {
        attrs.insert("doc".into(), v.clone().into());
    }
    if let Some(v) = &e.source_hash {
        attrs.insert("source_hash".into(), v.clone().into());
    }
    if let Some(v) = e.truncated {
        attrs.insert("truncated".into(), v.into());
    }
    if let Some(v) = e.bytes {
        attrs.insert("bytes".into(), v.into());
    }
    if let Some(v) = e.lines {
        attrs.insert("lines".into(), v.into());
    }
    if let Some(v) = e.extractor_schema_version {
        attrs.insert("extractor_schema_version".into(), v.into());
    }
    // source_text is intentionally excluded from attrs — it is a top-level
    // field on `file` entities in the server schema.  sha256 likewise.
    // T-future: once the server schema is confirmed, move these to
    // dedicated top-level WireEntity fields.

    let state = Some("active".to_string());

    WireEntity {
        id: e.id.clone(),
        name: e.name.clone(),
        entity_type: e.entity_type.clone(),
        context: e.context.clone(),
        confidence: None,
        state,
        attrs: if attrs.is_empty() {
            None
        } else {
            Some(serde_json::Value::Object(attrs))
        },
    }
}

/// Map an extractor `Edge` to the `WireEdge` the server expects.
fn entity_to_wire_edge(e: &Edge) -> WireEdge {
    WireEdge {
        src_id: e.src_id.clone(),
        dst_id: e.dst_id.clone(),
        edge_type: e.edge_type.clone(),
        weight: e.weight,
        metadata: e.metadata.clone(),
    }
}

// ---------------------------------------------------------------------------
// Utility functions
// ---------------------------------------------------------------------------

/// A stable string identifier for an edge (composite key).
fn edge_id(e: &WireEdge) -> String {
    format!("{}:{}:{}", e.src_id, e.edge_type, e.dst_id)
}

/// Identify ids not accounted for in a response.
///
/// "Unaccounted" means sent but not reflected in any of inserted/updated/
/// skipped/failed.  Since the server doesn't enumerate inserted ids per-row
/// we can only say "these N ids might have been dropped silently" and fall
/// back to individual retries to localise.
fn find_unaccounted_ids(
    entities: &[WireEntity],
    edges: &[WireEdge],
    resp: &IngestEntitiesResponse,
) -> HashSet<String> {
    let ent_delta = entities.len().saturating_sub(resp.entities.accounted());
    let edge_delta = edges.len().saturating_sub(resp.edges.accounted());

    // ids the server explicitly reported (failed) — skip these when choosing
    // candidates for individual retry.
    let reported_entity_ids: HashSet<String> =
        resp.entities.failed.iter().map(|f| f.key()).collect();
    let reported_edge_ids: HashSet<String> = resp.edges.failed.iter().map(|f| f.key()).collect();

    let entity_candidates = entities
        .iter()
        .map(|e| e.id.clone())
        .filter(|id| !reported_entity_ids.contains(id))
        .take(ent_delta);
    let edge_candidates = edges
        .iter()
        .map(edge_id)
        .filter(|id| !reported_edge_ids.contains(id))
        .take(edge_delta);

    entity_candidates.chain(edge_candidates).collect()
}

/// Accumulate a response's counts into the running report.
///
/// With the server's nested shape we can attribute each count directly to
/// entities vs edges and report inserted/updated/skipped accurately — no
/// proportional splitting needed.
fn record_response(resp: &IngestEntitiesResponse, report: &mut LoadReport) {
    report.entities_inserted += resp.entities.inserted;
    report.entities_updated += resp.entities.updated;
    report.entities_skipped += resp.entities.skipped;
    for f in &resp.entities.failed {
        report.entities_failed.push(crate::graph_loader::FailedRow {
            id: f.key(),
            reason: f.reason.clone(),
        });
    }
    report.edges_inserted += resp.edges.inserted;
    report.edges_skipped += resp.edges.skipped_duplicate;
    for f in &resp.edges.failed {
        report.edges_failed.push(crate::graph_loader::FailedRow {
            id: f.key(),
            reason: f.reason.clone(),
        });
    }
}

/// Estimate the wire-format JSON size of a chunk.
fn estimate_payload_bytes(entities: &[WireEntity], edges: &[WireEdge]) -> usize {
    let ent_bytes: usize = entities
        .iter()
        .map(|e| serde_json::to_vec(e).map(|v| v.len()).unwrap_or(256))
        .sum();
    let edge_bytes: usize = edges
        .iter()
        .map(|e| serde_json::to_vec(e).map(|v| v.len()).unwrap_or(128))
        .sum();
    ent_bytes + edge_bytes + 64 // overhead for request envelope fields
}

fn elapsed_ms(start: Instant) -> u64 {
    start.elapsed().as_millis().try_into().unwrap_or(u64::MAX)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use forge_fmem_client::transport::mock::{MockTransport, ScriptedResponse};
    use serde_json::json;

    const TENANT: &str = "00000000-0000-0000-0000-000000000001";
    const SESSION: &str = "00000000-0000-0000-0000-000000000002";

    fn make_entity(id: &str) -> Entity {
        Entity {
            id: id.to_string(),
            name: id.to_string(),
            entity_type: "function".to_string(),
            context: format!("fn {id}()"),
            ..Default::default()
        }
    }

    fn make_edge(src: &str, dst: &str) -> Edge {
        Edge {
            src_id: src.to_string(),
            dst_id: dst.to_string(),
            edge_type: "calls".to_string(),
            weight: 1.0,
            ..Default::default()
        }
    }

    fn loader(transport: &MockTransport) -> GraphLoader<'_> {
        GraphLoader::new(transport, TENANT.into(), SESSION.into())
    }

    // -----------------------------------------------------------------------
    // Test 1: empty batch is ok
    // -----------------------------------------------------------------------
    #[test]
    fn empty_batch_is_ok() {
        let m = MockTransport::panicking(); // must not call the wire at all
        let result = loader(&m).load(GraphBatch {
            entities: vec![],
            edges: vec![],
        });
        let report = result.unwrap();
        assert_eq!(report.entities_sent, 0);
        assert_eq!(report.edges_sent, 0);
        assert_eq!(report.chunks_submitted, 0);
        assert!(report.entities_failed.is_empty());
        assert!(report.edges_failed.is_empty());
    }

    // -----------------------------------------------------------------------
    // Test 2: reconciliation detects silent drop — hard-fails with "reconciliation"
    // -----------------------------------------------------------------------
    #[test]
    fn reconciliation_detects_silent_drop() {
        // 3 entities sent; server claims only 2 accounted for (silent drop of 1).
        let m = MockTransport::new();
        // First call: batch of 3 entities; server reports only 2 inserted (drops 1).
        m.expect_call(
            "tools/call",
            ScriptedResponse::Ok(json!({
                "entities": { "inserted": 2, "updated": 0, "skipped": 0, "failed": [] },
                "edges": { "inserted": 0, "skipped_duplicate": 0, "failed": [] },
                "embeddings": { "computed": 0, "received": 0, "failed": [] },
                "schema_version": "2026-03-01",
                "duration_ms": 5,
            })),
        );
        // Individual retry — server again drops it (not accounted).
        m.expect_call(
            "tools/call",
            ScriptedResponse::Ok(json!({
                "entities": { "inserted": 0, "updated": 0, "skipped": 0, "failed": [] },
                "edges": { "inserted": 0, "skipped_duplicate": 0, "failed": [] },
                "embeddings": { "computed": 0, "received": 0, "failed": [] },
                "schema_version": "2026-03-01",
                "duration_ms": 3,
            })),
        );

        let result = loader(&m).load(GraphBatch {
            entities: vec![make_entity("e1"), make_entity("e2"), make_entity("e3")],
            edges: vec![],
        });

        assert!(result.is_err());
        let msg = format!("{:?}", result.unwrap_err());
        assert!(
            msg.to_lowercase().contains("reconciliation"),
            "error message must contain 'reconciliation'; got: {msg}"
        );
        m.assert_done();
    }

    // -----------------------------------------------------------------------
    // Test 3: individual retry fills report on explicit per-row error
    // -----------------------------------------------------------------------
    #[test]
    fn individual_retry_fills_report() {
        // 2 entities sent; server drops 1 silently in the batch response.
        let m = MockTransport::new();

        // First call: batch of 2, server inserts 1, drops e2 silently.
        m.expect_call(
            "tools/call",
            ScriptedResponse::Ok(json!({
                "entities": { "inserted": 1, "updated": 0, "skipped": 0, "failed": [] },
                "edges": { "inserted": 0, "skipped_duplicate": 0, "failed": [] },
                "embeddings": { "computed": 0, "received": 0, "failed": [] },
                "schema_version": "2026-03-01",
                "duration_ms": 5,
            })),
        );

        // Individual retry for e2: server now returns an explicit failure.
        m.expect_call(
            "tools/call",
            ScriptedResponse::Ok(json!({
                "entities": {
                    "inserted": 0, "updated": 0, "skipped": 0,
                    "failed": [{ "id": "e2", "reason": "invalid_value: name too long" }]
                },
                "edges": { "inserted": 0, "skipped_duplicate": 0, "failed": [] },
                "embeddings": { "computed": 0, "received": 0, "failed": [] },
                "schema_version": "2026-03-01",
                "duration_ms": 2,
            })),
        );

        let result = loader(&m).load(GraphBatch {
            entities: vec![make_entity("e1"), make_entity("e2")],
            edges: vec![],
        });

        let report = result.unwrap();
        assert_eq!(report.entities_failed.len(), 1);
        assert_eq!(report.entities_failed[0].id, "e2");
        assert!(report.entities_failed[0].reason.contains("invalid_value"));
        m.assert_done();
    }

    // -----------------------------------------------------------------------
    // Test 4: payload chunking splits at limit
    // -----------------------------------------------------------------------
    #[test]
    fn payload_chunking_splits_at_limit() {
        // 20 entities. Set max_payload_bytes = 1 (effectively zero) so each
        // entity is forced into its own chunk — exactly 20 chunks, each of
        // size 1.  This makes the mock scripting deterministic regardless of
        // actual serialised entity size.
        let entities: Vec<Entity> = (0..20)
            .map(|i| make_entity(&format!("ent-{i:03}")))
            .collect();

        let m = MockTransport::new();
        // 20 chunks, each containing exactly 1 entity → entities.inserted: 1 each.
        for _ in 0..20 {
            m.expect_call(
                "tools/call",
                ScriptedResponse::Ok(json!({
                    "entities": { "inserted": 1, "updated": 0, "skipped": 0, "failed": [] },
                    "edges": { "inserted": 0, "skipped_duplicate": 0, "failed": [] },
                    "embeddings": { "computed": 0, "received": 0, "failed": [] },
                    "schema_version": "2026-03-01",
                    "duration_ms": 1,
                })),
            );
        }

        let report = GraphLoader::new(&m, TENANT.into(), SESSION.into())
            .with_max_payload_bytes(1) // forces one entity per chunk
            .load(GraphBatch {
                entities,
                edges: vec![],
            })
            .unwrap();

        assert!(
            report.chunks_submitted >= 2,
            "expected at least 2 chunks, got {}",
            report.chunks_submitted
        );
        assert_eq!(
            report.chunks_submitted, 20,
            "with cap=1, each entity gets its own chunk"
        );
        assert_eq!(report.entities_sent, 20);
        assert_eq!(report.entities_inserted, 20);
        m.assert_done();
    }

    // -----------------------------------------------------------------------
    // Test 5: edges ship with their entities
    // -----------------------------------------------------------------------
    #[test]
    fn edges_ship_with_later_endpoint_t10() {
        // T10: edge E1→E2 must land in the SAME chunk as its LATER endpoint
        // (E2), not with E1, so that strict_edges on the server side always
        // finds both endpoints persistent.  E1 is already committed in chunk 1
        // before the edge references it in chunk 2.
        //
        // Entity order in the batch: E1, E2. Edge: E1→E2.
        // Tiny payload cap forces one entity per chunk.
        // Expected: chunk 1 = E1 only (no edges), chunk 2 = E2 + edge.

        let m = MockTransport::new();

        // Chunk 1: E1 only, NO edges (edge waits for E2 to exist).
        m.expect_call_with(
            "tools/call",
            |p| {
                let args = &p["arguments"];
                let entities = args["entities"].as_array().unwrap();
                let edges = args["edges"].as_array().unwrap();
                entities.len() == 1 && entities[0]["id"] == "E1" && edges.is_empty()
            },
            ScriptedResponse::Ok(json!({
                "entities": { "inserted": 1, "updated": 0, "skipped": 0, "failed": [] },
                "edges": { "inserted": 0, "skipped_duplicate": 0, "failed": [] },
                "embeddings": { "computed": 0, "received": 0, "failed": [] },
                "schema_version": "2026-03-01", "duration_ms": 1,
            })),
        );

        // Chunk 2: E2 + the E1→E2 edge.
        m.expect_call_with(
            "tools/call",
            |p| {
                let args = &p["arguments"];
                let entities = args["entities"].as_array().unwrap();
                let edges = args["edges"].as_array().unwrap();
                entities.len() == 1
                    && entities[0]["id"] == "E2"
                    && edges.len() == 1
                    && edges[0]["src_id"] == "E1"
                    && edges[0]["dst_id"] == "E2"
            },
            ScriptedResponse::Ok(json!({
                "entities": { "inserted": 1, "updated": 0, "skipped": 0, "failed": [] },
                "edges": { "inserted": 1, "skipped_duplicate": 0, "failed": [] },
                "embeddings": { "computed": 0, "received": 0, "failed": [] },
                "schema_version": "2026-03-01", "duration_ms": 2,
            })),
        );

        let report = GraphLoader::new(&m, TENANT.into(), SESSION.into())
            .with_max_payload_bytes(1)
            .load(GraphBatch {
                entities: vec![make_entity("E1"), make_entity("E2")],
                edges: vec![make_edge("E1", "E2")],
            })
            .unwrap();

        assert_eq!(report.chunks_submitted, 2);
        assert_eq!(report.entities_sent, 2);
        assert_eq!(report.edges_sent, 1);
        m.assert_done();
    }

    #[test]
    fn t10_edge_with_out_of_batch_dst_stays_with_src() {
        // Edge E1→X where X is not in this batch (presumed already server-
        // resident). Edge should ship with E1 in chunk 1.
        let m = MockTransport::new();

        m.expect_call_with(
            "tools/call",
            |p| {
                let args = &p["arguments"];
                let entities = args["entities"].as_array().unwrap();
                let edges = args["edges"].as_array().unwrap();
                entities.len() == 1
                    && entities[0]["id"] == "E1"
                    && edges.len() == 1
                    && edges[0]["dst_id"] == "EXT_X"
            },
            ScriptedResponse::Ok(json!({
                "entities": { "inserted": 1, "updated": 0, "skipped": 0, "failed": [] },
                "edges": { "inserted": 1, "skipped_duplicate": 0, "failed": [] },
                "embeddings": { "computed": 0, "received": 0, "failed": [] },
                "schema_version": "2026-03-01", "duration_ms": 1,
            })),
        );

        let report = GraphLoader::new(&m, TENANT.into(), SESSION.into())
            .load(GraphBatch {
                entities: vec![make_entity("E1")],
                edges: vec![make_edge("E1", "EXT_X")],
            })
            .unwrap();

        assert_eq!(report.chunks_submitted, 1);
        m.assert_done();
    }
}
