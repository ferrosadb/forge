//! CQL schema for the task tables (idempotent CREATE TABLE IF NOT EXISTS).

/// The keyspace the board lives in. Named in errors so "I connected but could
/// not read the board" says *which* keyspace was missing.
pub const BOARD_KEYSPACE: &str = "agent_memory";

pub const CREATE_TASKS_TABLE: &str = "
CREATE TABLE IF NOT EXISTS agent_memory.tasks (
    tenant_id uuid,
    task_id text,
    title text,
    body text,
    status text,
    assignee text,
    reviewer text,
    priority int,
    workspace_kind text,
    workspace_path text,
    created_by text,
    origin text,
    block_reason text,
    result text,
    summary text,
    metadata text,
    skills set<text>,
    related_entity_ids set<uuid>,
    created_at bigint,
    updated_at bigint,
    PRIMARY KEY (tenant_id, task_id)
)
";

pub const CREATE_TASK_LINKS_TABLE: &str = "
CREATE TABLE IF NOT EXISTS agent_memory.task_links (
    tenant_id uuid,
    src_task_id text,
    link_type text,
    dst_task_id text,
    created_at bigint,
    PRIMARY KEY (tenant_id, src_task_id, link_type, dst_task_id)
)
";

pub const CREATE_TASK_COMMENTS_TABLE: &str = "
CREATE TABLE IF NOT EXISTS agent_memory.task_comments (
    tenant_id uuid,
    task_id text,
    comment_id timeuuid,
    author text,
    body text,
    created_at bigint,
    PRIMARY KEY (tenant_id, task_id, comment_id)
) WITH CLUSTERING ORDER BY (comment_id ASC)
";

/// Columns added after the table first shipped.
///
/// `CREATE TABLE IF NOT EXISTS` is a no-op against a table that already
/// exists, so a new column reaches a fresh install and no existing one. Every
/// SELECT in the store names its columns, so the first read after an upgrade
/// would fail on a column the deployed table does not have -- and that read is
/// the whole board.
///
/// Each statement must be safe to run on every connect: an "already exists"
/// answer is the expected one after the first time.
pub const ALTER_TASKS_ADD_COLUMNS: &[&str] = &["ALTER TABLE agent_memory.tasks ADD origin text"];
