//! Description extraction for ingest entities.
//!
//! Runs as an optional Pass 2 over the entities produced by the base
//! extractor. Calls an LLM (local by default) to synthesize a 1–2 sentence
//! description per public entity, then emits structured facts suitable
//! for writing to ferrosa-memory via `write_temporal_fact`.
//!
//! # Scope (v1)
//!
//! This module ships with three provider implementations: `Ollama` (local,
//! default), `Skip` (no-op), and `Mock` (tests only). OpenAI and Anthropic
//! are intentionally deferred — the `DescriptionProvider` trait is
//! designed so adding them is a matter of one new file.
//!
//! # Fail-loud policy
//!
//! Every degraded path must surface to the caller. The module NEVER
//! silently falls back from one provider to another; a provider that
//! cannot probe successfully either prompts the user (TTY) or exits
//! non-zero (non-interactive). See `probe.rs`.

pub mod config;
pub mod orchestrator;
pub mod probe;
pub mod project_root;
pub mod prompt;
pub mod provider;
pub mod providers;
pub mod redactor;
pub mod report;
pub mod schema;

pub use config::DescriptionsConfig;
pub use orchestrator::{extract_descriptions, ExtractionInputs};
pub use provider::{DescriptionProvider, ProbeInfo, ProviderError, Snippet};
pub use report::Report;
pub use schema::{Description, Provenance};
