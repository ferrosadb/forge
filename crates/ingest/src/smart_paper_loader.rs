//! Paper-specific loader that uses fmem `smart_ingest` for entities.
//!
//! Academic papers are noisy and often duplicated across arXiv, DOI pages,
//! PDFs, and local corpora. For paper entities we route each cleansed entity
//! through fmem's prediction-error-gated `smart_ingest` tool, then remap edges
//! to the entity ids fmem chose before submitting relationships via the normal
//! `ingest_entities` edge path.

use std::collections::HashMap;
use std::time::Instant;

use anyhow::{anyhow, bail, Context, Result};
use forge_fmem_client::transport::Transport;
use forge_fmem_client::{
    ingest_entities, smart_ingest, IngestEntitiesArgs, IngestOptions, SmartIngestArgs, WireEdge,
};

use crate::extractor::{Edge, Entity, IngestReport};
use crate::graph_loader::{FailedRow, LoadReport};

const SMART_INGEST_MAX_CONTENT_CHARS: usize = 8192;

/// Loader for cleansed paper graphs.
pub struct SmartPaperLoader<'a> {
    transport: &'a dyn Transport,
    tenant_id: String,
    session_id: String,
}

impl<'a> SmartPaperLoader<'a> {
    pub fn new<T: Transport>(transport: &'a T, tenant_id: String, session_id: String) -> Self {
        Self::from_dyn(transport, tenant_id, session_id)
    }

    pub fn from_dyn(transport: &'a dyn Transport, tenant_id: String, session_id: String) -> Self {
        Self {
            transport,
            tenant_id,
            session_id,
        }
    }

    /// Smart-ingest all entities, then insert remapped edges.
    pub fn load(&self, report: IngestReport) -> Result<LoadReport> {
        let started = Instant::now();
        let mut out = LoadReport {
            entities_sent: report.entities.len(),
            edges_sent: report.edges.len(),
            ..Default::default()
        };

        let id_map = self.smart_ingest_entities(report.entities, &mut out)?;
        self.ingest_edges(report.edges, &id_map, &mut out)?;

        out.chunks_submitted = usize::from(out.entities_sent > 0) + usize::from(out.edges_sent > 0);
        out.duration_ms = elapsed_ms(started);
        Ok(out)
    }

    fn smart_ingest_entities(
        &self,
        entities: Vec<Entity>,
        out: &mut LoadReport,
    ) -> Result<HashMap<String, String>> {
        let mut id_map = HashMap::with_capacity(entities.len());

        for entity in entities {
            let args = SmartIngestArgs {
                session_id: Some(self.session_id.clone()),
                content: smart_content(&entity),
                entity_type: entity.entity_type.clone(),
                entity_name: Some(entity.name.clone()),
                embedding: None,
                source_fold_id: None,
            };
            let response = smart_ingest(self.transport, args)
                .with_context(|| format!("smart_ingest failed for entity `{}`", entity.name))?;
            let resolved_id = response.resolved_entity_id().ok_or_else(|| {
                anyhow!(
                    "smart_ingest response for `{}` did not include an entity id (action={})",
                    entity.name,
                    response.action
                )
            })?;

            match response.action.as_str() {
                "Created" | "created" => out.entities_inserted += 1,
                "Updated" | "updated" | "Superseded" | "superseded" => out.entities_updated += 1,
                "Skipped" | "skipped" => out.entities_skipped += 1,
                other => {
                    out.entities_failed.push(FailedRow {
                        id: entity.id.clone(),
                        reason: format!("unknown smart_ingest action `{other}`"),
                    });
                    bail!(
                        "unknown smart_ingest action `{other}` for `{}`",
                        entity.name
                    );
                }
            }

            id_map.insert(entity.id, resolved_id);
        }

        Ok(id_map)
    }

    fn ingest_edges(
        &self,
        edges: Vec<Edge>,
        id_map: &HashMap<String, String>,
        out: &mut LoadReport,
    ) -> Result<()> {
        if edges.is_empty() {
            return Ok(());
        }

        let mut remapped = Vec::with_capacity(edges.len());
        for edge in edges {
            let src_id = id_map
                .get(&edge.src_id)
                .cloned()
                .unwrap_or_else(|| edge.src_id.clone());
            let dst_id = id_map
                .get(&edge.dst_id)
                .cloned()
                .unwrap_or_else(|| edge.dst_id.clone());
            remapped.push(WireEdge {
                src_id,
                dst_id,
                edge_type: edge.edge_type,
                weight: edge.weight,
                metadata: edge.metadata,
            });
        }

        let sent = remapped.len();
        let response = ingest_entities(
            self.transport,
            IngestEntitiesArgs {
                tenant_id: self.tenant_id.clone(),
                session_id: self.session_id.clone(),
                entities: Vec::new(),
                edges: remapped,
                options: IngestOptions {
                    on_conflict: "update".into(),
                    strict_edges: true,
                    embed_missing: false,
                },
            },
        )
        .context("ingest_entities failed while inserting smart-ingested paper edges")?;

        out.edges_inserted += response.edges.inserted;
        out.edges_skipped += response.edges.skipped_duplicate;
        out.edges_failed
            .extend(response.edges.failed.iter().map(|f| FailedRow {
                id: f.key(),
                reason: f.reason.clone(),
            }));

        let accounted = response.edges.accounted();
        if accounted != sent {
            bail!(
                "paper edge ingest count mismatch: sent {sent}, accounted {accounted} \
                 (inserted={}, skipped_duplicate={}, failed={})",
                response.edges.inserted,
                response.edges.skipped_duplicate,
                response.edges.failed.len()
            );
        }
        Ok(())
    }
}

fn smart_content(entity: &Entity) -> String {
    let mut content = entity.context.trim().to_string();
    if content.is_empty() {
        content = entity.name.clone();
    }
    truncate_chars(&content, SMART_INGEST_MAX_CONTENT_CHARS)
}

fn truncate_chars(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    text.chars().take(max_chars).collect()
}

fn elapsed_ms(started: Instant) -> u64 {
    started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64
}
