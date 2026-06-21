//! CQL schema for the task tables (idempotent CREATE TABLE IF NOT EXISTS).

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
