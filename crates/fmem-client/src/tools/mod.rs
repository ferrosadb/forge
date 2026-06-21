//! Typed wrappers over fmem MCP tools.
//!
//! - [`initialize`] — MCP handshake + protocol version assert (P3).
//! - [`ingest_skill`] — ingest a skill entity (P5). fmem Sprint 2.
//! - [`ensure_parent_tag`] — idempotent PARENT_TAG edge creation by
//!   name (P5a). fmem `skill-ingest-support`.
//! - [`verify_skill`] — read a skill's graph neighborhood for the
//!   verification phase (P5b). fmem `skill-ingest-support`.

pub mod batch_delete_entities;
pub mod batch_update_entities;
pub mod count_entities_by_type;
pub mod ensure_parent_tag;
pub mod handshake;
pub mod ingest_entities;
pub mod ingest_skill;
pub mod smart_ingest;
pub mod verify_skill;

pub use batch_delete_entities::{
    batch_delete_entities, BatchDeleteEntitiesArgs, BatchDeleteEntitiesResponse, DeleteResult,
    WireDeleteTarget, BATCH_DELETE_MAX,
};
pub use batch_update_entities::{
    batch_update_entities, BatchUpdateEntitiesArgs, BatchUpdateEntitiesResponse, PatchResult,
    WirePatchEntity, BATCH_UPDATE_MAX,
};
pub use count_entities_by_type::{
    count_entities_by_type, CountEntitiesByTypeArgs, CountEntitiesByTypeResponse,
};
pub use ensure_parent_tag::{
    ensure_parent_tag, EnsureParentTagAction, EnsureParentTagArgs, EnsureParentTagResponse,
};
pub use handshake::{initialize, ExpectedProtocolVersion, InitializeInfo, MCP_PROTOCOL_VERSION};
pub use ingest_entities::{
    ingest_entities, EdgeStats, EmbeddingStats, EntityStats, IngestEntitiesArgs,
    IngestEntitiesResponse, IngestOptions, WireEdge, WireEntity, WireFailedRow,
};
pub use ingest_skill::{
    ingest_skill, IngestSkillAction, IngestSkillArgs, IngestSkillResponse, Step,
};
pub use smart_ingest::{smart_ingest, SmartIngestArgs, SmartIngestResponse};
pub use verify_skill::{verify_skill, VerifySkillArgs, VerifySkillResponse};
