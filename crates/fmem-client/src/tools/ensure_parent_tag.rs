//! Typed wrapper over fmem's `ensure_parent_tag` MCP tool.
//!
//! fmem ships this in
//! `ferrosa-memory-core/src/skill.rs::ensure_parent_tag`. Forge calls
//! it once per PARENT_TAG edge declared in `tag-hierarchy.yaml`
//! (blueprint Phase A).
//!
//! Semantics (see fmem's docstring):
//! - Both tag names are normalized server-side (lowercase + dash).
//! - Creates the child/parent entities if absent.
//! - Idempotent: second call with the same edge returns
//!   `action: Skipped`.
//! - Self-loops and cycles are rejected (fmem uses the Sprint 2d DAG
//!   check for the cycle side).

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::Error;
use crate::transport::Transport;

/// Arguments for `ensure_parent_tag`.
#[derive(Debug, Clone, Serialize)]
pub struct EnsureParentTagArgs {
    pub child_tag: String,
    pub parent_tag: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
}

/// Outcome returned by fmem.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnsureParentTagAction {
    Created,
    Skipped,
}

/// Full response — action plus the resolved tag UUIDs (useful for
/// post-run auditing, even though forge doesn't need them to make
/// further calls).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnsureParentTagResponse {
    pub action: EnsureParentTagAction,
    pub child_id: Uuid,
    pub parent_id: Uuid,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
enum RawResponse {
    Created { child_id: Uuid, parent_id: Uuid },
    Skipped { child_id: Uuid, parent_id: Uuid },
}

impl From<RawResponse> for EnsureParentTagResponse {
    fn from(r: RawResponse) -> Self {
        match r {
            RawResponse::Created {
                child_id,
                parent_id,
            } => Self {
                action: EnsureParentTagAction::Created,
                child_id,
                parent_id,
            },
            RawResponse::Skipped {
                child_id,
                parent_id,
            } => Self {
                action: EnsureParentTagAction::Skipped,
                child_id,
                parent_id,
            },
        }
    }
}

/// Call fmem's `ensure_parent_tag`.
pub fn ensure_parent_tag<T: Transport>(
    transport: &T,
    args: EnsureParentTagArgs,
) -> Result<EnsureParentTagResponse, Error> {
    let args_value = serde_json::to_value(&args)
        .map_err(|e| Error::Schema(format!("failed to serialize EnsureParentTagArgs: {e}")))?;
    let raw = transport.call_tool("ensure_parent_tag", args_value)?;
    let typed: RawResponse = serde_json::from_value(raw.clone()).map_err(|e| {
        Error::Schema(format!(
            "ensure_parent_tag response did not match expected shape: {e}; raw={raw}"
        ))
    })?;
    Ok(typed.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::mock::{MockTransport, ScriptedResponse};
    use serde_json::json;

    fn args() -> EnsureParentTagArgs {
        EnsureParentTagArgs {
            child_tag: "tdd".into(),
            parent_tag: "testing".into(),
            session_id: None,
        }
    }

    #[test]
    fn parses_created() {
        let cid = Uuid::new_v4();
        let pid = Uuid::new_v4();
        let m = MockTransport::new();
        m.expect_call(
            "tools/call",
            ScriptedResponse::Ok(json!({
                "action": "created",
                "child_id": cid.to_string(),
                "parent_id": pid.to_string(),
            })),
        );
        let resp = ensure_parent_tag(&m, args()).unwrap();
        assert_eq!(resp.action, EnsureParentTagAction::Created);
        assert_eq!(resp.child_id, cid);
        assert_eq!(resp.parent_id, pid);
    }

    #[test]
    fn parses_skipped() {
        let m = MockTransport::new();
        m.expect_call(
            "tools/call",
            ScriptedResponse::Ok(json!({
                "action": "skipped",
                "child_id": Uuid::nil().to_string(),
                "parent_id": Uuid::nil().to_string(),
            })),
        );
        let resp = ensure_parent_tag(&m, args()).unwrap();
        assert_eq!(resp.action, EnsureParentTagAction::Skipped);
    }

    #[test]
    fn sends_both_names() {
        let m = MockTransport::new();
        m.expect_call_with(
            "tools/call",
            |p| {
                p["name"] == "ensure_parent_tag"
                    && p["arguments"]["child_tag"] == "tdd"
                    && p["arguments"]["parent_tag"] == "testing"
            },
            ScriptedResponse::Ok(json!({
                "action": "created",
                "child_id": Uuid::nil().to_string(),
                "parent_id": Uuid::nil().to_string(),
            })),
        );
        ensure_parent_tag(&m, args()).unwrap();
        m.assert_done();
    }

    #[test]
    fn cycle_rejection_surfaces_as_tool_error() {
        // fmem returns a tool error when a cycle would form.
        let m = MockTransport::new();
        m.expect_call(
            "tools/call",
            ScriptedResponse::ToolError {
                code: -32000,
                message: "PARENT_TAG edge tdd -> testing would form a cycle".into(),
            },
        );
        let err = ensure_parent_tag(&m, args()).unwrap_err();
        match err {
            Error::Tool { message, .. } => assert!(message.contains("cycle")),
            other => panic!("expected Tool, got {other:?}"),
        }
    }

    #[test]
    fn unknown_action_is_schema_error() {
        let m = MockTransport::new();
        m.expect_call(
            "tools/call",
            ScriptedResponse::Ok(json!({
                "action": "teleported",
                "child_id": Uuid::nil().to_string(),
                "parent_id": Uuid::nil().to_string(),
            })),
        );
        let err = ensure_parent_tag(&m, args()).unwrap_err();
        assert!(matches!(err, Error::Schema(_)));
    }

    #[test]
    fn session_id_included_when_set() {
        let m = MockTransport::new();
        m.expect_call_with(
            "tools/call",
            |p| p["arguments"]["session_id"] == "default",
            ScriptedResponse::Ok(json!({
                "action": "created",
                "child_id": Uuid::nil().to_string(),
                "parent_id": Uuid::nil().to_string(),
            })),
        );
        let mut a = args();
        a.session_id = Some("default".into());
        ensure_parent_tag(&m, a).unwrap();
    }
}
