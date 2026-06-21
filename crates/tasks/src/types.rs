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

#[derive(Debug, Deserialize)]
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

#[derive(Debug, Deserialize)]
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression (task t_2c779031): serializing a `Task` whose body contains
    /// literal control characters (embedded newlines/tabs) must produce valid
    /// JSON. This mirrors the `frg task get` / `frg task list` emit path, which
    /// serializes the in-memory `Task` via serde_json. Before this was locked
    /// in, a body with raw newlines could surface as unescaped control chars and
    /// break `frg task list | jq`.
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
