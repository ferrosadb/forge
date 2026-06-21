//! Tag taxonomy pre-pass for skill ingest.
//!
//! Walks the top level of the skill root, parses optional
//! `tag-hierarchy.yaml`, and returns a validated plan:
//!
//! - the list of tag names (normalized via the same rule as fmem),
//! - the list of `PARENT_TAG` edges to create (child → parent),
//! - a sorted set of every tag the pipeline will ever mention (top-level
//!   dirs ∪ hierarchy nodes ∪ every parsed skill's category + tags).
//!
//! The plan also enforces four validations before any fmem round-trip
//! (FMEA F25–F29):
//!
//! 1. [`detect_cycles`]   — PARENT_TAG edges form a DAG.
//! 2. [`detect_orphans`]  — every hierarchy node is a known tag.
//! 3. [`collect_all_tags`] — preflight union of every tag the pipeline
//!    will need, so Phase B never lazily creates a tag.
//! 4. [`check_name_collisions`] — no skill shares a name with any tag.

pub mod hierarchy;
pub mod plan;

pub use hierarchy::{parse_hierarchy, HierarchyError, MAX_HIERARCHY_BYTES, MAX_HIERARCHY_EDGES};
pub use plan::{
    build_plan, check_name_collisions, collect_all_tags, detect_cycles, detect_orphans,
    walk_top_level, PlanError, TagEdge, TaxonomyPlan,
};
