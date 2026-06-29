//! `TaskStore` — synchronous wrapper around the async scylla driver.
//!
//! Each `TaskStore` owns a single-thread tokio runtime and a `scylla::Session`.
//! All public methods are synchronous; they drive the async session via
//! `rt.block_on(...)`.
//!
//! All CQL uses literal string interpolation (no PREPARE) to work around the
//! ferrosa PREPARE bug.  Single-quote characters in text values are escaped by
//! doubling them.

#![allow(deprecated)] // scylla 0.15: into_legacy_result / rows_or_empty are deprecated but functional

use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use uuid::Uuid;

use crate::schema::{CREATE_TASKS_TABLE, CREATE_TASK_COMMENTS_TABLE, CREATE_TASK_LINKS_TABLE};
use crate::types::{
    Comment, CreateTaskRequest, KanbanBoard, KanbanColumns, Task, TaskFilter, TaskStatus,
    TaskWithLinks, UpdateTaskPatch,
};

/// Fixed tenant UUID for the single-user forge setup.
const TENANT_ID: &str = "00000000-0000-0000-0000-000000000001";

/// Escape a string for inline CQL (double any single quotes).
fn esc(s: &str) -> String {
    s.replace('\'', "''")
}

/// Format an optional string for CQL: NULL or 'value'.
fn opt_str(v: &Option<String>) -> String {
    match v {
        None => "null".to_string(),
        Some(s) => format!("'{}'", esc(s)),
    }
}

/// Timestamp in milliseconds since Unix epoch.
fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

/// Generate a task ID: "t_" + 8 lowercase hex chars from a random UUID.
fn gen_task_id() -> String {
    let v = Uuid::new_v4().as_u128() as u32;
    format!("t_{:08x}", v)
}

// ---------------------------------------------------------------------------
// Helper: run a CQL statement on an Arc<Session> in a blocking fashion.
// ---------------------------------------------------------------------------

macro_rules! cql_exec {
    ($rt:expr, $session:expr, $cql:expr) => {{
        let session = Arc::clone($session);
        let cql: String = $cql;
        $rt.block_on(async move { session.query_unpaged(cql.as_str(), ()).await })
    }};
}

// ---------------------------------------------------------------------------
// TaskStore
// ---------------------------------------------------------------------------

pub struct TaskStore {
    rt: tokio::runtime::Runtime,
    session: Arc<scylla::Session>,
    tenant_id: String,
}

impl TaskStore {
    /// Connect to the CQL cluster, create schema, and return a `TaskStore`.
    ///
    /// `cql_hosts` are the bootstrap contact points: passing every node lets the
    /// driver start from whichever is up and fail over for queries, so the board
    /// survives a single node loss instead of dying with one fixed contact point.
    pub fn connect(cql_hosts: &[String], tenant_id: Option<&str>) -> Result<Self> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .context("build tokio runtime")?;

        anyhow::ensure!(!cql_hosts.is_empty(), "no CQL contact points provided");
        let hosts = cql_hosts.to_vec();
        let session: scylla::Session = rt
            .block_on(async {
                scylla::SessionBuilder::new()
                    .known_nodes(&hosts)
                    .user("ferrosa_admin", "ferrosa_admin")
                    .build()
                    .await
            })
            .context("connect to CQL")?;

        let session = Arc::new(session);
        let store = Self {
            rt,
            session,
            tenant_id: tenant_id.unwrap_or(TENANT_ID).to_string(),
        };
        store.ensure_schema()?;
        Ok(store)
    }

    /// The driver's live view of the board cluster, derived from the topology it
    /// discovered via `system.peers` (the advertised client addresses, so this is
    /// NAT/Docker-correct): how many nodes are known and how many are up.
    /// `Node::is_down()` is the driver's own liveness marker — the same topology
    /// it routes queries over, so it can't disagree with reality.
    pub fn board_health(&self) -> crate::debug_stop::BoardHealth {
        let cluster = self.session.get_cluster_data();
        let nodes = cluster.get_nodes_info();
        let nodes_total = nodes.len();
        let nodes_up = nodes
            .iter()
            .filter(|n| n.is_enabled() && !n.is_down())
            .count();
        crate::debug_stop::BoardHealth {
            nodes_up,
            nodes_total,
        }
    }

    /// Create the three task tables if they don't exist (idempotent).
    fn ensure_schema(&self) -> Result<()> {
        for stmt in [
            CREATE_TASKS_TABLE,
            CREATE_TASK_LINKS_TABLE,
            CREATE_TASK_COMMENTS_TABLE,
        ] {
            cql_exec!(self.rt, &self.session, stmt.to_string())
                .with_context(|| format!("ensure_schema: {}", &stmt[..50]))?;
        }
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Task CRUD
    // -----------------------------------------------------------------------

    /// Create a new task and return it.
    pub fn create_task(&self, req: CreateTaskRequest) -> Result<Task> {
        let task_id = gen_task_id();
        let now = now_ms();
        let status = TaskStatus::Triage;
        let priority = req.priority.unwrap_or(50);
        let created_by = req
            .created_by
            .clone()
            .unwrap_or_else(|| "agent".to_string());

        let skills_set = format_set_text(req.skills.as_deref().unwrap_or(&[]));

        let cql = format!(
            "INSERT INTO agent_memory.tasks \
             (tenant_id, task_id, title, body, status, assignee, reviewer, priority, \
              workspace_kind, workspace_path, created_by, block_reason, result, summary, \
              metadata, skills, related_entity_ids, created_at, updated_at) \
             VALUES ({tenant}, '{tid}', '{title}', {body}, '{status}', {assignee}, {reviewer}, \
              {priority}, {wkind}, {wpath}, '{cby}', null, null, null, {meta}, {skills}, \
              {{}}, {now}, {now})",
            tenant = self.tenant_id,
            tid = esc(&task_id),
            title = esc(&req.title),
            body = opt_str(&req.body),
            status = status.as_str(),
            assignee = opt_str(&req.assignee),
            reviewer = opt_str(&req.reviewer),
            priority = priority,
            wkind = opt_str(&req.workspace_kind),
            wpath = opt_str(&req.workspace_path),
            meta = opt_str(&req.metadata),
            cby = esc(&created_by),
            skills = skills_set,
            now = now,
        );

        cql_exec!(self.rt, &self.session, cql).context("create_task INSERT")?;

        // Link to parents if provided
        if let Some(parents) = &req.parents {
            for parent_id in parents {
                self.link_tasks(parent_id, &task_id, "child")?;
            }
        }

        let task = Task {
            task_id,
            title: req.title,
            body: req.body,
            status,
            assignee: req.assignee,
            reviewer: req.reviewer,
            priority,
            workspace_kind: req.workspace_kind,
            workspace_path: req.workspace_path,
            created_by,
            block_reason: None,
            result: None,
            summary: None,
            metadata: req
                .metadata
                .map(|s| serde_json::from_str(&s).unwrap_or(serde_json::Value::String(s))),
            skills: req.skills.unwrap_or_default(),
            related_entity_ids: Vec::new(),
            created_at: now,
            updated_at: now,
        };
        Ok(task)
    }

    /// Apply a patch to an existing task and return the updated task.
    pub fn update_task(&self, task_id: &str, patch: UpdateTaskPatch) -> Result<Task> {
        let existing = self.get_task(task_id)?;
        let mut task = existing.task;
        let now = now_ms();

        if let Some(s) = patch.status {
            task.status = TaskStatus::parse(&s).ok_or_else(|| anyhow!("Unknown status: {}", s))?;
        }
        if let Some(a) = patch.assignee {
            task.assignee = Some(a);
        }
        if let Some(r) = patch.reviewer {
            task.reviewer = Some(r);
        }
        if let Some(p) = patch.priority {
            task.priority = p;
        }
        if let Some(t) = patch.title {
            task.title = t;
        }
        if let Some(b) = patch.body {
            task.body = Some(b);
        }
        if let Some(br) = patch.block_reason {
            task.block_reason = Some(br);
        }
        if let Some(res) = patch.result {
            task.result = Some(res);
        }
        if let Some(sum) = patch.summary {
            task.summary = Some(sum);
        }
        task.updated_at = now;

        let cql = format!(
            "UPDATE agent_memory.tasks SET \
             title={title}, body={body}, status='{status}', assignee={assignee}, \
             reviewer={reviewer}, priority={priority}, block_reason={block_reason}, \
             result={result}, summary={summary}, updated_at={now} \
             WHERE tenant_id={tenant} AND task_id='{tid}'",
            title = opt_str(&Some(task.title.clone())),
            body = opt_str(&task.body),
            status = task.status.as_str(),
            assignee = opt_str(&task.assignee),
            reviewer = opt_str(&task.reviewer),
            priority = task.priority,
            block_reason = opt_str(&task.block_reason),
            result = opt_str(&task.result),
            summary = opt_str(&task.summary),
            now = now,
            tenant = self.tenant_id,
            tid = esc(task_id),
        );

        cql_exec!(self.rt, &self.session, cql).context("update_task UPDATE")?;

        Ok(task)
    }

    /// Fetch a task with its links and recent comments.
    pub fn get_task(&self, task_id: &str) -> Result<TaskWithLinks> {
        let task = self.fetch_task_row(task_id)?;

        // Fetch links where this task is the source
        let links_cql = format!(
            "SELECT src_task_id, link_type, dst_task_id FROM agent_memory.task_links \
             WHERE tenant_id={tenant} AND src_task_id='{tid}'",
            tenant = self.tenant_id,
            tid = esc(task_id),
        );

        let links_result = cql_exec!(self.rt, &self.session, links_cql)
            .context("get_task: fetch links")?
            .into_legacy_result()
            .context("get_task: legacy result")?;

        let mut parents = Vec::new();
        let mut children = Vec::new();
        for row in links_result.rows_or_empty() {
            let (_, link_type, dst): (String, String, String) =
                row.into_typed().context("get_task: parse link row")?;
            if link_type == "parent" {
                parents.push(crate::types::TaskLink {
                    link_type: link_type.clone(),
                    task_id: dst,
                });
            } else {
                children.push(crate::types::TaskLink {
                    link_type: link_type.clone(),
                    task_id: dst,
                });
            }
        }

        // Recent comments (last 10 by timeuuid order)
        let comments_cql = format!(
            "SELECT author, body, created_at \
             FROM agent_memory.task_comments \
             WHERE tenant_id={tenant} AND task_id='{tid}' LIMIT 10",
            tenant = self.tenant_id,
            tid = esc(task_id),
        );

        let comments_result = cql_exec!(self.rt, &self.session, comments_cql)
            .context("get_task: fetch comments")?
            .into_legacy_result()
            .context("get_task: comments legacy result")?;

        let mut recent_comments = Vec::new();
        let mut comment_seq: u32 = 0;
        for row in comments_result.rows_or_empty() {
            let vals = row.columns;
            let author = match vals.first().and_then(|v| v.as_ref()) {
                Some(scylla::frame::response::result::CqlValue::Text(s)) => s.clone(),
                _ => String::new(),
            };
            let body = match vals.get(1).and_then(|v| v.as_ref()) {
                Some(scylla::frame::response::result::CqlValue::Text(s)) => s.clone(),
                _ => String::new(),
            };
            let created_at = match vals.get(2).and_then(|v| v.as_ref()) {
                Some(scylla::frame::response::result::CqlValue::BigInt(i)) => *i,
                _ => 0,
            };
            comment_seq += 1;
            recent_comments.push(Comment {
                comment_id: format!("c{}", comment_seq),
                author,
                body,
                created_at,
            });
        }

        Ok(TaskWithLinks {
            task,
            parents,
            children,
            recent_comments,
        })
    }

    /// List tasks, optionally filtered by status, assignee, and priority range.
    pub fn list_tasks(&self, filter: TaskFilter) -> Result<Vec<Task>> {
        let mut conditions = Vec::new();
        conditions.push(format!("tenant_id={}", self.tenant_id));

        if let Some(ref s) = filter.status {
            conditions.push(format!("status='{}'", esc(s)));
        }
        if let Some(ref a) = filter.assignee {
            conditions.push(format!("assignee='{}'", esc(a)));
        }
        if let Some(gte) = filter.priority_gte {
            conditions.push(format!("priority>={}", gte));
        }
        if let Some(lte) = filter.priority_lte {
            conditions.push(format!("priority<={}", lte));
        }

        let where_clause = conditions.join(" AND ");
        let limit = filter.limit.unwrap_or(100);
        let cql = format!(
            "SELECT task_id, title, body, status, assignee, reviewer, priority, \
             workspace_kind, workspace_path, created_by, block_reason, result, summary, \
             metadata, skills, related_entity_ids, created_at, updated_at \
             FROM agent_memory.tasks WHERE {} LIMIT {} ALLOW FILTERING",
            where_clause, limit
        );

        let result = cql_exec!(self.rt, &self.session, cql)
            .context("list_tasks SELECT")?
            .into_legacy_result()
            .context("list_tasks: legacy result")?;

        let mut tasks = Vec::new();
        for row in result.rows_or_empty() {
            if let Ok(task) = parse_task_row(row) {
                tasks.push(task);
            }
        }
        Ok(tasks)
    }

    /// Create a parent→child link (stored in both directions).
    pub fn link_tasks(&self, parent_id: &str, child_id: &str, link_type: &str) -> Result<()> {
        let now = now_ms();

        // parent side: src=parent, link_type=child, dst=child
        let cql1 = format!(
            "INSERT INTO agent_memory.task_links \
             (tenant_id, src_task_id, link_type, dst_task_id, created_at) \
             VALUES ({tenant}, '{src}', '{lt}', '{dst}', {now})",
            tenant = self.tenant_id,
            src = esc(parent_id),
            lt = esc(link_type),
            dst = esc(child_id),
            now = now,
        );
        // child side: src=child, link_type=parent, dst=parent
        let cql2 = format!(
            "INSERT INTO agent_memory.task_links \
             (tenant_id, src_task_id, link_type, dst_task_id, created_at) \
             VALUES ({tenant}, '{src}', 'parent', '{dst}', {now})",
            tenant = self.tenant_id,
            src = esc(child_id),
            dst = esc(parent_id),
            now = now,
        );

        cql_exec!(self.rt, &self.session, cql1).context("link_tasks INSERT parent side")?;
        cql_exec!(self.rt, &self.session, cql2).context("link_tasks INSERT child side")?;
        Ok(())
    }

    /// Remove the link between two tasks (both directions).
    pub fn unlink_tasks(&self, parent_id: &str, child_id: &str) -> Result<()> {
        let cql1 = format!(
            "DELETE FROM agent_memory.task_links \
             WHERE tenant_id={tenant} AND src_task_id='{src}' AND link_type='child' \
             AND dst_task_id='{dst}'",
            tenant = self.tenant_id,
            src = esc(parent_id),
            dst = esc(child_id),
        );
        let cql2 = format!(
            "DELETE FROM agent_memory.task_links \
             WHERE tenant_id={tenant} AND src_task_id='{src}' AND link_type='parent' \
             AND dst_task_id='{dst}'",
            tenant = self.tenant_id,
            src = esc(child_id),
            dst = esc(parent_id),
        );

        cql_exec!(self.rt, &self.session, cql1).context("unlink_tasks DELETE parent side")?;
        cql_exec!(self.rt, &self.session, cql2).context("unlink_tasks DELETE child side")?;
        Ok(())
    }

    /// Add a comment to a task.
    pub fn add_comment(&self, task_id: &str, author: &str, body: &str) -> Result<Comment> {
        let now = now_ms();
        let cql = format!(
            "INSERT INTO agent_memory.task_comments \
             (tenant_id, task_id, comment_id, author, body, created_at) \
             VALUES ({tenant}, '{tid}', now(), '{author}', '{body}', {now})",
            tenant = self.tenant_id,
            tid = esc(task_id),
            author = esc(author),
            body = esc(body),
            now = now,
        );

        cql_exec!(self.rt, &self.session, cql).context("add_comment INSERT")?;

        Ok(Comment {
            comment_id: format!("ts:{}", now),
            author: author.to_string(),
            body: body.to_string(),
            created_at: now,
        })
    }

    /// Return all non-archived tasks grouped into a kanban board.
    pub fn board(&self) -> Result<KanbanBoard> {
        let cql = format!(
            "SELECT task_id, title, body, status, assignee, reviewer, priority, \
             workspace_kind, workspace_path, created_by, block_reason, result, summary, \
             metadata, skills, related_entity_ids, created_at, updated_at \
             FROM agent_memory.tasks \
             WHERE tenant_id={tenant} \
             LIMIT 500 ALLOW FILTERING",
            tenant = self.tenant_id,
        );

        let result = cql_exec!(self.rt, &self.session, cql)
            .context("board SELECT")?
            .into_legacy_result()
            .context("board: legacy result")?;

        let mut triage = Vec::new();
        let mut ready = Vec::new();
        let mut in_progress = Vec::new();
        let mut blocked = Vec::new();
        let mut complete = Vec::new();

        for row in result.rows_or_empty() {
            if let Ok(task) = parse_task_row(row) {
                match task.status {
                    TaskStatus::Triage => triage.push(task),
                    TaskStatus::Ready => ready.push(task),
                    TaskStatus::InProgress => in_progress.push(task),
                    TaskStatus::Blocked => blocked.push(task),
                    TaskStatus::Complete => complete.push(task),
                    TaskStatus::Archived => {}
                }
            }
        }

        Ok(KanbanBoard {
            columns: KanbanColumns {
                triage,
                ready,
                in_progress,
                blocked,
                complete,
            },
        })
    }

    // -----------------------------------------------------------------------
    // Internals
    // -----------------------------------------------------------------------

    fn fetch_task_row(&self, task_id: &str) -> Result<Task> {
        let cql = format!(
            "SELECT task_id, title, body, status, assignee, reviewer, priority, \
             workspace_kind, workspace_path, created_by, block_reason, result, summary, \
             metadata, skills, related_entity_ids, created_at, updated_at \
             FROM agent_memory.tasks \
             WHERE tenant_id={tenant} AND task_id='{tid}'",
            tenant = self.tenant_id,
            tid = esc(task_id),
        );

        let result = cql_exec!(self.rt, &self.session, cql)
            .context("fetch_task_row SELECT")?
            .into_legacy_result()
            .context("fetch_task_row: legacy result")?;

        let rows = result.rows().context("fetch_task_row: expected rows")?;
        let row = rows
            .into_iter()
            .next()
            .ok_or_else(|| anyhow!("Task not found: {}", task_id))?;
        parse_task_row(row)
    }
}

// ---------------------------------------------------------------------------
// Row parsing
// ---------------------------------------------------------------------------

/// Parse a CQL row into a `Task`.
///
/// Column order must match SELECT list in `fetch_task_row` / `list_tasks` / `board`.
fn parse_task_row(row: scylla::frame::response::result::Row) -> Result<Task> {
    use scylla::frame::response::result::CqlValue;

    let mut cols = row.columns.into_iter();

    let task_id = col_str(cols.next(), "task_id")?;
    let title = col_str(cols.next(), "title")?;
    let body = col_opt_str(cols.next());
    let status_str = col_str(cols.next(), "status")?;
    let status = TaskStatus::parse(&status_str).unwrap_or(TaskStatus::Triage);
    let assignee = col_opt_str(cols.next());
    let reviewer = col_opt_str(cols.next());
    let priority = match cols.next().flatten() {
        Some(CqlValue::Int(i)) => i,
        _ => 50,
    };
    let workspace_kind = col_opt_str(cols.next());
    let workspace_path = col_opt_str(cols.next());
    let created_by = col_str(cols.next(), "created_by").unwrap_or_else(|_| "agent".to_string());
    let block_reason = col_opt_str(cols.next());
    let result_val = col_opt_str(cols.next());
    let summary = col_opt_str(cols.next());
    let metadata = col_opt_str(cols.next())
        .map(|s| serde_json::from_str(&s).unwrap_or(serde_json::Value::String(s)));
    let _skills = cols.next(); // set<text> — skipped for now
    let _related = cols.next(); // set<uuid> — skipped for now
    let created_at = match cols.next().flatten() {
        Some(CqlValue::BigInt(i)) => i,
        _ => 0,
    };
    let updated_at = match cols.next().flatten() {
        Some(CqlValue::BigInt(i)) => i,
        _ => 0,
    };

    Ok(Task {
        task_id,
        title,
        body,
        status,
        assignee,
        reviewer,
        priority,
        workspace_kind,
        workspace_path,
        created_by,
        block_reason,
        result: result_val,
        summary,
        metadata,
        skills: Vec::new(),
        related_entity_ids: Vec::new(),
        created_at,
        updated_at,
    })
}

fn col_str(
    v: Option<Option<scylla::frame::response::result::CqlValue>>,
    name: &str,
) -> Result<String> {
    match v.flatten() {
        Some(scylla::frame::response::result::CqlValue::Text(s)) => Ok(s),
        Some(scylla::frame::response::result::CqlValue::Ascii(s)) => Ok(s),
        _ => Err(anyhow!("Missing or non-text column: {}", name)),
    }
}

fn col_opt_str(v: Option<Option<scylla::frame::response::result::CqlValue>>) -> Option<String> {
    match v.flatten() {
        Some(scylla::frame::response::result::CqlValue::Text(s)) => Some(s),
        Some(scylla::frame::response::result::CqlValue::Ascii(s)) => Some(s),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Format a slice of strings as a CQL set literal: {'a', 'b'}.
fn format_set_text(items: &[String]) -> String {
    if items.is_empty() {
        return "{}".to_string();
    }
    let inner: Vec<String> = items.iter().map(|s| format!("'{}'", esc(s))).collect();
    format!("{{{}}}", inner.join(", "))
}
