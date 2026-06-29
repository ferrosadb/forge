//! `debug_stop` degraded-board alerts for forge.
//!
//! When enabled, task-tool responses reflect the health of the CQL board cluster
//! so a developer's agent **stops and investigates** instead of trusting results
//! from a degraded board. Severity-based, mirroring the ferrosa-memory design:
//!
//! - **Degraded but serving** (a node down but quorum holds) → a `debug_stop_alert`
//!   is attached to the response; the tool still serves.
//! - **Critical** (board quorum lost) → the call fails so the agent halts.
//!
//! Detection is the **driver's own cluster view** ([`scylla::transport::ClusterData`]):
//! `get_nodes_info()` is the membership the driver discovered from `system.peers`
//! (the advertised client addresses, NAT/Docker-correct), and `Node::is_down()` is
//! the driver's liveness marker. We read that rather than re-probing seeds — it is
//! the same topology the driver actually routes queries over, so it can't lie.

use serde::Serialize;
use serde_json::Value;

/// A point-in-time view of the board cluster. forge's only monitored component is
/// the CQL board, so this is DB-only (no external providers, unlike fmem).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BoardHealth {
    pub nodes_up: usize,
    pub nodes_total: usize,
}

impl BoardHealth {
    /// Quorum holds when a strict majority of known nodes are up. With no nodes
    /// known yet (`nodes_total == 0`) quorum is treated as held (nothing to judge).
    pub fn quorum(&self) -> bool {
        self.nodes_total == 0 || self.nodes_up * 2 > self.nodes_total
    }
}

/// Whether the agent should warn-and-continue or stop.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Degraded,
    Critical,
}

/// The alert attached to (or failing) a tool response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DebugStopAlert {
    pub debug_stop: bool,
    pub severity: Severity,
    pub degraded: Vec<String>,
    pub action: &'static str,
}

impl DebugStopAlert {
    pub fn is_critical(&self) -> bool {
        self.severity == Severity::Critical
    }
}

/// Evaluate the alert. `None` when off or the board is fully healthy. Critical
/// when quorum is lost, else Degraded.
pub fn evaluate(health: &BoardHealth, debug_stop: bool) -> Option<DebugStopAlert> {
    if !debug_stop || health.nodes_total == 0 || health.nodes_up >= health.nodes_total {
        return None;
    }
    let (severity, msg) = if health.quorum() {
        (
            Severity::Degraded,
            format!(
                "{} of {} board nodes down (quorum OK)",
                health.nodes_total - health.nodes_up,
                health.nodes_total
            ),
        )
    } else {
        (
            Severity::Critical,
            format!(
                "board quorum lost ({}/{} nodes up)",
                health.nodes_up, health.nodes_total
            ),
        )
    };
    Some(DebugStopAlert {
        debug_stop: true,
        severity,
        degraded: vec![msg],
        action: "STOP and investigate",
    })
}

/// JSON-RPC-ish code returned when `debug_stop` fails a call on critical degradation.
pub const DEBUG_STOP_CRITICAL: i32 = -32010;

/// Apply `debug_stop` to a tool result given the board `health`.
///
/// - off / healthy → unchanged.
/// - critical → `Err((DEBUG_STOP_CRITICAL, msg))` so the agent halts.
/// - degraded → attach `debug_stop_alert` to an object result (non-object results
///   are wrapped under `result`).
pub fn apply_debug_stop(
    result: Result<Value, (i32, String)>,
    health: &BoardHealth,
    debug_stop: bool,
) -> Result<Value, (i32, String)> {
    let Some(alert) = evaluate(health, debug_stop) else {
        return result;
    };
    if alert.is_critical() {
        return Err((
            DEBUG_STOP_CRITICAL,
            format!(
                "debug_stop: board critically degraded — {}. {}",
                alert.degraded.join("; "),
                alert.action
            ),
        ));
    }
    result.map(|mut value| {
        let alert_json = serde_json::to_value(&alert).unwrap_or(Value::Null);
        match value.as_object_mut() {
            Some(obj) => {
                obj.insert("debug_stop_alert".to_string(), alert_json);
            }
            None => {
                value = serde_json::json!({ "result": value, "debug_stop_alert": alert_json });
            }
        }
        value
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn h(up: usize, total: usize) -> BoardHealth {
        BoardHealth {
            nodes_up: up,
            nodes_total: total,
        }
    }

    #[test]
    fn off_and_healthy_are_silent() {
        assert_eq!(evaluate(&h(1, 3), false), None);
        assert_eq!(evaluate(&h(3, 3), true), None);
        assert_eq!(evaluate(&h(0, 0), true), None); // nothing known yet
    }

    #[test]
    fn one_node_down_quorum_ok_is_degraded() {
        let a = evaluate(&h(2, 3), true).expect("alert");
        assert_eq!(a.severity, Severity::Degraded);
        assert!(a.degraded[0].contains("1 of 3 board nodes down"));
    }

    #[test]
    fn quorum_lost_is_critical() {
        let a = evaluate(&h(1, 3), true).expect("alert");
        assert_eq!(a.severity, Severity::Critical);
        assert!(a.degraded[0].contains("quorum lost"));
    }

    #[test]
    fn apply_passthrough_attach_and_fail() {
        // healthy / off → passthrough
        assert_eq!(
            apply_debug_stop(Ok(json!({"x":1})), &h(3, 3), true).unwrap(),
            json!({"x":1})
        );
        // degraded → attach
        let r = apply_debug_stop(Ok(json!({"x":1})), &h(2, 3), true).unwrap();
        assert_eq!(r["debug_stop_alert"]["severity"], "degraded");
        assert_eq!(r["x"], 1);
        // degraded, non-object → wrapped
        let r = apply_debug_stop(Ok(json!([1, 2])), &h(2, 3), true).unwrap();
        assert_eq!(r["result"], json!([1, 2]));
        // critical → fail loud
        let (code, msg) = apply_debug_stop(Ok(json!({"x":1})), &h(1, 3), true).unwrap_err();
        assert_eq!(code, DEBUG_STOP_CRITICAL);
        assert!(msg.contains("quorum lost"));
    }
}
