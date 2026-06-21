//! Skill catalog ingest pipeline.
//!
//! Walks a `research/skills/**/SKILL.md` tree, parses each skill's
//! YAML frontmatter + markdown body, computes a deterministic content
//! hash, and (in later sprints) ships the result to ferrosa-memory via
//! `frg fmem-skill-ingest`. See `specs/fmem-skill-ingest/`
//! for the full blueprint.

pub mod build_args;
pub mod collision;
pub mod hash;
pub mod parse;
pub mod run;
pub mod secret_check;
pub mod supplementary;
pub mod taxonomy;
pub mod walk;

pub use run::{run, RunConfig, RunError, Summary, VerifyFailure, VerifyFailureReason};
