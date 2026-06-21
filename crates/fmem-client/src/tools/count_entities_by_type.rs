//! Typed wrapper over fmem's `count_entities_by_type` MCP tool.
//!
//! Read-only: returns entity counts for one `(tenant, session)` scope
//! broken down by `entity_type`, by `state`, and by the joint
//! `(entity_type, state)` histogram.  No side-effects, no session dirty
//! flip — safe to call on every `frg context-check`.
//!
//! Contract source:
//! `../ferrosa-memory/specs/implemented/feat-count-entities-by-type.md`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::error::Error;
use crate::transport::Transport;

/// Request args.  `session_id` is optional — the server defaults to the
/// nil UUID, same convention as `get_stats`.
#[derive(Debug, Clone, Serialize, Default)]
pub struct CountEntitiesByTypeArgs {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
}

/// Parsed response from `count_entities_by_type`.
///
/// Server-side invariant: `total == sum(by_entity_type) == sum(by_state)
/// == sum-of-sums(by_type_and_state)`.  Clients can re-assert via
/// `assert_invariant()` as defense-in-depth.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct CountEntitiesByTypeResponse {
    #[serde(default)]
    pub total: u64,
    #[serde(default)]
    pub by_entity_type: BTreeMap<String, u64>,
    #[serde(default)]
    pub by_state: BTreeMap<String, u64>,
    #[serde(default)]
    pub by_type_and_state: BTreeMap<String, BTreeMap<String, u64>>,
    #[serde(default)]
    pub duration_ms: u64,
}

impl CountEntitiesByTypeResponse {
    /// Defense-in-depth check of the server-side invariant.  Returns an
    /// error string describing the mismatch if any breakdown doesn't sum
    /// to `total`.  Callers decide whether to hard-fail or log.
    pub fn assert_invariant(&self) -> Result<(), String> {
        let sum_type: u64 = self.by_entity_type.values().sum();
        let sum_state: u64 = self.by_state.values().sum();
        let sum_joint: u64 = self
            .by_type_and_state
            .values()
            .flat_map(|inner| inner.values())
            .sum();
        if sum_type != self.total {
            return Err(format!(
                "by_entity_type sums to {sum_type} but total is {}",
                self.total
            ));
        }
        if sum_state != self.total {
            return Err(format!(
                "by_state sums to {sum_state} but total is {}",
                self.total
            ));
        }
        if sum_joint != self.total {
            return Err(format!(
                "by_type_and_state sums to {sum_joint} but total is {}",
                self.total
            ));
        }
        Ok(())
    }

    /// Count of entities of a specific type, or 0 if the type is absent.
    pub fn count_of_type(&self, entity_type: &str) -> u64 {
        self.by_entity_type.get(entity_type).copied().unwrap_or(0)
    }

    /// Count of entities of a specific (type, state), or 0 if either key is absent.
    pub fn count_of_type_state(&self, entity_type: &str, state: &str) -> u64 {
        self.by_type_and_state
            .get(entity_type)
            .and_then(|inner| inner.get(state).copied())
            .unwrap_or(0)
    }
}

/// Call `count_entities_by_type` on the server.
pub fn count_entities_by_type(
    transport: &dyn Transport,
    args: CountEntitiesByTypeArgs,
) -> Result<CountEntitiesByTypeResponse, Error> {
    let value = serde_json::to_value(&args)
        .map_err(|e| Error::Schema(format!("failed to serialize CountEntitiesByTypeArgs: {e}")))?;
    let raw = transport.call_tool("count_entities_by_type", value)?;
    serde_json::from_value(raw.clone()).map_err(|e| {
        Error::Schema(format!(
            "count_entities_by_type response did not match expected shape: {e}; raw={raw}"
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::mock::{MockTransport, ScriptedResponse};
    use serde_json::json;

    #[test]
    fn parses_full_response() {
        let m = MockTransport::new();
        m.expect_call(
            "tools/call",
            ScriptedResponse::Ok(json!({
                "total": 6,
                "by_entity_type": { "document": 2, "bug": 3, "function": 1 },
                "by_state": { "active": 4, "resolved": 2 },
                "by_type_and_state": {
                    "document": { "active": 2 },
                    "bug": { "active": 1, "resolved": 2 },
                    "function": { "active": 1 }
                },
                "duration_ms": 6
            })),
        );
        let r = count_entities_by_type(&m, CountEntitiesByTypeArgs::default()).unwrap();
        assert_eq!(r.total, 6);
        assert_eq!(r.count_of_type("bug"), 3);
        assert_eq!(r.count_of_type_state("bug", "resolved"), 2);
        assert_eq!(r.count_of_type_state("bug", "active"), 1);
        assert_eq!(r.count_of_type("function"), 1);
        r.assert_invariant()
            .expect("response should be self-consistent");
        m.assert_done();
    }

    #[test]
    fn empty_session_parses() {
        let m = MockTransport::new();
        m.expect_call(
            "tools/call",
            ScriptedResponse::Ok(json!({
                "total": 0,
                "by_entity_type": {},
                "by_state": {},
                "by_type_and_state": {},
                "duration_ms": 1
            })),
        );
        let r = count_entities_by_type(&m, CountEntitiesByTypeArgs::default()).unwrap();
        assert_eq!(r.total, 0);
        assert!(r.by_entity_type.is_empty());
        r.assert_invariant().unwrap();
        m.assert_done();
    }

    #[test]
    fn count_of_missing_keys_is_zero() {
        let r = CountEntitiesByTypeResponse {
            total: 3,
            by_entity_type: [("bug".to_string(), 3)].into_iter().collect(),
            by_state: [("active".to_string(), 3)].into_iter().collect(),
            by_type_and_state: [(
                "bug".to_string(),
                [("active".to_string(), 3)].into_iter().collect(),
            )]
            .into_iter()
            .collect(),
            duration_ms: 0,
        };
        assert_eq!(r.count_of_type("document"), 0);
        assert_eq!(r.count_of_type_state("bug", "nonexistent"), 0);
        assert_eq!(r.count_of_type_state("nonexistent", "active"), 0);
    }

    #[test]
    fn invariant_violation_is_detected() {
        // Construct a bad response (sum_by_state != total) — server-side bug
        // we want to notice rather than trust blindly.
        let bad = CountEntitiesByTypeResponse {
            total: 10,
            by_entity_type: [("bug".to_string(), 10)].into_iter().collect(),
            by_state: [("active".to_string(), 5)].into_iter().collect(), // 5 != 10
            by_type_and_state: [(
                "bug".to_string(),
                [("active".to_string(), 10)].into_iter().collect(),
            )]
            .into_iter()
            .collect(),
            duration_ms: 0,
        };
        let err = bad.assert_invariant().unwrap_err();
        assert!(err.contains("by_state"));
    }

    #[test]
    fn omits_session_id_when_none() {
        let args = CountEntitiesByTypeArgs::default();
        let v = serde_json::to_value(&args).unwrap();
        assert!(v.get("session_id").is_none());
    }

    #[test]
    fn includes_session_id_when_set() {
        let args = CountEntitiesByTypeArgs {
            session_id: Some("00000000-0000-0000-0000-000000000001".into()),
        };
        let v = serde_json::to_value(&args).unwrap();
        assert_eq!(v["session_id"], "00000000-0000-0000-0000-000000000001");
    }
}
