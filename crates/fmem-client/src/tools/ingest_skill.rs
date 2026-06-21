//! Typed wrapper over fmem's `ingest_skill` MCP tool.
//!
//! fmem ships `ingest_skill` as of Sprint 2a
//! (`ferrosa-memory-core/src/skill.rs`, `ferrosa-memory-core/src/dispatch.rs`).
//! Request/response shapes mirror fmem's `IngestSkillParams` /
//! `SkillIngestAction`.
//!
//! Behavior relevant to forge:
//! - fmem auto-creates tag entities for `category` + each entry in
//!   `tags`, wired up via TAGGED_AS edges. forge does *not* call
//!   `smart_ingest` separately for tags.
//! - Prerequisites that don't yet exist are silently skipped on the
//!   server side, but the names are **returned** in
//!   `action.missing_prerequisites` so callers can re-ingest precisely
//!   (fmem commit `aae5771`; replaces the old "scan-for-deferred"
//!   heuristic).
//! - `content_hash` is opaque to fmem; passing the same value as the
//!   stored entity is a no-op (returns `action: Skipped`).
//! - The server generates the skill's `version` (YYYYMMDDNN); forge
//!   must not send one.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::Error;
use crate::transport::Transport;

/// One step of a skill's instructions.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Step {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase: Option<String>,
    pub instruction: String,
}

/// Arguments passed to fmem's `ingest_skill`.
#[derive(Debug, Clone, Serialize)]
pub struct IngestSkillArgs {
    pub name: String,
    pub category: String,
    pub description: String,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub trigger_keywords: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub tags: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub prerequisites: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub steps: Vec<Step>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub output_artifacts: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completion_criteria: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_hash: Option<String>,
    /// Session to record as `ingested_by_session`. fmem accepts a UUID
    /// string or the literal `"default"` (see
    /// `ferrosa-memory-core/src/dispatch.rs`'s tool description).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
}

/// Outcome of an `ingest_skill` call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IngestSkillAction {
    Created {
        entity_id: Uuid,
        version: String,
        /// Prerequisite names the caller declared that didn't resolve
        /// to a REQUIRES edge (target skill not yet ingested, cycle
        /// rejected, etc.). Empty when every prereq resolved.
        missing_prerequisites: Vec<String>,
    },
    Updated {
        entity_id: Uuid,
        version: String,
        prior_version: Option<String>,
        missing_prerequisites: Vec<String>,
    },
    Skipped {
        entity_id: Uuid,
        version: String,
        reason: String,
    },
}

impl IngestSkillAction {
    pub fn entity_id(&self) -> Uuid {
        match self {
            Self::Created { entity_id, .. }
            | Self::Updated { entity_id, .. }
            | Self::Skipped { entity_id, .. } => *entity_id,
        }
    }

    pub fn version(&self) -> &str {
        match self {
            Self::Created { version, .. }
            | Self::Updated { version, .. }
            | Self::Skipped { version, .. } => version,
        }
    }

    /// Prerequisite names the server couldn't resolve. Empty for
    /// `Skipped` (the skill was unchanged; no edges were touched).
    pub fn missing_prerequisites(&self) -> &[String] {
        match self {
            Self::Created {
                missing_prerequisites,
                ..
            }
            | Self::Updated {
                missing_prerequisites,
                ..
            } => missing_prerequisites,
            Self::Skipped { .. } => &[],
        }
    }
}

/// Raw response shape emitted by fmem — the tagged `action` enum
/// matches `SkillIngestAction` on the server.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
enum RawResponse {
    Created {
        entity_id: Uuid,
        version: String,
        #[serde(default)]
        missing_prerequisites: Vec<String>,
    },
    Updated {
        entity_id: Uuid,
        version: String,
        #[serde(default)]
        prior_version: Option<String>,
        #[serde(default)]
        missing_prerequisites: Vec<String>,
    },
    Skipped {
        entity_id: Uuid,
        version: String,
        reason: String,
    },
}

impl From<RawResponse> for IngestSkillAction {
    fn from(r: RawResponse) -> Self {
        match r {
            RawResponse::Created {
                entity_id,
                version,
                missing_prerequisites,
            } => Self::Created {
                entity_id,
                version,
                missing_prerequisites,
            },
            RawResponse::Updated {
                entity_id,
                version,
                prior_version,
                missing_prerequisites,
            } => Self::Updated {
                entity_id,
                version,
                prior_version,
                missing_prerequisites,
            },
            RawResponse::Skipped {
                entity_id,
                version,
                reason,
            } => Self::Skipped {
                entity_id,
                version,
                reason,
            },
        }
    }
}

/// Full typed response returned to the caller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IngestSkillResponse {
    pub action: IngestSkillAction,
}

/// Call fmem's `ingest_skill` MCP tool.
pub fn ingest_skill<T: Transport>(
    transport: &T,
    args: IngestSkillArgs,
) -> Result<IngestSkillResponse, Error> {
    // fmem accepts session as a free-form string; omit when absent.
    let args_value = serde_json::to_value(&args)
        .map_err(|e| Error::Schema(format!("failed to serialize IngestSkillArgs: {e}")))?;
    let raw = transport.call_tool("ingest_skill", args_value)?;
    parse_response(raw)
}

fn parse_response(raw: serde_json::Value) -> Result<IngestSkillResponse, Error> {
    let typed: RawResponse = serde_json::from_value(raw.clone()).map_err(|e| {
        // fmem may have returned a flat JSON-RPC error (not an MCP tool
        // error) that escaped the envelope unwrap — surface it as a
        // Schema error rather than panicking.
        Error::Schema(format!(
            "ingest_skill response did not match expected shape: {e}; raw={raw}"
        ))
    })?;
    Ok(IngestSkillResponse {
        action: typed.into(),
    })
}

// ---------------------------------------------------------------------------
// Helper: let forge-ingest hand us a fully-parsed Skill and a content
// hash and get back IngestSkillArgs without copying field-by-field.
// This lives in forge-ingest, not here — we only expose the strict
// types so forge-ingest can construct them.

/// Convenience: produce args for a skill that lacks supplementary tags,
/// prereqs, etc. Tests use this; real callers populate every field.
#[cfg(test)]
fn minimal_args(name: &str) -> IngestSkillArgs {
    IngestSkillArgs {
        name: name.to_string(),
        category: "task-level".to_string(),
        description: format!("{name} skill"),
        trigger_keywords: Vec::new(),
        tags: Vec::new(),
        prerequisites: Vec::new(),
        steps: Vec::new(),
        output_artifacts: Vec::new(),
        completion_criteria: None,
        content_hash: Some(format!("sha256:{}", "0".repeat(64))),
        session_id: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::mock::{MockTransport, ScriptedResponse};
    use serde_json::json;

    fn mock_with_response(body: serde_json::Value) -> MockTransport {
        let m = MockTransport::new();
        m.expect_call("tools/call", ScriptedResponse::Ok(body));
        m
    }

    #[test]
    fn parses_created() {
        let eid = Uuid::new_v4();
        let m = mock_with_response(json!({
            "action": "created",
            "entity_id": eid.to_string(),
            "version": "2026041601",
        }));
        let resp = ingest_skill(&m, minimal_args("tdd")).unwrap();
        match resp.action {
            IngestSkillAction::Created {
                entity_id,
                version,
                missing_prerequisites,
            } => {
                assert_eq!(entity_id, eid);
                assert_eq!(version, "2026041601");
                // Field absent on the wire (fmem omits empty) defaults to [].
                assert!(missing_prerequisites.is_empty());
            }
            other => panic!("expected Created, got {other:?}"),
        }
    }

    #[test]
    fn parses_created_with_missing_prereqs() {
        // fmem commit aae5771 adds missing_prerequisites on Created / Updated.
        let eid = Uuid::new_v4();
        let m = mock_with_response(json!({
            "action": "created",
            "entity_id": eid.to_string(),
            "version": "2026041601",
            "missing_prerequisites": ["unit-testing", "refactor"],
        }));
        let resp = ingest_skill(&m, minimal_args("tdd")).unwrap();
        assert_eq!(
            resp.action.missing_prerequisites(),
            &["unit-testing".to_string(), "refactor".to_string()]
        );
    }

    #[test]
    fn parses_updated_with_prior_version() {
        let eid = Uuid::new_v4();
        let m = mock_with_response(json!({
            "action": "updated",
            "entity_id": eid.to_string(),
            "version": "2026041602",
            "prior_version": "2026041601",
        }));
        let resp = ingest_skill(&m, minimal_args("tdd")).unwrap();
        match resp.action {
            IngestSkillAction::Updated {
                prior_version,
                version,
                ..
            } => {
                assert_eq!(prior_version.as_deref(), Some("2026041601"));
                assert_eq!(version, "2026041602");
            }
            other => panic!("expected Updated, got {other:?}"),
        }
    }

    #[test]
    fn skipped_has_empty_missing_prereqs() {
        // Skipped means nothing was touched, so prereq edges weren't
        // attempted — accessor returns empty slice.
        let eid = Uuid::new_v4();
        let m = mock_with_response(json!({
            "action": "skipped",
            "entity_id": eid.to_string(),
            "version": "2026041601",
            "reason": "content_hash unchanged",
        }));
        let resp = ingest_skill(&m, minimal_args("tdd")).unwrap();
        assert!(resp.action.missing_prerequisites().is_empty());
    }

    #[test]
    fn parses_skipped() {
        let eid = Uuid::new_v4();
        let m = mock_with_response(json!({
            "action": "skipped",
            "entity_id": eid.to_string(),
            "version": "2026041601",
            "reason": "content_hash unchanged",
        }));
        let resp = ingest_skill(&m, minimal_args("tdd")).unwrap();
        assert!(matches!(resp.action, IngestSkillAction::Skipped { .. }));
        assert_eq!(resp.action.version(), "2026041601");
    }

    #[test]
    fn unknown_action_is_schema_error() {
        let m = mock_with_response(json!({
            "action": "teleported",
            "entity_id": Uuid::new_v4().to_string(),
        }));
        let err = ingest_skill(&m, minimal_args("tdd")).unwrap_err();
        assert!(matches!(err, Error::Schema(_)));
    }

    #[test]
    fn tool_error_from_fmem_passes_through() {
        let m = MockTransport::new();
        m.expect_call(
            "tools/call",
            ScriptedResponse::ToolError {
                code: -32602,
                message: "invalid skill name".into(),
            },
        );
        let err = ingest_skill(&m, minimal_args("tdd")).unwrap_err();
        match err {
            Error::Tool { code, message } => {
                assert_eq!(code, -32602);
                assert!(message.contains("invalid"));
            }
            other => panic!("expected Tool, got {other:?}"),
        }
    }

    #[test]
    fn serializes_expected_args() {
        let m = MockTransport::new();
        m.expect_call_with(
            "tools/call",
            |p| {
                p["name"] == "ingest_skill"
                    && p["arguments"]["name"] == "tdd"
                    && p["arguments"]["category"] == "task-level"
                    && p["arguments"]["content_hash"].as_str().unwrap().starts_with("sha256:")
                    // omitted empty fields should not appear
                    && p["arguments"].get("tags").is_none()
                    && p["arguments"].get("prerequisites").is_none()
            },
            ScriptedResponse::Ok(json!({
                "action": "created",
                "entity_id": Uuid::new_v4().to_string(),
                "version": "2026041601",
            })),
        );
        ingest_skill(&m, minimal_args("tdd")).unwrap();
        m.assert_done();
    }

    #[test]
    fn serializes_optional_fields_when_present() {
        let m = MockTransport::new();
        m.expect_call_with(
            "tools/call",
            |p| {
                p["arguments"]["tags"].as_array().unwrap().len() == 2
                    && p["arguments"]["prerequisites"][0] == "unit-testing"
                    && p["arguments"]["completion_criteria"].as_str().is_some()
            },
            ScriptedResponse::Ok(json!({
                "action": "created",
                "entity_id": Uuid::new_v4().to_string(),
                "version": "2026041601",
            })),
        );
        let args = IngestSkillArgs {
            name: "tdd".into(),
            category: "task-level".into(),
            description: "do tdd".into(),
            trigger_keywords: vec!["test".into()],
            tags: vec!["testing".into(), "quality".into()],
            prerequisites: vec!["unit-testing".into()],
            steps: vec![Step {
                phase: Some("Red".into()),
                instruction: "Write failing test".into(),
            }],
            output_artifacts: Vec::new(),
            completion_criteria: Some("all tests pass after refactor".into()),
            content_hash: Some(format!("sha256:{}", "a".repeat(64))),
            session_id: Some("default".into()),
        };
        ingest_skill(&m, args).unwrap();
        m.assert_done();
    }

    #[test]
    fn step_phase_is_optional_in_wire_format() {
        let s = Step {
            phase: None,
            instruction: "do the thing".into(),
        };
        let v = serde_json::to_value(&s).unwrap();
        assert!(v.get("phase").is_none());
        assert_eq!(v["instruction"], "do the thing");
    }
}
