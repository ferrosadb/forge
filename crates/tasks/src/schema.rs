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

/// Why `ensure_schema` could not complete.
///
/// The board used to wrap every schema failure in one message telling the
/// operator the keyspace was not usable and to create it. That advice is wrong
/// for every cause but one, and it is the cause that is *least* likely: a
/// cluster that answered the connection almost always has the keyspace. A
/// consensus timeout under load sent operators to create a keyspace holding
/// thousands of live rows.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum SchemaFailure {
    /// The keyspace or table genuinely is not there.
    MissingKeyspace,
    /// The cluster took the request and could not agree in time.
    Unavailable,
    /// The credentials connected but may not change schema.
    Unauthorized,
    /// Something else. Say so rather than guess.
    Unrecognised,
}

impl SchemaFailure {
    /// What the operator should actually do. Never advises creating something
    /// that the error did not say was missing.
    pub fn advice(self) -> &'static str {
        match self {
            Self::MissingKeyspace => {
                "create the keyspace, or point the board at the database that holds it"
            }
            Self::Unavailable => {
                "the cluster did not reach agreement in time -- it is reachable but loaded or \
                 degraded; retry, and check node health before changing any schema"
            }
            Self::Unauthorized => {
                "the board connected but is not allowed to read or change this schema -- check \
                 the role's permissions on the keyspace"
            }
            Self::Unrecognised => {
                "the cluster refused the schema check for a reason the board does not recognise; \
                 the error above is verbatim from the driver"
            }
        }
    }
}

/// Classify a driver error by what it actually says.
pub fn classify_schema_failure(error_text: &str) -> SchemaFailure {
    let text = error_text.to_lowercase();
    if text.contains("unauthorized")
        || text.contains("permission")
        || text.contains("not authorized")
        || text.contains("unauthorised")
    {
        return SchemaFailure::Unauthorized;
    }
    if text.contains("timeout")
        || text.contains("timed out")
        || text.contains("unavailable")
        || text.contains("not enough replicas")
        || text.contains("cannot achieve consistency")
    {
        return SchemaFailure::Unavailable;
    }
    if text.contains("keyspace")
        && (text.contains("does not exist")
            || text.contains("unknown")
            || text.contains("not found"))
    {
        return SchemaFailure::MissingKeyspace;
    }
    SchemaFailure::Unrecognised
}

/// The tables the board cannot run without.
pub const BOARD_TABLES: &[&str] = &["tasks", "task_links", "task_comments"];

/// Which of the board's tables are absent from `present`.
///
/// `ensure_schema` used to run three `CREATE TABLE IF NOT EXISTS` and every
/// `ALTER` on EVERY connect, including a read-only `task list`. Schema DDL goes
/// through cluster consensus, so a loaded cluster failed the whole board on a
/// statement whose answer was always "already exists". Asking
/// `system_schema.tables` first is a local read: the steady state issues no DDL,
/// and a genuinely fresh install still gets its schema.
pub fn missing_board_tables(present: &[String]) -> Vec<&'static str> {
    BOARD_TABLES
        .iter()
        .copied()
        .filter(|table| !present.iter().any(|have| have == table))
        .collect()
}

/// The column an `ALTER TABLE ... ADD <name> <type>` adds.
///
/// Returned so `ensure_schema` can ask whether the column is already there
/// instead of issuing the DDL and forgiving the failure. `None` means the
/// statement is not a plain single-column ADD, and the caller must refuse
/// rather than guess -- silently skipping an upgrade would break the next read.
pub fn added_column_name(stmt: &str) -> Option<&str> {
    let mut words = stmt.split_whitespace();
    while let Some(word) = words.next() {
        if word.eq_ignore_ascii_case("ADD") {
            return words
                .next()
                .filter(|name| name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_timeout_is_not_reported_as_a_missing_keyspace() {
        let failure = classify_schema_failure(
            "Request timeout: Timeout while waiting for a response from the cluster",
        );
        assert_eq!(failure, SchemaFailure::Unavailable);
        assert!(
            !failure
                .advice()
                .to_lowercase()
                .contains("create the keyspace"),
            "a timeout must not send the operator to create a keyspace that already exists: {}",
            failure.advice()
        );
    }

    #[test]
    fn a_genuinely_unknown_keyspace_still_advises_creating_it() {
        let failure = classify_schema_failure("Keyspace 'agent_memory' does not exist");
        assert_eq!(failure, SchemaFailure::MissingKeyspace);
        assert!(failure.advice().contains("create the keyspace"));
    }

    #[test]
    fn a_permission_failure_is_reported_as_permission_not_absence() {
        let failure =
            classify_schema_failure("User ferrosa_admin has no CREATE permission on keyspace");
        assert_eq!(failure, SchemaFailure::Unauthorized);
        assert!(!failure.advice().contains("create the keyspace"));
    }

    #[test]
    fn an_unrecognised_error_says_so_rather_than_guessing() {
        let failure = classify_schema_failure("something the driver has never said before");
        assert_eq!(failure, SchemaFailure::Unrecognised);
        assert!(!failure.advice().contains("create the keyspace"));
    }

    #[test]
    fn consistency_failures_are_unavailability_not_absence() {
        for text in [
            "Cannot achieve consistency level QUORUM",
            "Not enough replicas available for query",
            "Request timed out",
        ] {
            assert_eq!(
                classify_schema_failure(text),
                SchemaFailure::Unavailable,
                "{text}"
            );
        }
    }

    #[test]
    fn a_board_whose_tables_all_exist_needs_no_ddl_at_all() {
        let present = ["tasks", "task_links", "task_comments"].map(str::to_owned);
        assert_eq!(
            missing_board_tables(&present),
            Vec::<&str>::new(),
            "the steady state must not put schema DDL through consensus on every connect"
        );
    }

    #[test]
    fn a_fresh_install_reports_every_table() {
        assert_eq!(
            missing_board_tables(&[]),
            vec!["tasks", "task_links", "task_comments"]
        );
    }

    #[test]
    fn a_partial_schema_reports_only_what_is_absent() {
        let present = ["tasks".to_owned(), "task_comments".to_owned()];
        assert_eq!(missing_board_tables(&present), vec!["task_links"]);
    }

    #[test]
    fn unrelated_tables_in_the_shared_keyspace_are_ignored() {
        // agent_memory is shared with ferrosa-memory, which has 26 tables of
        // its own. None of them mean the board's schema is present.
        let present = ["entity_store", "document_chunks", "audit_log"].map(str::to_owned);
        assert_eq!(
            missing_board_tables(&present),
            vec!["tasks", "task_links", "task_comments"]
        );
    }

    #[test]
    fn the_added_column_name_is_read_out_of_the_alter_statement() {
        assert_eq!(
            added_column_name("ALTER TABLE agent_memory.tasks ADD origin text"),
            Some("origin")
        );
    }

    #[test]
    fn every_shipped_alter_statement_yields_a_column_name() {
        // If this fails, ensure_schema cannot tell whether the column is
        // already present and would fall back to issuing DDL every connect --
        // the exact behaviour this change removes.
        for stmt in ALTER_TASKS_ADD_COLUMNS {
            assert!(
                added_column_name(stmt).is_some(),
                "no column name parsed out of {stmt}"
            );
        }
    }

    #[test]
    fn a_statement_that_is_not_an_add_yields_nothing() {
        assert_eq!(
            added_column_name("ALTER TABLE agent_memory.tasks DROP origin"),
            None
        );
    }
}
