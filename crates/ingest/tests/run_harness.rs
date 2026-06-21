//! Four-phase orchestrator integration tests.
//!
//! Build a synthetic skill tree in a tempdir, script a `MockTransport`
//! with the exact sequence of MCP calls `run()` is expected to make,
//! and assert the resulting `Summary`. Covers FMEA F7, F16, F18, F21,
//! F23–F29 at the orchestrator level.

use std::fs;
use std::path::PathBuf;

use forge_fmem_client::transport::mock::{MockTransport, ScriptedResponse};
use forge_ingest::skill_ingest::{run, RunConfig, VerifyFailureReason};
use serde_json::json;
use tempfile::TempDir;
use uuid::Uuid;

// ---------------------------------------------------------------------
// Fixture helpers
// ---------------------------------------------------------------------

struct Fixture {
    tmp: TempDir,
}

impl Fixture {
    fn new() -> Self {
        Self {
            tmp: TempDir::new().unwrap(),
        }
    }

    fn root(&self) -> PathBuf {
        self.tmp.path().to_path_buf()
    }

    fn skill(&self, rel: &str, frontmatter: &str, body: &str) {
        let full = self.tmp.path().join(rel);
        fs::create_dir_all(full.parent().unwrap()).unwrap();
        let mut raw = String::new();
        raw.push_str("---\n");
        raw.push_str(frontmatter);
        raw.push_str("---\n");
        raw.push_str(body);
        fs::write(full, raw).unwrap();
    }

    fn hierarchy(&self, body: &str) {
        fs::write(self.tmp.path().join("tag-hierarchy.yaml"), body).unwrap();
    }
}

fn config(root: PathBuf) -> RunConfig {
    RunConfig {
        root,
        filter: None,
        dry_run: false,
        force: false,
        session_id: None,
    }
}

fn ok_created() -> ScriptedResponse {
    ScriptedResponse::Ok(json!({
        "action": "created",
        "entity_id": Uuid::new_v4().to_string(),
        "version": "2026041601",
    }))
}

/// `Created` response with a server-reported list of deferred prereqs.
/// fmem commit `aae5771` emits this shape; Phase C uses it to decide
/// precisely which skills need a re-ingest call.
fn ok_created_missing(missing: Vec<&str>) -> ScriptedResponse {
    ScriptedResponse::Ok(json!({
        "action": "created",
        "entity_id": Uuid::new_v4().to_string(),
        "version": "2026041601",
        "missing_prerequisites": missing,
    }))
}

fn ok_skipped() -> ScriptedResponse {
    ScriptedResponse::Ok(json!({
        "action": "skipped",
        "entity_id": Uuid::new_v4().to_string(),
        "version": "2026041601",
        "reason": "content_hash unchanged",
    }))
}

fn ok_parent_tag_created() -> ScriptedResponse {
    ScriptedResponse::Ok(json!({
        "action": "created",
        "child_id": Uuid::new_v4().to_string(),
        "parent_id": Uuid::new_v4().to_string(),
    }))
}

fn ok_verify(tags: Vec<&str>, missing: Vec<&str>) -> ScriptedResponse {
    ScriptedResponse::Ok(json!({
        "exists": true,
        "entity_id": Uuid::new_v4().to_string(),
        "version": "2026041601",
        "content_hash": "sha256:abc",
        "tags": tags,
        "prerequisites": [],
        "required_by": [],
        "missing_prerequisites": missing,
    }))
}

// ---------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------

#[test]
fn dry_run_never_touches_transport() {
    let fx = Fixture::new();
    fx.skill(
        "task-level/tdd/SKILL.md",
        "name: tdd\ndescription: do tdd\n",
        "## Instructions\n- step\n",
    );
    let mut cfg = config(fx.root());
    cfg.dry_run = true;

    let mock = MockTransport::panicking(); // any call panics
    let summary = run(cfg, &mock).expect("dry run should succeed");
    assert_eq!(summary.skills_created, 0);
    assert_eq!(summary.skills_updated, 0);
    assert!(summary.verification_failures.is_empty());
    assert_eq!(summary.exit_code(), 0);
}

#[test]
fn happy_path_three_skills_no_hierarchy() {
    let fx = Fixture::new();
    fx.skill(
        "task-level/tdd/SKILL.md",
        "name: tdd\ndescription: do tdd\n",
        "## Instructions\n- step\n",
    );
    fx.skill(
        "task-level/refactor/SKILL.md",
        "name: refactor\ndescription: refactor\n",
        "## Instructions\n- step\n",
    );
    fx.skill(
        "tech/rust/SKILL.md",
        "name: rust\ndescription: rust\n",
        "body",
    );

    let mock = MockTransport::new();
    // Phase A: no hierarchy → no ensure_parent_tag calls.
    // Phase B: three ingest_skill calls.
    mock.expect_call("tools/call", ok_created()); // refactor (sorted by path comes first under task-level)
    mock.expect_call("tools/call", ok_created()); // tdd
    mock.expect_call("tools/call", ok_created()); // rust
                                                  // Phase D: three verify_skill calls. None need re-pass since
                                                  // none declare prerequisites.
    mock.expect_call("tools/call", ok_verify(vec!["task-level"], vec![]));
    mock.expect_call("tools/call", ok_verify(vec!["task-level"], vec![]));
    mock.expect_call("tools/call", ok_verify(vec!["tech"], vec![]));

    let summary = run(config(fx.root()), &mock).unwrap();
    assert_eq!(summary.skills_created, 3);
    assert_eq!(summary.skills_failed, 0);
    assert_eq!(summary.verified, 3);
    assert!(summary.verification_failures.is_empty());
    assert_eq!(summary.exit_code(), 0);
    mock.assert_done();
}

#[test]
fn phase_c_repass_uses_server_missing_prereqs_signal() {
    // tdd requires unit-testing. Walker sorts alphabetically, so tdd
    // is processed BEFORE unit-testing exists in fmem. fmem's Phase B
    // response for tdd carries missing_prerequisites: ["unit-testing"].
    // After unit-testing is ingested (with an empty missing list),
    // Phase C re-ingests tdd — now every edge lands.
    let fx = Fixture::new();
    fx.skill(
        "task-level/tdd/SKILL.md",
        "name: tdd\ndescription: tdd\nprerequisites: [unit-testing]\n",
        "body",
    );
    fx.skill(
        "task-level/unit-testing/SKILL.md",
        "name: unit-testing\ndescription: ut\n",
        "body",
    );

    let mock = MockTransport::new();
    // Phase B (two ingest calls, sorted by path): tdd (with missing
    // prereq), then unit-testing (all clean).
    mock.expect_call("tools/call", ok_created_missing(vec!["unit-testing"])); // tdd
    mock.expect_call("tools/call", ok_created()); // unit-testing
                                                  // Phase C: tdd's Phase B outcome had a non-empty list, so it gets
                                                  // re-ingested. unit-testing's outcome was clean, so no re-pass.
    mock.expect_call("tools/call", ok_skipped()); // tdd re-ingest
                                                  // Phase D verifies both.
    mock.expect_call("tools/call", ok_verify(vec!["task-level"], vec![])); // tdd
    mock.expect_call("tools/call", ok_verify(vec!["task-level"], vec![])); // unit-testing

    let summary = run(config(fx.root()), &mock).unwrap();
    assert_eq!(summary.skills_created, 2);
    assert_eq!(summary.repass_attempts, 1);
    assert_eq!(summary.repass_completed, 1);
    assert_eq!(summary.verified, 2);
    assert_eq!(summary.exit_code(), 0);
    mock.assert_done();
}

#[test]
fn phase_c_skipped_when_no_missing_prereqs_reported() {
    // A skill declares a prereq that fmem already had from a prior run
    // — the first-pass response carries no missing_prerequisites.
    // Phase C must NOT re-ingest (it would be wasted work).
    let fx = Fixture::new();
    fx.skill(
        "task-level/tdd/SKILL.md",
        "name: tdd\ndescription: tdd\nprerequisites: [unit-testing]\n",
        "body",
    );

    let mock = MockTransport::new();
    // Phase B: ok_created (empty missing list — prereq already in fmem).
    mock.expect_call("tools/call", ok_created());
    // Phase D only — no re-pass.
    mock.expect_call("tools/call", ok_verify(vec!["task-level"], vec![]));

    let summary = run(config(fx.root()), &mock).unwrap();
    assert_eq!(summary.skills_created, 1);
    assert_eq!(
        summary.repass_attempts, 0,
        "no re-pass when server says all prereqs resolved"
    );
    assert_eq!(summary.verified, 1);
    assert_eq!(summary.exit_code(), 0);
    mock.assert_done();
}

#[test]
fn verification_failure_exits_4() {
    let fx = Fixture::new();
    fx.skill(
        "task-level/tdd/SKILL.md",
        "name: tdd\ndescription: do tdd\nprerequisites: [ghost-prereq]\n",
        "body",
    );

    let mock = MockTransport::new();
    mock.expect_call("tools/call", ok_created()); // ingest_skill
                                                  // Phase D sees the missing prereq.
    mock.expect_call(
        "tools/call",
        ok_verify(vec!["task-level"], vec!["ghost-prereq"]),
    );

    let summary = run(config(fx.root()), &mock).unwrap();
    assert_eq!(summary.skills_created, 1);
    assert_eq!(summary.verification_failures.len(), 1);
    let failure = &summary.verification_failures[0];
    assert_eq!(failure.skill, "tdd");
    assert!(matches!(
        failure.reason,
        VerifyFailureReason::MissingPrerequisites(_)
    ));
    assert_eq!(summary.exit_code(), 4);
}

#[test]
fn missing_expected_tag_fails_verification() {
    // Skill declares `tags: [testing]`, but fmem's verify reports only
    // [task-level]. This is a data-integrity failure — exit 4.
    let fx = Fixture::new();
    fx.skill(
        "task-level/tdd/SKILL.md",
        "name: tdd\ndescription: do tdd\ntags: [testing]\n",
        "body",
    );

    let mock = MockTransport::new();
    mock.expect_call("tools/call", ok_created()); // ingest_skill
    mock.expect_call("tools/call", ok_verify(vec!["task-level"], vec![]));

    let summary = run(config(fx.root()), &mock).unwrap();
    assert_eq!(summary.verification_failures.len(), 1);
    assert!(matches!(
        summary.verification_failures[0].reason,
        VerifyFailureReason::MissingTags { .. }
    ));
    assert_eq!(summary.exit_code(), 4);
}

#[test]
fn hierarchy_phase_a_wired() {
    let fx = Fixture::new();
    fx.skill(
        "task-level/tdd/SKILL.md",
        "name: tdd\ndescription: do tdd\n",
        "body",
    );
    fx.skill(
        "quality/SKILL-pretend/SKILL.md",
        "name: quality-guide\ndescription: q\n",
        "body",
    );
    fx.hierarchy("task-level: quality\n");

    let mock = MockTransport::new();
    // Phase A: one ensure_parent_tag.
    mock.expect_call("tools/call", ok_parent_tag_created());
    // Phase B: two ingest_skill.
    mock.expect_call("tools/call", ok_created());
    mock.expect_call("tools/call", ok_created());
    // Phase D: two verify_skill.
    mock.expect_call("tools/call", ok_verify(vec!["quality"], vec![]));
    mock.expect_call("tools/call", ok_verify(vec!["task-level"], vec![]));

    let summary = run(config(fx.root()), &mock).unwrap();
    assert_eq!(summary.taxonomy_edges_created, 1);
    assert_eq!(summary.skills_created, 2);
    assert_eq!(summary.verified, 2);
    assert_eq!(summary.exit_code(), 0);
    mock.assert_done();
}

#[test]
fn filter_matches_zero_warns_exits_clean() {
    let fx = Fixture::new();
    fx.skill(
        "task-level/tdd/SKILL.md",
        "name: tdd\ndescription: tdd\n",
        "body",
    );

    let mut cfg = config(fx.root());
    cfg.filter = Some("nonexistent*".into());

    let mock = MockTransport::new();
    // Filter matches nothing — no ingest/verify calls.
    let summary = run(cfg, &mock).unwrap();
    assert_eq!(summary.skills_created, 0);
    assert_eq!(summary.skills_filtered_out, 1);
    assert_eq!(summary.exit_code(), 0);
    mock.assert_done();
}

#[test]
fn name_collision_aborts_before_any_call() {
    let fx = Fixture::new();
    fx.skill(
        "task-level/tdd/SKILL.md",
        "name: duplicate\ndescription: a\n",
        "",
    );
    fx.skill(
        "tech/rust/SKILL.md",
        "name: duplicate\ndescription: b\n",
        "",
    );

    let mock = MockTransport::panicking();
    let err = run(config(fx.root()), &mock).unwrap_err();
    let msg = format!("{err}");
    assert!(msg.contains("collision"));
}

#[test]
fn ingest_transport_error_counts_as_failure() {
    let fx = Fixture::new();
    fx.skill(
        "task-level/tdd/SKILL.md",
        "name: tdd\ndescription: do tdd\n",
        "body",
    );

    let mock = MockTransport::new();
    mock.expect_call(
        "tools/call",
        ScriptedResponse::ToolError {
            code: -32000,
            message: "backend unreachable".into(),
        },
    );
    // Phase D still runs; with nothing ingested the verify call sees
    // the skill as missing.
    mock.expect_call(
        "tools/call",
        ScriptedResponse::Ok(json!({
            "exists": false,
            "tags": [],
            "prerequisites": [],
            "required_by": [],
            "missing_prerequisites": []
        })),
    );

    let summary = run(config(fx.root()), &mock).unwrap();
    assert_eq!(summary.skills_failed, 1);
    assert_eq!(summary.verification_failures.len(), 1);
    assert!(matches!(
        summary.verification_failures[0].reason,
        VerifyFailureReason::DoesNotExist
    ));
    // Verification failures trump transport failures in exit-code map.
    assert_eq!(summary.exit_code(), 4);
}

#[test]
fn force_flag_omits_content_hash_on_the_wire() {
    let fx = Fixture::new();
    fx.skill(
        "task-level/tdd/SKILL.md",
        "name: tdd\ndescription: do tdd\n",
        "body",
    );

    let mut cfg = config(fx.root());
    cfg.force = true;

    let mock = MockTransport::new();
    mock.expect_call_with(
        "tools/call",
        |p| {
            p["name"] == "ingest_skill"
                // content_hash must be absent under --force.
                && p["arguments"].get("content_hash").is_none()
        },
        ok_created(),
    );
    mock.expect_call("tools/call", ok_verify(vec!["task-level"], vec![]));

    let summary = run(cfg, &mock).unwrap();
    assert_eq!(summary.exit_code(), 0);
    mock.assert_done();
}
