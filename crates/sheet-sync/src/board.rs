//! Board sink seam: [`BoardSink`] is the trait boundary between the sync
//! engine and the forge task board (CQL). Like [`crate::sheets::SheetsApi`],
//! it is injected so [`crate::sync::pull`] can be exercised end-to-end in
//! tests via `FakeBoard` — no network, no CQL.

use crate::board_plan::BoardOp;
use forge_tasks::TaskStatus;

/// Board read/write boundary for pull.
pub trait BoardSink {
    /// The board's *current* status for `task_id`, or `None` if no such task
    /// is known to the board. Feeds [`crate::board_plan::plan_pull`]'s
    /// never-move-backward rule — see that module's doc.
    fn existing_status(&self, task_id: &str) -> Option<TaskStatus>;

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
}

#[cfg(test)]
impl FakeBoard {
    pub(crate) fn new() -> Self {
        Self {
            statuses: std::collections::HashMap::new(),
            applied: Vec::new(),
            next_id: 0,
        }
    }

    fn mint_task_id(&mut self) -> String {
        self.next_id += 1;
        format!("t_{}", self.next_id)
    }
}

#[cfg(test)]
impl BoardSink for FakeBoard {
    fn existing_status(&self, task_id: &str) -> Option<TaskStatus> {
        self.statuses.get(task_id).cloned()
    }

    fn apply(&mut self, op: &BoardOp) -> anyhow::Result<Option<String>> {
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
