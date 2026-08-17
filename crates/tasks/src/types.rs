//! Domain types for the task system.

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// TaskStatus
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Triage,
    Ready,
    InProgress,
    Blocked,
    Complete,
    Archived,
}

impl TaskStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            TaskStatus::Triage => "triage",
            TaskStatus::Ready => "ready",
            TaskStatus::InProgress => "in_progress",
            TaskStatus::Blocked => "blocked",
            TaskStatus::Complete => "complete",
            TaskStatus::Archived => "archived",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "triage" => Some(TaskStatus::Triage),
            "ready" => Some(TaskStatus::Ready),
            "in_progress" => Some(TaskStatus::InProgress),
            "blocked" => Some(TaskStatus::Blocked),
            "complete" => Some(TaskStatus::Complete),
            "archived" => Some(TaskStatus::Archived),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Task
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    pub task_id: String,
    pub title: String,
    pub body: Option<String>,
    pub status: TaskStatus,
    pub assignee: Option<String>,
    pub reviewer: Option<String>,
    pub priority: i32,
    pub workspace_kind: Option<String>,
    pub workspace_path: Option<String>,
    pub created_by: String,
    pub block_reason: Option<String>,
    pub result: Option<String>,
    pub summary: Option<String>,
    pub metadata: Option<serde_json::Value>,
    pub skills: Vec<String>,
    pub related_entity_ids: Vec<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

// ---------------------------------------------------------------------------
// TaskWithLinks
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize)]
pub struct TaskLink {
    pub link_type: String,
    pub task_id: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct TaskWithLinks {
    pub task: Task,
    pub parents: Vec<TaskLink>,
    pub children: Vec<TaskLink>,
    pub recent_comments: Vec<Comment>,
}

// ---------------------------------------------------------------------------
// Comment
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Comment {
    pub comment_id: String,
    pub author: String,
    pub body: String,
    pub created_at: i64,
}

// ---------------------------------------------------------------------------
// Request / Patch types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
pub struct CreateTaskRequest {
    pub title: String,
    pub body: Option<String>,
    pub assignee: Option<String>,
    pub reviewer: Option<String>,
    pub priority: Option<i32>,
    pub workspace_kind: Option<String>,
    pub workspace_path: Option<String>,
    pub metadata: Option<String>,
    pub created_by: Option<String>,
    pub skills: Option<Vec<String>>,
    pub parents: Option<Vec<String>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateTaskPatch {
    pub status: Option<String>,
    pub assignee: Option<String>,
    pub reviewer: Option<String>,
    pub priority: Option<i32>,
    pub title: Option<String>,
    pub body: Option<String>,
    pub block_reason: Option<String>,
    pub result: Option<String>,
    pub summary: Option<String>,
}

// ---------------------------------------------------------------------------
// Filter
// ---------------------------------------------------------------------------

#[derive(Debug, Default)]
pub struct TaskFilter {
    pub status: Option<String>,
    pub assignee: Option<String>,
    pub priority_gte: Option<i32>,
    pub priority_lte: Option<i32>,
    pub limit: Option<usize>,
}

// ---------------------------------------------------------------------------
// KanbanBoard
// ---------------------------------------------------------------------------

impl Task {
    /// A listing-sized copy: the fields you scan a board with, without the prose.
    ///
    /// `task_board` returned 619,574 characters for 382 rows and exceeded the
    /// tool-result token limit outright, so an agent could not read the board at
    /// all without spilling it to a file first. Almost all of that is `body`,
    /// `result` and `metadata` -- the long-form fields that matter when you are
    /// working a single task and are noise when you are choosing between eighty.
    ///
    /// `task_get` still returns everything, so nothing is lost: the detail is one
    /// call away for the task you actually pick.
    pub fn slim(&self) -> Task {
        Task {
            body: None,
            result: None,
            metadata: None,
            // Kept: a one-line summary is what makes a listing decidable, and it
            // is bounded by construction.
            summary: self.summary.clone(),
            ..self.clone()
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct KanbanBoard {
    pub columns: KanbanColumns,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct KanbanColumns {
    pub triage: Vec<Task>,
    pub ready: Vec<Task>,
    pub in_progress: Vec<Task>,
    pub blocked: Vec<Task>,
    pub complete: Vec<Task>,
}

impl KanbanBoard {
    /// Every column slimmed. See `Task::slim`.
    pub fn slim(&self) -> KanbanBoard {
        let slim = |column: &Vec<Task>| column.iter().map(Task::slim).collect();
        KanbanBoard {
            columns: KanbanColumns {
                triage: slim(&self.columns.triage),
                ready: slim(&self.columns.ready),
                in_progress: slim(&self.columns.in_progress),
                blocked: slim(&self.columns.blocked),
                complete: slim(&self.columns.complete),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression (task t_2c779031): serializing a `Task` whose body contains
    /// literal control characters (embedded newlines/tabs) must produce valid
    /// JSON. This mirrors the `frg task get` / `frg task list` emit path, which
    /// serializes the in-memory `Task` via serde_json. Before this was locked
    /// in, a body with raw newlines could surface as unescaped control chars and
    /// break `frg task list | jq`.
    fn task_at(id: &str, created_at: i64) -> Task {
        Task {
            task_id: id.to_string(),
            title: format!("task {id}"),
            body: Some("a long body that a listing does not need".repeat(40)),
            status: TaskStatus::Triage,
            assignee: None,
            reviewer: None,
            priority: 50,
            workspace_kind: None,
            workspace_path: None,
            created_by: "agent".to_string(),
            block_reason: None,
            result: Some("a long result".repeat(40)),
            summary: Some("one line that makes a listing decidable".to_string()),
            metadata: None,
            skills: Vec::new(),
            related_entity_ids: Vec::new(),
            created_at,
            updated_at: created_at,
        }
    }

    #[test]
    fn slim_drops_the_prose_and_keeps_what_a_listing_needs() {
        // task_board returned 619,574 characters for 382 tasks and exceeded the
        // tool-result token limit outright, so the board could not be read at all
        // without spilling it to a file. Nearly all of it is body/result.
        let full = task_at("t_1", 10);
        let slim = full.slim();

        assert_eq!(slim.body, None, "body is the bulk and belongs in task_get");
        assert_eq!(slim.result, None);
        assert_eq!(slim.metadata, None);

        // Kept, because these are what make a row decidable without opening it.
        assert_eq!(slim.task_id, "t_1");
        assert_eq!(slim.title, full.title);
        assert_eq!(slim.status, full.status);
        assert_eq!(slim.priority, 50);
        assert_eq!(slim.summary, full.summary);
        assert_eq!(slim.created_at, 10);

        let big = serde_json::to_string(&full).unwrap().len();
        let small = serde_json::to_string(&slim).unwrap().len();
        assert!(
            small * 4 < big,
            "slim should be far smaller: {small} vs {big}"
        );
    }

    #[test]
    fn a_board_column_is_ordered_newest_first() {
        // The defect: rows arrive in task_id order because the table is
        // PRIMARY KEY (tenant_id, task_id), so a LIMIT took an arbitrary slice
        // rather than the newest rows. Tasks created hours earlier were absent
        // from task_board and task_list while task_get returned them fine.
        let mut column = vec![
            task_at("t_aaa", 100),
            task_at("t_zzz", 300),
            task_at("t_mmm", 200),
        ];
        crate::store::sort_newest_first(&mut column);
        assert_eq!(
            column.iter().map(|t| t.created_at).collect::<Vec<_>>(),
            vec![300, 200, 100],
            "newest first, regardless of how task_id sorts"
        );
    }

    #[test]
    fn ties_break_on_task_id_so_paging_cannot_shuffle() {
        // Two tasks created in the same millisecond must have a stable order, or a
        // page boundary could return one twice and the other never.
        let mut column = vec![task_at("t_bbb", 100), task_at("t_aaa", 100)];
        crate::store::sort_newest_first(&mut column);
        assert_eq!(
            column
                .iter()
                .map(|t| t.task_id.as_str())
                .collect::<Vec<_>>(),
            vec!["t_aaa", "t_bbb"],
            "equal timestamps order by id, giving a total order"
        );
    }

    #[test]
    fn task_with_multiline_body_serializes_to_valid_json() {
        let task = Task {
            task_id: "t_deadbeef".to_string(),
            title: "multi-line body".to_string(),
            body: Some("first line\nsecond line\twith tab\rand carriage".to_string()),
            status: TaskStatus::Triage,
            assignee: None,
            reviewer: None,
            priority: 50,
            workspace_kind: None,
            workspace_path: None,
            created_by: "agent".to_string(),
            block_reason: None,
            result: None,
            summary: None,
            metadata: None,
            skills: Vec::new(),
            related_entity_ids: Vec::new(),
            created_at: 1,
            updated_at: 2,
        };

        // Single task (frg task get's inner task) and a list (frg task list).
        let single = serde_json::to_string(&task).unwrap();
        let list = serde_json::to_string(&vec![task.clone()]).unwrap();

        for out in [&single, &list] {
            assert!(
                !out.contains('\n') && !out.contains('\t') && !out.contains('\r'),
                "raw control char leaked into compact JSON: {out:?}"
            );
            // Must re-parse cleanly (the `| jq` scenario from the bug report).
            let _: serde_json::Value =
                serde_json::from_str(out).expect("task JSON must be valid / jq-parseable");
        }

        // Body round-trips byte-for-byte through serialize → parse.
        let parsed: serde_json::Value = serde_json::from_str(&single).unwrap();
        assert_eq!(
            parsed["body"].as_str().unwrap(),
            "first line\nsecond line\twith tab\rand carriage"
        );
    }
}
