//! MCP client for ferrosa-memory.
//!
//! Typed wrappers over the JSON-RPC tools forge needs to administer
//! ferrosa-memory state. Today: skill catalog seeding (`ingest_skill`,
//! `ensure_parent_tag`, `verify_skill`). Future admin commands reuse
//! the same `Transport` trait.

pub mod error;
pub mod tools;
pub mod transport;

pub use error::Error;
pub use tools::{
    batch_delete_entities, batch_update_entities, count_entities_by_type, ensure_parent_tag,
    ingest_entities, ingest_skill, initialize, smart_ingest, verify_skill, BatchDeleteEntitiesArgs,
    BatchDeleteEntitiesResponse, BatchUpdateEntitiesArgs, BatchUpdateEntitiesResponse,
    CountEntitiesByTypeArgs, CountEntitiesByTypeResponse, DeleteResult, EdgeStats, EmbeddingStats,
    EnsureParentTagAction, EnsureParentTagArgs, EnsureParentTagResponse, EntityStats,
    ExpectedProtocolVersion, IngestEntitiesArgs, IngestEntitiesResponse, IngestOptions,
    IngestSkillAction, IngestSkillArgs, IngestSkillResponse, InitializeInfo, PatchResult,
    SmartIngestArgs, SmartIngestResponse, Step, VerifySkillArgs, VerifySkillResponse,
    WireDeleteTarget, WireEdge, WireEntity, WireFailedRow, WirePatchEntity, BATCH_DELETE_MAX,
    BATCH_UPDATE_MAX, MCP_PROTOCOL_VERSION,
};
pub use transport::{
    HttpAuth, HttpConfig, HttpTransport, MockTransport, StdioTransport, Transport,
};
