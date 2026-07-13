//! Binary-level contracts for structured checklist CLI operations.
//! Correctness: JSON records reach checklist state without log parsing or field reconstruction.
//! Last revised: 2026-07-12
//! Last changed: Added T-005 structured attempt start/finish coverage.

use std::fs;
use std::path::Path;
use std::process::{Command, Output};

fn frg(root: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_frg"))
        .current_dir(root)
        .args(args)
        .output()
        .unwrap()
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "command failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn checklist_cli_uses_structured_attempt_files() {
    let temp = tempfile::TempDir::new().unwrap();
    assert_success(&frg(
        temp.path(),
        &[
            "checklist",
            "create",
            "attempts",
            "--items",
            "Implement T-005",
        ],
    ));
    assert_success(&frg(
        temp.path(),
        &["checklist", "claim", "attempts", "--agent", "agent-a"],
    ));
    let fingerprint_path = temp.path().join("fingerprint.json");
    fs::write(
        &fingerprint_path,
        serde_json::json!({
            "acceptanceCriterion": "CLI and MCP parity",
            "relevantInputs": [{"path": "crates/cli/src/main.rs", "digest": "sha256:abc"}],
            "normalizedCommand": "cargo test -p forge checklist"
        })
        .to_string(),
    )
    .unwrap();
    let started = frg(
        temp.path(),
        &[
            "checklist",
            "attempt-start",
            "attempts",
            "implement-t-005",
            "--agent",
            "agent-a",
            "--role",
            "implementer",
            "--file",
            fingerprint_path.to_str().unwrap(),
        ],
    );
    assert_success(&started);
    let started: serde_json::Value = serde_json::from_slice(&started.stdout).unwrap();
    assert_eq!(started["attemptId"], "A-1");
    assert_eq!(started["decision"], "accepted");

    let finish_path = temp.path().join("finish.json");
    fs::write(
        &finish_path,
        serde_json::json!({
            "resultSignature": "tests passed",
            "progress": "wired CLI",
            "newInformation": "surface is stable",
            "nextAction": "verify MCP"
        })
        .to_string(),
    )
    .unwrap();
    let finished = frg(
        temp.path(),
        &[
            "checklist",
            "attempt-finish",
            "attempts",
            "implement-t-005",
            "A-1",
            "--file",
            finish_path.to_str().unwrap(),
        ],
    );
    assert_success(&finished);
    let finished: serde_json::Value = serde_json::from_slice(&finished.stdout).unwrap();
    assert_eq!(
        finished["items"][0]["attemptState"]["lastAttempt"]["attemptId"],
        "A-1"
    );
}

#[test]
fn checklist_cli_wires_wait_review_resolve_score_and_scored_ready() {
    let temp = tempfile::TempDir::new().unwrap();
    assert_success(&frg(
        temp.path(),
        &[
            "checklist",
            "create",
            "workflow",
            "--items",
            "Zulu,Alpha,Beta",
        ],
    ));

    let review_gate = temp.path().join("review-gate.json");
    fs::write(
        &review_gate,
        serde_json::json!({
            "kind": "review",
            "createdAt": "2026-07-12T18:00:00Z",
            "reason": "Needs human review"
        })
        .to_string(),
    )
    .unwrap();
    assert_success(&frg(
        temp.path(),
        &[
            "checklist",
            "wait",
            "workflow",
            "zulu",
            "--file",
            review_gate.to_str().unwrap(),
        ],
    ));
    let review = temp.path().join("review.json");
    fs::write(
        &review,
        serde_json::json!({
            "reviewId": "R-1",
            "outcome": "approved",
            "reviewerId": "human:bkearns",
            "reviewedAt": "2026-07-12T18:05:00Z",
            "reason": "Approved",
            "feedback": [],
            "followUps": []
        })
        .to_string(),
    )
    .unwrap();
    assert_success(&frg(
        temp.path(),
        &[
            "checklist",
            "review",
            "workflow",
            "zulu",
            "--file",
            review.to_str().unwrap(),
        ],
    ));

    let decision_gate = temp.path().join("decision-gate.json");
    fs::write(
        &decision_gate,
        serde_json::json!({
            "kind": "decision",
            "createdAt": "2026-07-12T18:06:00Z",
            "reason": "Choose the implementation path",
            "question": "Use the structured adapter?",
            "attemptIds": ["A-2"]
        })
        .to_string(),
    )
    .unwrap();
    assert_success(&frg(
        temp.path(),
        &[
            "checklist",
            "wait",
            "workflow",
            "alpha",
            "--file",
            decision_gate.to_str().unwrap(),
        ],
    ));
    let rejected = frg(
        temp.path(),
        &[
            "checklist",
            "resolve",
            "workflow",
            "alpha",
            "--by",
            "",
            "--reason",
            "pivot",
        ],
    );
    assert!(!rejected.status.success());
    let resolved = frg(
        temp.path(),
        &[
            "checklist",
            "resolve",
            "workflow",
            "alpha",
            "--by",
            "human:bkearns",
            "--reason",
            "Use the structured adapter",
        ],
    );
    assert_success(&resolved);
    let resolved: serde_json::Value = serde_json::from_slice(&resolved.stdout).unwrap();
    assert_eq!(resolved["priorAttemptIds"], serde_json::json!(["A-2"]));
    assert!(!resolved["recoveryHints"].as_array().unwrap().is_empty());

    let policy = temp.path().join("policy.json");
    fs::write(
        &policy,
        serde_json::json!({
            "defaultBasePriority": 10,
            "easeBonus": {"small": 0, "medium": 0, "large": 0, "unspecified": 0},
            "dagUnlock": {
                "priorityDivisor": 10,
                "effortWeight": {"small": 0, "medium": 0, "large": 0, "unspecified": 0}
            },
            "goalProgressMaxBonus": 0,
            "exactRetryPenaltyPerUnit": 4,
            "semanticFixationPenaltyPerUnit": 6,
            "parentGoalRetryPenaltyPerUnit": 8,
            "minimumFixatedItemsForGoalPenalty": 2,
            "minimumPostPivotReturnsForGoalPenalty": 2,
            "decay": {"intervalSeconds": 3600, "recoveryPerInterval": 2},
            "unblock": {"penalizedItem": 9, "penalizedGoal": 11},
            "criticalVisibilityFloor": 25
        })
        .to_string(),
    )
    .unwrap();
    let score = frg(
        temp.path(),
        &[
            "checklist",
            "score",
            "workflow",
            "--file",
            policy.to_str().unwrap(),
        ],
    );
    assert_success(&score);
    let score: serde_json::Value = serde_json::from_slice(&score.stdout).unwrap();
    assert!(score["items"][0]["components"]["basePriority"].is_number());
    assert!(score["items"][0]["explanation"].is_string());

    let ready = frg(
        temp.path(),
        &[
            "checklist",
            "ready",
            "workflow",
            "--scored",
            "--policy-file",
            policy.to_str().unwrap(),
        ],
    );
    assert_success(&ready);
    let ready: serde_json::Value = serde_json::from_slice(&ready.stdout).unwrap();
    assert_eq!(
        ready["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| entry["item"]["id"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["alpha", "beta"]
    );
}
