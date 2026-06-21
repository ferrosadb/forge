//! Typed wrapper over fmem's `verify_skill` MCP tool.
//!
//! fmem ships this in
//! `ferrosa-memory-core/src/skill.rs::verify_skill`. Forge calls it
//! once per parsed skill at the end of the run (blueprint Phase D —
//! verification gate). A skill is considered "fully ingested" when
//! `exists == true`, `missing_prerequisites` is empty, and every tag
//! the local parser produced appears in the returned `tags` list.
//!
//! The tool never errors on missing skills — it returns
//! `{exists: false}` so verification pipelines can see negative
//! results too.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::Error;
use crate::transport::Transport;

/// Arguments for `verify_skill`.
#[derive(Debug, Clone, Serialize)]
pub struct VerifySkillArgs {
    pub skill_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
}

/// Response: graph neighborhood of the named skill.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct VerifySkillResponse {
    pub exists: bool,
    #[serde(default)]
    pub entity_id: Option<Uuid>,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub content_hash: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub prerequisites: Vec<String>,
    #[serde(default)]
    pub required_by: Vec<String>,
    #[serde(default)]
    pub missing_prerequisites: Vec<String>,
}

/// Call fmem's `verify_skill`.
pub fn verify_skill<T: Transport>(
    transport: &T,
    args: VerifySkillArgs,
) -> Result<VerifySkillResponse, Error> {
    let args_value = serde_json::to_value(&args)
        .map_err(|e| Error::Schema(format!("failed to serialize VerifySkillArgs: {e}")))?;
    let raw = transport.call_tool("verify_skill", args_value)?;
    let typed: VerifySkillResponse = serde_json::from_value(raw.clone()).map_err(|e| {
        Error::Schema(format!(
            "verify_skill response did not match expected shape: {e}; raw={raw}"
        ))
    })?;
    Ok(typed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::mock::{MockTransport, ScriptedResponse};
    use serde_json::json;

    fn args(name: &str) -> VerifySkillArgs {
        VerifySkillArgs {
            skill_name: name.into(),
            session_id: None,
        }
    }

    #[test]
    fn parses_existing_skill_with_full_neighborhood() {
        let eid = Uuid::new_v4();
        let m = MockTransport::new();
        m.expect_call(
            "tools/call",
            ScriptedResponse::Ok(json!({
                "exists": true,
                "entity_id": eid.to_string(),
                "version": "2026041601",
                "content_hash": "sha256:abc",
                "tags": ["task-level", "testing"],
                "prerequisites": ["unit-testing"],
                "required_by": ["bdd"],
                "missing_prerequisites": []
            })),
        );
        let resp = verify_skill(&m, args("tdd")).unwrap();
        assert!(resp.exists);
        assert_eq!(resp.entity_id, Some(eid));
        assert_eq!(resp.version.as_deref(), Some("2026041601"));
        assert_eq!(resp.tags, vec!["task-level", "testing"]);
        assert_eq!(resp.prerequisites, vec!["unit-testing"]);
        assert_eq!(resp.required_by, vec!["bdd"]);
        assert!(resp.missing_prerequisites.is_empty());
    }

    #[test]
    fn parses_nonexistent_skill_cleanly() {
        let m = MockTransport::new();
        m.expect_call(
            "tools/call",
            ScriptedResponse::Ok(json!({
                "exists": false,
                "tags": [],
                "prerequisites": [],
                "required_by": [],
                "missing_prerequisites": []
            })),
        );
        let resp = verify_skill(&m, args("ghost")).unwrap();
        assert!(!resp.exists);
        assert!(resp.entity_id.is_none());
        assert!(resp.version.is_none());
    }

    #[test]
    fn parses_missing_prereqs_list() {
        let m = MockTransport::new();
        m.expect_call(
            "tools/call",
            ScriptedResponse::Ok(json!({
                "exists": true,
                "entity_id": Uuid::new_v4().to_string(),
                "version": "2026041601",
                "tags": [],
                "prerequisites": [],
                "required_by": [],
                "missing_prerequisites": ["unit-testing", "refactor"]
            })),
        );
        let resp = verify_skill(&m, args("tdd")).unwrap();
        assert_eq!(resp.missing_prerequisites, vec!["unit-testing", "refactor"]);
    }

    #[test]
    fn sends_skill_name() {
        let m = MockTransport::new();
        m.expect_call_with(
            "tools/call",
            |p| p["name"] == "verify_skill" && p["arguments"]["skill_name"] == "tdd",
            ScriptedResponse::Ok(json!({
                "exists": false,
                "tags": [],
                "prerequisites": [],
                "required_by": [],
                "missing_prerequisites": []
            })),
        );
        verify_skill(&m, args("tdd")).unwrap();
        m.assert_done();
    }

    #[test]
    fn bad_shape_is_schema_error() {
        let m = MockTransport::new();
        m.expect_call(
            "tools/call",
            // Missing `exists` field — not parseable.
            ScriptedResponse::Ok(json!({ "hello": "world" })),
        );
        let err = verify_skill(&m, args("tdd")).unwrap_err();
        assert!(matches!(err, Error::Schema(_)));
    }
}
