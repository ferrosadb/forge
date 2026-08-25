//! Live check that `BoardExec` round-trips a create-with-follow-up-status
//! through the real CQL task store. Run with a dev node up on 9042:
//!   cargo test -p forge-sheet-sync --test board_exec_live -- --ignored --nocapture
//!
//! Mirrors `crates/tasks/tests/board_health_live.rs`'s pattern: gated with
//! `#[ignore]` so the default `cargo test` run (no DB required) never hits
//! this file.

use forge_sheet_sync::board::BoardSink;
use forge_sheet_sync::board_exec::BoardExec;
use forge_sheet_sync::board_plan::BoardOp;
use forge_tasks::{CreateTaskRequest, TaskStatus};

#[test]
#[ignore = "requires a live CQL node on 127.0.0.1:9042"]
fn create_with_non_triage_target_status_lands_via_follow_up_update() {
    let mut exec = BoardExec::connect(None).expect("connect");

    let op = BoardOp::Create {
        row_id: "row-live-1".to_string(),
        req: CreateTaskRequest {
            origin: forge_tasks::TaskOrigin::default(),
            title: "board_exec_live smoke task".to_string(),
            body: None,
            assignee: None,
            reviewer: None,
            priority: None,
            workspace_kind: None,
            workspace_path: None,
            metadata: None,
            created_by: Some("board_exec_live".to_string()),
            skills: None,
            parents: None,
        },
        target_status: TaskStatus::InProgress,
    };

    let task_id = exec
        .apply(&op)
        .expect("apply create")
        .expect("create returns a task id");

    assert_eq!(
        exec.existing_status(&task_id).expect("read status"),
        Some(TaskStatus::InProgress),
        "create + follow-up update should land the task at the mapped target status"
    );
}
