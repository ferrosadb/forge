//! Board sink seam: [`BoardSink`] is the trait boundary between the sync
//! engine and the forge task board (CQL). Like [`crate::sheets::SheetsApi`],
//! it is injected so [`crate::sync::pull`] can be exercised end-to-end in
//! tests via `FakeBoard` — no network, no CQL.

use crate::board_plan::BoardOp;
use forge_tasks::TaskStatus;

/// Board read/write boundary for pull.
pub trait BoardSink {
    /// The board's *current* status for `task_id`, or `Ok(None)` if no such
    /// task is known to the board. Feeds [`crate::board_plan::plan_pull`]'s
    /// never-move-backward rule — see that module's doc.
    ///
    /// A read that FAILS is an `Err`, never `Ok(None)`. Collapsing the two
    /// disarmed the never-move-backward rule exactly when the board was
    /// unreachable: an unreadable `complete` task looked like a task with no
    /// status, and the pull happily reset it to the sheet's value.
    fn existing_status(&self, task_id: &str) -> anyhow::Result<Option<TaskStatus>>;

    /// Applies one planned op to the board. Returns the task id it created
    /// or updated; `None` for [`BoardOp::Skip`], which never touches the
    /// board.
    fn apply(&mut self, op: &BoardOp) -> anyhow::Result<Option<String>>;
}

/// Test-only in-memory [`BoardSink`]. `apply` mints a fresh `t_<n>` id for
/// every `Create` (an `Update`'s task id comes from the op itself, since it
/// already names an existing task) and records every applied op's kind and
/// row id in `applied`, so tests can assert exactly what pull did (or, for
/// dry-run, that it did nothing).
#[cfg(test)]
pub(crate) struct FakeBoard {
    pub statuses: std::collections::HashMap<String, TaskStatus>,
    pub applied: Vec<(String, String)>,
    next_id: usize,
    /// If `Some(n)`, the `n`-th call (1-indexed) to `apply` returns `Err`
    /// instead of applying the op — simulates a board mutation failing
    /// partway through a multi-row pull, so callers can assert what state
    /// was (and wasn't) persisted before the failure.
    fail_on_call: Option<usize>,
    call_count: usize,
    /// When true, `existing_status` returns `Err` — simulates the board being
    /// unreadable while a pull is deciding whether a task's status is protected.
    fail_status_read: bool,
}

#[cfg(test)]
impl FakeBoard {
    pub(crate) fn new() -> Self {
        Self {
            statuses: std::collections::HashMap::new(),
            applied: Vec::new(),
            next_id: 0,
            fail_on_call: None,
            call_count: 0,
            fail_status_read: false,
        }
    }

    /// A [`FakeBoard`] whose `n`-th `apply` call (1-indexed) fails with
    /// `Err`; every call before it succeeds normally.
    pub(crate) fn new_failing_on_call(n: usize) -> Self {
        Self {
            fail_on_call: Some(n),
            ..Self::new()
        }
    }

    /// A [`FakeBoard`] whose `existing_status` fails — the board is
    /// reachable enough to be asked and cannot answer.
    pub(crate) fn new_failing_status_read() -> Self {
        Self {
            fail_status_read: true,
            ..Self::new()
        }
    }

    fn mint_task_id(&mut self) -> String {
        self.next_id += 1;
        format!("t_{}", self.next_id)
    }
}

#[cfg(test)]
impl BoardSink for FakeBoard {
    fn existing_status(&self, task_id: &str) -> anyhow::Result<Option<TaskStatus>> {
        if self.fail_status_read {
            anyhow::bail!("FakeBoard: simulated status-read failure for {task_id}");
        }
        Ok(self.statuses.get(task_id).cloned())
    }

    fn apply(&mut self, op: &BoardOp) -> anyhow::Result<Option<String>> {
        self.call_count += 1;
        if self.fail_on_call == Some(self.call_count) {
            anyhow::bail!(
                "FakeBoard: simulated apply failure on call {}",
                self.call_count
            );
        }
        match op {
            BoardOp::Create {
                row_id,
                target_status,
                ..
            } => {
                let task_id = self.mint_task_id();
                self.statuses.insert(task_id.clone(), target_status.clone());
                self.applied.push(("create".to_string(), row_id.clone()));
                Ok(Some(task_id))
            }
            BoardOp::Update {
                row_id,
                task_id,
                patch,
            } => {
                if let Some(status_str) = &patch.status {
                    let status = TaskStatus::parse(status_str).ok_or_else(|| {
                        anyhow::anyhow!(
                            "FakeBoard::apply: patch.status {status_str:?} is not a valid TaskStatus"
                        )
                    })?;
                    self.statuses.insert(task_id.clone(), status);
                }
                self.applied.push(("update".to_string(), row_id.clone()));
                Ok(Some(task_id.clone()))
            }
            BoardOp::Skip { row_id, .. } => {
                self.applied.push(("skip".to_string(), row_id.clone()));
                Ok(None)
            }
        }
    }
}
