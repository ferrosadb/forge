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

use crate::schema::{
    BOARD_KEYSPACE, CREATE_TASKS_TABLE, CREATE_TASK_COMMENTS_TABLE, CREATE_TASK_LINKS_TABLE,
};
use crate::types::{
    Comment, CreateTaskRequest, KanbanBoard, KanbanColumns, Task, TaskFilter, TaskStatus,
    TaskWithLinks, UpdateTaskPatch,
};

/// Fixed tenant UUID for the single-user forge setup.
const TENANT_ID: &str = "00000000-0000-0000-0000-000000000001";

/// Applied when a row's `priority` is NULL (a value the writer never stores, but
/// which an externally-inserted row may carry). Mirrors `create_task`'s default.
const DEFAULT_PRIORITY: i32 = 50;

/// Applied when a row's `created_by` is NULL. A label, not a decision input, so
/// a documented placeholder is better than failing an otherwise-readable board.
const DEFAULT_CREATED_BY: &str = "agent";

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

/// How many rows a board or list read will pull before it stops.
///
/// The reads used to pass the caller's `limit` straight to CQL. The tasks table
/// is `PRIMARY KEY (tenant_id, task_id)`, so rows come back in task_id order --
/// effectively arbitrary -- and a LIMIT therefore took an ARBITRARY SLICE, not
/// the newest rows. With a few hundred open tasks the window stopped including
/// anything recent: tasks created hours earlier were absent from both
/// `task_board` and `task_list` while `task_get` returned them fine. An agent
/// that captured a deferral and read the board back could not see its own write.
///
/// CQL cannot fix this with ORDER BY: `created_at` is not a clustering column, so
/// ordering has to happen after the fetch, which means fetching enough to order
/// meaningfully. This is that bound -- high enough to cover the whole board in
/// practice, finite so a runaway table cannot be read into memory unbounded. When
/// it is reached the caller is TOLD, rather than being handed a silent window.
const MAX_FETCH_ROWS: usize = 10_000;

/// Newest first, with task_id breaking ties so the order is total and a page
/// boundary cannot shuffle between reads.
pub(crate) fn sort_newest_first(tasks: &mut [Task]) {
    tasks.sort_by(|a, b| {
        b.created_at
            .cmp(&a.created_at)
            .then_with(|| a.task_id.cmp(&b.task_id))
    });
}

/// A read that may have been cut short, and says so.
#[derive(Debug, Clone)]
pub struct FetchedTasks {
    pub tasks: Vec<Task>,
    /// Rows matching before the caller's limit/offset was applied.
    pub total: usize,
    /// True when MAX_FETCH_ROWS was hit, so `total` is a floor rather than the
    /// count. A capped read that reports itself is usable; one that does not is
    /// worse than an error.
    pub truncated: bool,
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

        anyhow::ensure!(
            !cql_hosts.is_empty(),
            "no CQL contact point configured for the task board -- set --cql-host, \
             FORGE_CQL_HOST, or cql_host in .forge/config.toml / ~/.config/forge.toml"
        );
        let hosts = cql_hosts.to_vec();
        // Every failure below names the contact points actually tried. Without
        // it the operator is told "connect to CQL: Connection refused" and has
        // no way to know WHICH host that was -- the flag, the environment
        // variable, the project config or the global config may each have
        // supplied it.
        let contacted = hosts.join(", ");
        let session: scylla::Session = rt
            .block_on(async {
                scylla::SessionBuilder::new()
                    .known_nodes(&hosts)
                    .user("ferrosa_admin", "ferrosa_admin")
                    .build()
                    .await
            })
            .with_context(|| {
                format!("cannot reach the task board at {contacted} (CQL contact points tried)")
            })?;

        let session = Arc::new(session);
        let store = Self {
            rt,
            session,
            tenant_id: tenant_id.unwrap_or(TENANT_ID).to_string(),
        };
        store.ensure_schema().with_context(|| {
            format!(
                "connected to {contacted}, but the task board keyspace '{BOARD_KEYSPACE}' is not \
                 usable there -- create the keyspace, or point the board at the database that \
                 holds it"
            )
        })?;
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
                .with_context(|| format!("ensure_schema: {}", first_line(stmt)))?;
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

    /// Fetch a task with its links and recent comments. Errors when no such
    /// task exists; see [`TaskStore::find_task`] when absence is a legitimate
    /// answer rather than a failure.
    pub fn get_task(&self, task_id: &str) -> Result<TaskWithLinks> {
        self.find_task(task_id)?
            .ok_or_else(|| anyhow!("Task not found: {}", task_id))
    }

    /// Fetch a task, distinguishing "no such task" (`Ok(None)`) from "I could
    /// not read the board" (`Err`).
    ///
    /// `get_task(..).ok()` collapsed both into `None`, so a caller asking
    /// "does this task already exist?" during an outage was told "no" and acted
    /// on it -- creating a duplicate, or moving a finished task backwards
    /// because its protected status could not be read.
    pub fn find_task(&self, task_id: &str) -> Result<Option<TaskWithLinks>> {
        let Some(task) = self.fetch_task_row(task_id)? else {
            return Ok(None);
        };

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
        let link_rows = links_result
            .rows()
            .context("get_task: expected link rows")?;
        for row in link_rows {
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
        let comment_rows = comments_result
            .rows()
            .context("get_task: expected comment rows")?;
        for row in comment_rows {
            // A comment whose columns do not decode is schema drift, not an
            // empty comment: blanking the author and body used to publish a
            // row that says nothing and looks deliberate.
            let mut vals = row.columns.into_iter();
            let author = col_str(vals.next(), "task_comments.author")?;
            let body = col_str(vals.next(), "task_comments.body")?;
            let created_at = col_i64(vals.next(), "task_comments.created_at")?;
            comment_seq += 1;
            recent_comments.push(Comment {
                comment_id: format!("c{}", comment_seq),
                author,
                body,
                created_at,
            });
        }

        Ok(Some(TaskWithLinks {
            task,
            parents,
            children,
            recent_comments,
        }))
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
        // Fetch to the bound, THEN order, THEN apply the caller's limit. Passing
        // the limit to CQL took an arbitrary slice, because rows arrive in
        // task_id order and created_at is not a clustering column.
        let cql = format!(
            "SELECT task_id, title, body, status, assignee, reviewer, priority, \
             workspace_kind, workspace_path, created_by, block_reason, result, summary, \
             metadata, skills, related_entity_ids, created_at, updated_at \
             FROM agent_memory.tasks WHERE {} LIMIT {} ALLOW FILTERING",
            where_clause, MAX_FETCH_ROWS
        );

        let result = cql_exec!(self.rt, &self.session, cql)
            .context("list_tasks SELECT")?
            .into_legacy_result()
            .context("list_tasks: legacy result")?;

        let rows = result.rows().context("list_tasks: expected rows")?;
        let mut tasks = parse_task_rows(rows).context("list_tasks: parse rows")?;
        sort_newest_first(&mut tasks);
        tasks.truncate(limit);
        Ok(tasks)
    }

    /// As `list_tasks`, but reporting how many matched and whether the read was
    /// cut short, so a caller can tell a complete answer from a window.
    pub fn list_tasks_paged(&self, filter: TaskFilter, offset: usize) -> Result<FetchedTasks> {
        let limit = filter.limit.unwrap_or(100);
        // Ask for everything the bound allows; the window is applied here.
        let mut unbounded = filter;
        unbounded.limit = Some(MAX_FETCH_ROWS);
        let mut tasks = self.list_tasks(unbounded)?;
        let truncated = tasks.len() >= MAX_FETCH_ROWS;
        let total = tasks.len();
        let tasks = if offset >= tasks.len() {
            Vec::new()
        } else {
            tasks.split_off(offset).into_iter().take(limit).collect()
        };
        Ok(FetchedTasks {
            tasks,
            total,
            truncated,
        })
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
             LIMIT {limit} ALLOW FILTERING",
            tenant = self.tenant_id,
            limit = MAX_FETCH_ROWS,
        );

        let result = cql_exec!(self.rt, &self.session, cql)
            .context("board SELECT")?
            .into_legacy_result()
            .context("board: legacy result")?;

        let rows = result.rows().context("board: expected rows")?;
        let tasks = parse_task_rows(rows).context("board: parse rows")?;

        let mut triage = Vec::new();
        let mut ready = Vec::new();
        let mut in_progress = Vec::new();
        let mut blocked = Vec::new();
        let mut complete = Vec::new();

        for task in tasks {
            match task.status {
                TaskStatus::Triage => triage.push(task),
                TaskStatus::Ready => ready.push(task),
                TaskStatus::InProgress => in_progress.push(task),
                TaskStatus::Blocked => blocked.push(task),
                TaskStatus::Complete => complete.push(task),
                TaskStatus::Archived => {}
            }
        }

        // Newest first in every column. Without this the board's order was
        // task_id order -- arbitrary -- so the most recently captured work sat
        // wherever its id happened to fall.
        for column in [
            &mut triage,
            &mut ready,
            &mut in_progress,
            &mut blocked,
            &mut complete,
        ] {
            sort_newest_first(column);
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

    /// The task's own row, or `Ok(None)` when the board holds no such task.
    fn fetch_task_row(&self, task_id: &str) -> Result<Option<Task>> {
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
        match rows.into_iter().next() {
            None => Ok(None),
            Some(row) => parse_task_row(row)
                .with_context(|| format!("task {task_id}"))
                .map(Some),
        }
    }
}

// ---------------------------------------------------------------------------
// Row parsing
// ---------------------------------------------------------------------------

/// Parse every row of a board read, or fail the whole read.
///
/// Dropping the rows that would not parse (`if let Ok(task) = ...`) meant a
/// board that had drifted from this schema simply reported fewer tasks, with no
/// error and no count to compare against. That is the same failure as an empty
/// answer from a dead host, one row at a time: the caller cannot tell "this task
/// does not exist" from "I could not decode it".
fn parse_task_rows(rows: Vec<scylla::frame::response::result::Row>) -> Result<Vec<Task>> {
    rows.into_iter()
        .enumerate()
        .map(|(i, row)| parse_task_row(row).with_context(|| format!("row {i}")))
        .collect()
}

/// Parse a CQL row into a `Task`.
///
/// Column order must match SELECT list in `fetch_task_row` / `list_tasks` / `board`.
///
/// Strict on purpose. A NULL in a nullable column is *data* and gets the
/// documented default; a column that is missing or of the wrong CQL type is
/// schema drift, and is an error rather than a plausible-looking task.
fn parse_task_row(row: scylla::frame::response::result::Row) -> Result<Task> {
    let mut cols = row.columns.into_iter();

    let task_id = col_str(cols.next(), "task_id")?;
    let title = col_str(cols.next(), "title")?;
    let body = col_opt_str(cols.next());
    let status_str = col_str(cols.next(), "status")?;
    // An unrecognised status used to be silently filed under `triage`, which
    // put the task in the wrong kanban column and, for a `complete` task,
    // resurrected it as outstanding work.
    let status = TaskStatus::parse(&status_str).ok_or_else(|| {
        anyhow!(
            "task {task_id}: unknown status {status_str:?} -- the board schema is \
             ahead of this build of forge"
        )
    })?;
    let assignee = col_opt_str(cols.next());
    let reviewer = col_opt_str(cols.next());
    let priority = col_i32_or(cols.next(), "priority", DEFAULT_PRIORITY)?;
    let workspace_kind = col_opt_str(cols.next());
    let workspace_path = col_opt_str(cols.next());
    let created_by = col_opt_str_strict(cols.next(), "created_by")?
        .unwrap_or_else(|| DEFAULT_CREATED_BY.to_string());
    let block_reason = col_opt_str(cols.next());
    let result_val = col_opt_str(cols.next());
    let summary = col_opt_str(cols.next());
    let metadata = col_opt_str(cols.next())
        .map(|s| serde_json::from_str(&s).unwrap_or(serde_json::Value::String(s)));
    let _skills = cols.next(); // set<text> — skipped for now
    let _related = cols.next(); // set<uuid> — skipped for now
                                // Timestamps decide the newest-first order every read depends on. A row we
                                // cannot time would sort to the bottom and quietly fall out of any window.
    let created_at = col_i64(cols.next(), "created_at")?;
    let updated_at = col_i64(cols.next(), "updated_at")?;

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

/// `Ok(None)` for a CQL NULL, `Err` for a missing column or a non-text type.
/// [`col_opt_str`] cannot tell those apart, which is fine where the column is
/// genuinely optional and wrong where a decode failure would masquerade as one.
fn col_opt_str_strict(
    v: Option<Option<scylla::frame::response::result::CqlValue>>,
    name: &str,
) -> Result<Option<String>> {
    use scylla::frame::response::result::CqlValue;
    match v {
        None => Err(anyhow!("Missing column: {}", name)),
        Some(None) => Ok(None),
        Some(Some(CqlValue::Text(s) | CqlValue::Ascii(s))) => Ok(Some(s)),
        Some(Some(other)) => Err(anyhow!(
            "Column {} is not text: {:?}",
            name,
            std::mem::discriminant(&other)
        )),
    }
}

/// A CQL `int`, defaulting when NULL but failing on a missing or mistyped column.
fn col_i32_or(
    v: Option<Option<scylla::frame::response::result::CqlValue>>,
    name: &str,
    default: i32,
) -> Result<i32> {
    use scylla::frame::response::result::CqlValue;
    match v {
        None => Err(anyhow!("Missing column: {}", name)),
        Some(None) => Ok(default),
        Some(Some(CqlValue::Int(i))) => Ok(i),
        Some(Some(_)) => Err(anyhow!("Column {} is not an int", name)),
    }
}

/// A CQL `bigint`. Missing, NULL, or mistyped are all errors: every caller uses
/// these for ordering, and a fabricated 0 sorts the row out of sight.
fn col_i64(
    v: Option<Option<scylla::frame::response::result::CqlValue>>,
    name: &str,
) -> Result<i64> {
    use scylla::frame::response::result::CqlValue;
    match v.flatten() {
        Some(CqlValue::BigInt(i)) => Ok(i),
        Some(_) => Err(anyhow!("Column {} is not a bigint", name)),
        None => Err(anyhow!("Missing or null column: {}", name)),
    }
}

/// First non-blank line of a CQL statement, for error context. Slicing the raw
/// first 50 bytes panics on a multi-byte boundary and reads as noise.
fn first_line(stmt: &str) -> &str {
    stmt.trim().lines().next().unwrap_or(stmt).trim()
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

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use scylla::frame::response::result::{CqlValue, Row};

    /// A well-formed `tasks` row in the column order every SELECT uses.
    fn valid_row() -> Row {
        Row {
            columns: vec![
                Some(CqlValue::Text("t_00000001".into())), // task_id
                Some(CqlValue::Text("a title".into())),    // title
                Some(CqlValue::Text("a body".into())),     // body
                Some(CqlValue::Text("ready".into())),      // status
                None,                                      // assignee
                None,                                      // reviewer
                Some(CqlValue::Int(70)),                   // priority
                None,                                      // workspace_kind
                None,                                      // workspace_path
                Some(CqlValue::Text("agent".into())),      // created_by
                None,                                      // block_reason
                None,                                      // result
                None,                                      // summary
                None,                                      // metadata
                None,                                      // skills
                None,                                      // related_entity_ids
                Some(CqlValue::BigInt(1_700_000_000_000)), // created_at
                Some(CqlValue::BigInt(1_700_000_000_001)), // updated_at
            ],
        }
    }

    fn row_with(index: usize, value: Option<CqlValue>) -> Row {
        let mut row = valid_row();
        row.columns[index] = value;
        row
    }

    const STATUS: usize = 3;
    const PRIORITY: usize = 6;
    const CREATED_BY: usize = 9;
    const CREATED_AT: usize = 16;

    #[test]
    fn a_well_formed_row_parses() {
        let task = parse_task_row(valid_row()).expect("valid row");
        assert_eq!(task.task_id, "t_00000001");
        assert_eq!(task.status, TaskStatus::Ready);
        assert_eq!(task.priority, 70);
        assert_eq!(task.created_at, 1_700_000_000_000);
    }

    /// The board used to file an unrecognised status under `triage`. A task the
    /// cluster considers `complete` then reappeared as outstanding work, and a
    /// board written by a newer forge was silently mis-columned by an older one.
    #[test]
    fn an_unknown_status_is_an_error_not_a_task_filed_under_triage() {
        let err = parse_task_row(row_with(STATUS, Some(CqlValue::Text("in_review".into()))))
            .expect_err("an unknown status must not be guessed at");
        let rendered = format!("{err:#}");
        assert!(
            rendered.contains("in_review") && rendered.contains("t_00000001"),
            "the error should name the status and the task, got: {rendered}"
        );
    }

    #[test]
    fn a_mistyped_column_is_an_error_not_a_default_value() {
        // Schema drift, not data: `priority` is an int, and reading a text there
        // as 50 invents a value the board never stored.
        assert!(parse_task_row(row_with(PRIORITY, Some(CqlValue::Text("high".into())))).is_err());
        assert!(parse_task_row(row_with(CREATED_BY, Some(CqlValue::Int(1)))).is_err());
    }

    #[test]
    fn a_missing_timestamp_is_an_error_because_it_decides_the_read_order() {
        // created_at = 0 sorts to the bottom of every newest-first read, so a row
        // we cannot time silently drops out of any limited window.
        let err = parse_task_row(row_with(CREATED_AT, None))
            .expect_err("a row without a created_at must not be fabricated as epoch 0");
        assert!(format!("{err:#}").contains("created_at"));
    }

    #[test]
    fn a_short_row_is_an_error_not_a_partially_filled_task() {
        let mut row = valid_row();
        row.columns.truncate(4);
        assert!(parse_task_row(row).is_err());
    }

    #[test]
    fn a_null_in_a_nullable_column_keeps_its_documented_default() {
        // NULL is data, not a decode failure: the row is readable and the
        // documented default applies.
        let task = parse_task_row(row_with(PRIORITY, None)).expect("null priority is readable");
        assert_eq!(task.priority, DEFAULT_PRIORITY);
        let task = parse_task_row(row_with(CREATED_BY, None)).expect("null created_by is readable");
        assert_eq!(task.created_by, DEFAULT_CREATED_BY);
    }

    /// The board read used to be `if let Ok(task) = parse_task_row(row)`, so a
    /// row it could not decode simply did not appear. The caller got a shorter
    /// list and no error -- the same "I could not look" reported as "there is
    /// nothing", one task at a time.
    #[test]
    fn one_unreadable_row_fails_the_whole_read_instead_of_vanishing_from_it() {
        let rows = vec![
            valid_row(),
            row_with(STATUS, Some(CqlValue::Text("nonsense".into()))),
            valid_row(),
        ];
        let err = parse_task_rows(rows).expect_err("an undecodable row must fail the read");
        assert!(
            format!("{err:#}").contains("row 1"),
            "the error should say which row failed, got: {err:#}"
        );
    }

    #[test]
    fn all_readable_rows_are_returned() {
        let tasks = parse_task_rows(vec![valid_row(), valid_row()]).expect("both rows readable");
        assert_eq!(tasks.len(), 2);
    }

    #[test]
    fn first_line_of_a_statement_is_used_for_error_context() {
        // The old context sliced 50 raw bytes, which panics on a multi-byte
        // boundary and reads as truncated noise.
        assert_eq!(
            first_line(CREATE_TASKS_TABLE),
            "CREATE TABLE IF NOT EXISTS agent_memory.tasks ("
        );
    }
}
