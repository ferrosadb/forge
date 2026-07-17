//! [`BoardExec`]: the real [`crate::board::BoardSink`], backed by the CQL
//! task store (`forge_tasks::TaskStore`). This is the seam's live
//! implementation — `FakeBoard` (in [`crate::board`], test-only) is its
//! network-free stand-in used by [`crate::sync::pull`]'s unit tests.
//!
//! `CreateTaskRequest` has no `status` field (the store always creates a
//! task at its default status, `Triage` — see `forge_tasks::TaskStatus`),
//! so a [`crate::board_plan::BoardOp::Create`] whose `target_status` isn't
//! `Triage` needs a follow-up `update_task` call to land the row's mapped
//! status. `needs_status_followup`/`status_patch` are the pure helpers
//! that decide and build that follow-up; they're unit-tested directly,
//! without a store, per the module's TDD brief.

use crate::board::BoardSink;
use crate::board_plan::BoardOp;
use forge_tasks::{resolve_cql_hosts, TaskStatus, TaskStore, UpdateTaskPatch};

/// [`BoardSink`] backed by a live `forge_tasks::TaskStore` (CQL).
pub struct BoardExec {
    store: TaskStore,
}

impl BoardExec {
    /// Connects to the CQL cluster via `forge_tasks::resolve_cql_hosts`
    /// (honoring `cql_host`, then env, then the documented default — see
    /// that function's doc) and wraps the resulting store. No tenant
    /// scoping: sheet-sync operates on the default tenant.
    pub fn connect(cql_host: Option<&str>) -> anyhow::Result<Self> {
        let hosts = resolve_cql_hosts(cql_host);
        let store = TaskStore::connect(&hosts, None)?;
        Ok(Self { store })
    }

    /// Wraps an already-connected store. Lets callers (and future tests)
    /// inject a `TaskStore` directly rather than going through `connect`.
    pub fn new(store: TaskStore) -> Self {
        Self { store }
    }
}

impl BoardSink for BoardExec {
    fn existing_status(&self, task_id: &str) -> Option<TaskStatus> {
        self.store.get_task(task_id).ok().map(|t| t.task.status)
    }

    fn apply(&mut self, op: &BoardOp) -> anyhow::Result<Option<String>> {
        match op {
            BoardOp::Skip { .. } => Ok(None),
            BoardOp::Create {
                req, target_status, ..
            } => {
                let task = self.store.create_task(req.clone())?;
                if needs_status_followup(target_status) {
                    self.store
                        .update_task(&task.task_id, status_patch(target_status))?;
                }
                Ok(Some(task.task_id))
            }
            BoardOp::Update { task_id, patch, .. } => {
                self.store.update_task(task_id, patch.clone())?;
                Ok(Some(task_id.clone()))
            }
        }
    }
}

/// Whether a newly created task (which always lands at the store's
/// default status, `Triage`) needs a follow-up `update_task` to reach
/// `target`. `Triage` needs none; every other status does.
fn needs_status_followup(target: &TaskStatus) -> bool {
    !matches!(target, TaskStatus::Triage)
}

/// Builds the minimal [`UpdateTaskPatch`] that sets only `status`, leaving
/// every other field untouched (`None`), for the [`needs_status_followup`]
/// follow-up call.
fn status_patch(target: &TaskStatus) -> UpdateTaskPatch {
    UpdateTaskPatch {
        status: Some(target.as_str().to_string()),
        assignee: None,
        reviewer: None,
        priority: None,
        title: None,
        body: None,
        block_reason: None,
        result: None,
        summary: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn needs_status_followup_false_for_triage() {
        assert!(!needs_status_followup(&TaskStatus::Triage));
    }

    #[test]
    fn needs_status_followup_true_for_every_non_triage_status() {
        for status in [
            TaskStatus::Ready,
            TaskStatus::InProgress,
            TaskStatus::Blocked,
            TaskStatus::Complete,
            TaskStatus::Archived,
        ] {
            assert!(
                needs_status_followup(&status),
                "expected {status:?} to need a status follow-up"
            );
        }
    }

    #[test]
    fn status_patch_sets_only_status() {
        let patch = status_patch(&TaskStatus::InProgress);
        assert_eq!(patch.status.as_deref(), Some("in_progress"));
        assert!(patch.title.is_none());
        assert!(patch.priority.is_none());
        assert!(patch.assignee.is_none());
        assert!(patch.reviewer.is_none());
        assert!(patch.body.is_none());
        assert!(patch.block_reason.is_none());
        assert!(patch.result.is_none());
        assert!(patch.summary.is_none());
    }
}
