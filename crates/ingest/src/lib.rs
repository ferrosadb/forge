//! Codebase ingestion for ferrosa-memory knowledge graphs.
//!
//! Extracts crates/packages, modules, and inter-module relationships
//! from a codebase and outputs structured entities + typed edges
//! ready for bulk insertion into ferrosa-memory.
//!
//! ## Ingest path
//!
//! All writes go through [`graph_loader`] — the MCP `ingest_entities`
//! tool with strict count-reconciliation (FMEA F3). The 0.6.x Python/CQL
//! `loader` module was removed in 0.8.0; see CHANGELOG.

#![cfg_attr(
    test,
    allow(
        clippy::assertions_on_constants,
        clippy::field_reassign_with_default,
        clippy::too_many_arguments,
        clippy::type_complexity
    )
)]

pub mod cache;
pub mod corpus;
pub mod descriptions;
pub mod extractor;
pub mod graph_loader;
pub mod ignore_policy;
pub mod lsp;
pub mod paper;
pub mod pdf;
pub mod position;
pub mod refresh;
pub mod sanitize;
pub mod skill_ingest;
pub mod smart_paper_loader;
pub mod source_buffer;
pub mod url;
