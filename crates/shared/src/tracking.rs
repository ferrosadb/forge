//! Token savings tracking via SQLite.
//!
//! Records input/output byte counts per command invocation,
//! estimates token savings, and provides analytics queries.

use anyhow::Result;
use rusqlite::Connection;
use serde::Serialize;
use std::path::PathBuf;

/// Estimated tokens per byte (chars/4 approximation).
const BYTES_PER_TOKEN: f64 = 4.0;

/// Get the default database path (~/.local/share/forge/history.db).
pub fn default_db_path() -> Result<PathBuf> {
    let data_dir = dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("forge");
    std::fs::create_dir_all(&data_dir)?;
    Ok(data_dir.join("history.db"))
}

/// Run forward-only migrations on the tracking database.
/// Each migration checks whether the schema change already exists
/// before applying, so it is safe to run repeatedly.
fn migrate(conn: &Connection) -> Result<()> {
    // v0.5.0: add tool_version column to filter_log
    let has_tool_version: bool = conn
        .prepare("SELECT 1 FROM pragma_table_info('filter_log') WHERE name = 'tool_version'")?
        .exists([])?;
    if !has_tool_version {
        conn.execute_batch("ALTER TABLE filter_log ADD COLUMN tool_version TEXT")?;
    }
    Ok(())
}

/// Open or create the tracking database.
pub fn open_db(path: &std::path::Path) -> Result<Connection> {
    let conn = Connection::open(path)?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS command_log (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            timestamp TEXT NOT NULL DEFAULT (datetime('now')),
            command TEXT NOT NULL,
            subcommand TEXT NOT NULL,
            input_bytes INTEGER NOT NULL,
            output_bytes INTEGER NOT NULL,
            exit_code INTEGER NOT NULL DEFAULT 0
        );
        CREATE INDEX IF NOT EXISTS idx_command_log_ts ON command_log(timestamp);

        CREATE TABLE IF NOT EXISTS filter_log (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            timestamp TEXT NOT NULL DEFAULT (datetime('now')),
            command TEXT NOT NULL,
            filter_name TEXT NOT NULL,
            invocation_mode TEXT NOT NULL DEFAULT 'unknown',
            project_dir TEXT,
            input_bytes INTEGER NOT NULL,
            output_bytes INTEGER NOT NULL,
            compression_ratio REAL NOT NULL DEFAULT 0.0,
            duration_ms INTEGER NOT NULL DEFAULT 0,
            filter_success INTEGER NOT NULL DEFAULT 1,
            error_message TEXT,
            exit_code INTEGER NOT NULL DEFAULT 0,
            tool_version TEXT
        );
        CREATE INDEX IF NOT EXISTS idx_filter_log_ts ON filter_log(timestamp);
        CREATE INDEX IF NOT EXISTS idx_filter_log_filter ON filter_log(filter_name);
        CREATE INDEX IF NOT EXISTS idx_filter_log_project ON filter_log(project_dir);

        CREATE TABLE IF NOT EXISTS context_log (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            session_id TEXT NOT NULL,
            timestamp TEXT NOT NULL DEFAULT (datetime('now')),
            file_path TEXT NOT NULL,
            symbol TEXT,
            start_line INTEGER,
            end_line INTEGER,
            byte_count INTEGER NOT NULL DEFAULT 0
        );
        CREATE INDEX IF NOT EXISTS idx_context_session ON context_log(session_id);",
    )?;
    migrate(&conn)?;
    Ok(conn)
}

/// Record a command execution with its input/output sizes.
pub fn record(
    conn: &Connection,
    command: &str,
    subcommand: &str,
    input_bytes: usize,
    output_bytes: usize,
    exit_code: i32,
) -> Result<()> {
    conn.execute(
        "INSERT INTO command_log (command, subcommand, input_bytes, output_bytes, exit_code)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        rusqlite::params![command, subcommand, input_bytes, output_bytes, exit_code],
    )?;
    Ok(())
}

/// Invocation mode for filter_log entries.
#[derive(Debug, Clone, Copy, Serialize)]
pub enum InvocationMode {
    /// Called via `frg run <cmd>`
    Run,
    /// Called via Claude Code PostToolUse hook
    Hook,
    /// Called via stdin pipe (e.g., `cmd | frg test-summary`)
    Pipe,
    /// Called via MCP server tool invocation
    Mcp,
}

impl std::fmt::Display for InvocationMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Run => write!(f, "run"),
            Self::Hook => write!(f, "hook"),
            Self::Pipe => write!(f, "pipe"),
            Self::Mcp => write!(f, "mcp"),
        }
    }
}

/// Parameters for recording a filter invocation.
pub struct FilterRecord<'a> {
    pub command: &'a str,
    pub filter_name: &'a str,
    pub mode: InvocationMode,
    pub project_dir: Option<&'a str>,
    pub input_bytes: usize,
    pub output_bytes: usize,
    pub duration_ms: u64,
    pub filter_success: bool,
    pub error_message: Option<&'a str>,
    pub exit_code: i32,
    pub tool_version: &'a str,
}

/// Record a detailed filter invocation for post-analysis.
pub fn record_filter(conn: &Connection, rec: &FilterRecord) -> Result<()> {
    let ratio = if rec.input_bytes > 0 {
        rec.output_bytes as f64 / rec.input_bytes as f64
    } else {
        0.0
    };
    conn.execute(
        "INSERT INTO filter_log (command, filter_name, invocation_mode, project_dir,
         input_bytes, output_bytes, compression_ratio, duration_ms,
         filter_success, error_message, exit_code, tool_version)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        rusqlite::params![
            rec.command,
            rec.filter_name,
            rec.mode.to_string(),
            rec.project_dir,
            rec.input_bytes,
            rec.output_bytes,
            ratio,
            rec.duration_ms,
            rec.filter_success as i32,
            rec.error_message,
            rec.exit_code,
            rec.tool_version,
        ],
    )?;
    Ok(())
}

/// A record of context (file/symbol) shown to the LLM in a session.
#[derive(Debug, Serialize)]
pub struct ContextEntry {
    pub session_id: String,
    pub file_path: String,
    pub symbol: Option<String>,
    pub start_line: Option<usize>,
    pub end_line: Option<usize>,
    pub byte_count: usize,
    pub timestamp: String,
}

/// Record that the LLM has seen a file/symbol in a given session.
pub fn record_context(
    conn: &Connection,
    session_id: &str,
    file_path: &str,
    symbol: Option<&str>,
    start_line: Option<usize>,
    end_line: Option<usize>,
    byte_count: usize,
) -> Result<()> {
    conn.execute(
        "INSERT INTO context_log (session_id, file_path, symbol, start_line, end_line, byte_count)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        rusqlite::params![session_id, file_path, symbol, start_line, end_line, byte_count],
    )?;
    Ok(())
}

/// Query all context entries for a session, ordered by most recent first.
pub fn query_context(conn: &Connection, session_id: &str) -> Result<Vec<ContextEntry>> {
    let mut stmt = conn.prepare(
        "SELECT session_id, file_path, symbol, start_line, end_line, byte_count, timestamp
         FROM context_log
         WHERE session_id = ?1
         ORDER BY id DESC",
    )?;
    let entries = stmt
        .query_map(rusqlite::params![session_id], |row| {
            Ok(ContextEntry {
                session_id: row.get(0)?,
                file_path: row.get(1)?,
                symbol: row.get(2)?,
                start_line: row.get::<_, Option<i64>>(3)?.map(|v| v as usize),
                end_line: row.get::<_, Option<i64>>(4)?.map(|v| v as usize),
                byte_count: row.get::<_, i64>(5)? as usize,
                timestamp: row.get(6)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(entries)
}

/// Clear all context entries for a session.
pub fn clear_context(conn: &Connection, session_id: &str) -> Result<()> {
    conn.execute(
        "DELETE FROM context_log WHERE session_id = ?1",
        rusqlite::params![session_id],
    )?;
    Ok(())
}

/// Delete all analytics data (filter_log and command_log).
/// Returns the number of rows deleted from each table.
pub fn clear_analytics(conn: &Connection) -> Result<(usize, usize)> {
    let filter_rows = conn.execute("DELETE FROM filter_log", [])?;
    let command_rows = conn.execute("DELETE FROM command_log", [])?;
    Ok((filter_rows, command_rows))
}

/// Summary of filter effectiveness for post-analysis.
#[derive(Debug, Serialize)]
pub struct FilterAnalytics {
    pub total_invocations: u64,
    pub by_filter: Vec<FilterStats>,
    pub by_mode: Vec<ModeStats>,
    pub by_project: Vec<ProjectStats>,
    pub failures: Vec<FilterFailure>,
}

#[derive(Debug, Serialize)]
pub struct FilterStats {
    pub filter_name: String,
    pub count: u64,
    pub avg_compression_ratio: f64,
    pub total_input_tokens: i64,
    /// Positive = tokens saved, negative = filter inflated output.
    pub total_saved_tokens: i64,
    pub avg_duration_ms: f64,
    pub failure_count: u64,
}

#[derive(Debug, Serialize)]
pub struct ModeStats {
    pub mode: String,
    pub count: u64,
    pub total_saved_tokens: u64,
}

#[derive(Debug, Serialize)]
pub struct ProjectStats {
    pub project_dir: String,
    pub count: u64,
    pub total_saved_tokens: u64,
    pub top_filter: String,
}

#[derive(Debug, Serialize)]
pub struct FilterFailure {
    pub timestamp: String,
    pub command: String,
    pub filter_name: String,
    pub error_message: String,
    pub tool_version: Option<String>,
}

/// Query detailed filter analytics.
pub fn query_filter_analytics(conn: &Connection) -> Result<FilterAnalytics> {
    // Total
    let total_invocations: u64 =
        conn.query_row("SELECT COUNT(*) FROM filter_log", [], |row| row.get(0))?;

    // By filter
    let mut stmt = conn.prepare(
        "SELECT filter_name, COUNT(*),
                SUM(input_bytes), SUM(output_bytes),
                AVG(duration_ms),
                SUM(CASE WHEN filter_success = 0 THEN 1 ELSE 0 END)
         FROM filter_log GROUP BY filter_name
         ORDER BY SUM(input_bytes) - SUM(output_bytes) DESC",
    )?;
    let by_filter: Vec<FilterStats> = stmt
        .query_map([], |row| {
            let in_b: i64 = row.get(2)?;
            let out_b: i64 = row.get(3)?;
            let in_t = (in_b as f64 / BYTES_PER_TOKEN) as i64;
            let out_t = (out_b as f64 / BYTES_PER_TOKEN) as i64;
            // Compute ratio from aggregate totals (not per-invocation AVG)
            // so that RATIO and SAVED_TK are always consistent.
            let agg_ratio = if in_b > 0 {
                out_b as f64 / in_b as f64
            } else {
                0.0
            };
            Ok(FilterStats {
                filter_name: row.get(0)?,
                count: row.get(1)?,
                avg_compression_ratio: agg_ratio,
                total_input_tokens: in_t,
                total_saved_tokens: in_t - out_t,
                avg_duration_ms: row.get(4)?,
                failure_count: row.get(5)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;

    // By invocation mode
    let mut stmt = conn.prepare(
        "SELECT invocation_mode, COUNT(*),
                SUM(input_bytes) - SUM(output_bytes)
         FROM filter_log GROUP BY invocation_mode",
    )?;
    let by_mode: Vec<ModeStats> = stmt
        .query_map([], |row| {
            let saved_bytes: i64 = row.get(2)?;
            Ok(ModeStats {
                mode: row.get(0)?,
                count: row.get(1)?,
                total_saved_tokens: (saved_bytes.max(0) as f64 / BYTES_PER_TOKEN) as u64,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;

    // By project (top 10)
    let mut stmt = conn.prepare(
        "SELECT COALESCE(project_dir, 'unknown'), COUNT(*),
                SUM(input_bytes) - SUM(output_bytes),
                (SELECT filter_name FROM filter_log f2
                 WHERE f2.project_dir = filter_log.project_dir
                 GROUP BY filter_name ORDER BY COUNT(*) DESC LIMIT 1)
         FROM filter_log
         WHERE project_dir IS NOT NULL
         GROUP BY project_dir
         ORDER BY COUNT(*) DESC LIMIT 10",
    )?;
    let by_project: Vec<ProjectStats> = stmt
        .query_map([], |row| {
            let saved_bytes: i64 = row.get(2)?;
            Ok(ProjectStats {
                project_dir: row.get(0)?,
                count: row.get(1)?,
                total_saved_tokens: (saved_bytes.max(0) as f64 / BYTES_PER_TOKEN) as u64,
                top_filter: row.get::<_, Option<String>>(3)?.unwrap_or_default(),
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;

    // Recent failures (last 20)
    let mut stmt = conn.prepare(
        "SELECT timestamp, command, filter_name, COALESCE(error_message, ''),
                tool_version
         FROM filter_log WHERE filter_success = 0
         ORDER BY timestamp DESC LIMIT 20",
    )?;
    let failures: Vec<FilterFailure> = stmt
        .query_map([], |row| {
            Ok(FilterFailure {
                timestamp: row.get(0)?,
                command: row.get(1)?,
                filter_name: row.get(2)?,
                error_message: row.get(3)?,
                tool_version: row.get(4)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;

    Ok(FilterAnalytics {
        total_invocations,
        by_filter,
        by_mode,
        by_project,
        failures,
    })
}

#[derive(Debug, Serialize)]
pub struct GainSummary {
    pub total_commands: u64,
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    pub total_saved_tokens: u64,
    pub savings_pct: f64,
    pub by_subcommand: Vec<SubcommandGain>,
    pub daily: Vec<DailyGain>,
}

#[derive(Debug, Serialize)]
pub struct SubcommandGain {
    pub subcommand: String,
    pub count: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub saved_tokens: u64,
    pub savings_pct: f64,
}

#[derive(Debug, Serialize)]
pub struct DailyGain {
    pub date: String,
    pub commands: u64,
    pub saved_tokens: u64,
}

/// Query cumulative token savings from both command_log (run mode)
/// and filter_log (hook/pipe mode).
pub fn query_gains(conn: &Connection) -> Result<GainSummary> {
    // Combined totals from both tables
    let mut stmt = conn.prepare(
        "SELECT COUNT(*), COALESCE(SUM(input_bytes), 0), COALESCE(SUM(output_bytes), 0)
         FROM (
             SELECT input_bytes, output_bytes FROM command_log
             UNION ALL
             SELECT input_bytes, output_bytes FROM filter_log
         )",
    )?;
    let (total_commands, total_in_bytes, total_out_bytes): (u64, u64, u64) =
        stmt.query_row([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?;

    let total_input_tokens = (total_in_bytes as f64 / BYTES_PER_TOKEN) as u64;
    let total_output_tokens = (total_out_bytes as f64 / BYTES_PER_TOKEN) as u64;
    let total_saved_tokens = total_input_tokens.saturating_sub(total_output_tokens);
    let savings_pct = if total_input_tokens > 0 {
        (total_saved_tokens as f64 / total_input_tokens as f64) * 100.0
    } else {
        0.0
    };

    // By source: command_log subcommands + filter_log filter_names
    let mut stmt = conn.prepare(
        "SELECT source, COUNT(*), SUM(input_bytes), SUM(output_bytes)
         FROM (
             SELECT subcommand AS source, input_bytes, output_bytes FROM command_log
             UNION ALL
             SELECT filter_name AS source, input_bytes, output_bytes FROM filter_log
         )
         GROUP BY source ORDER BY SUM(input_bytes) - SUM(output_bytes) DESC",
    )?;
    let by_subcommand: Vec<SubcommandGain> = stmt
        .query_map([], |row| {
            let sub: String = row.get(0)?;
            let count: u64 = row.get(1)?;
            let in_b: u64 = row.get(2)?;
            let out_b: u64 = row.get(3)?;
            let in_t = (in_b as f64 / BYTES_PER_TOKEN) as u64;
            let out_t = (out_b as f64 / BYTES_PER_TOKEN) as u64;
            let saved = in_t.saturating_sub(out_t);
            let pct = if in_t > 0 {
                (saved as f64 / in_t as f64) * 100.0
            } else {
                0.0
            };
            Ok(SubcommandGain {
                subcommand: sub,
                count,
                input_tokens: in_t,
                output_tokens: out_t,
                saved_tokens: saved,
                savings_pct: pct,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;

    // Daily breakdown (last 30 days) from both tables
    let mut stmt = conn.prepare(
        "SELECT d, SUM(cnt), SUM(saved)
         FROM (
             SELECT date(timestamp) AS d, COUNT(*) AS cnt,
                    SUM(input_bytes) - SUM(output_bytes) AS saved
             FROM command_log
             WHERE timestamp > datetime('now', '-30 days')
             GROUP BY date(timestamp)
             UNION ALL
             SELECT date(timestamp) AS d, COUNT(*) AS cnt,
                    SUM(input_bytes) - SUM(output_bytes) AS saved
             FROM filter_log
             WHERE timestamp > datetime('now', '-30 days')
             GROUP BY date(timestamp)
         )
         GROUP BY d ORDER BY d DESC LIMIT 30",
    )?;
    let daily: Vec<DailyGain> = stmt
        .query_map([], |row| {
            let date: String = row.get(0)?;
            let commands: u64 = row.get(1)?;
            let saved_bytes: i64 = row.get(2)?;
            Ok(DailyGain {
                date,
                commands,
                saved_tokens: (saved_bytes.max(0) as f64 / BYTES_PER_TOKEN) as u64,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;

    Ok(GainSummary {
        total_commands,
        total_input_tokens,
        total_output_tokens,
        total_saved_tokens,
        savings_pct,
        by_subcommand,
        daily,
    })
}

/// Format gains as a human-readable report.
pub fn format_gains_report(gains: &GainSummary) -> String {
    let mut out = String::new();
    out.push_str(&format!("Token Savings Summary\n{}\n", "=".repeat(50)));
    out.push_str(&format!("Total commands:     {}\n", gains.total_commands));
    out.push_str(&format!(
        "Input tokens:       {:>10}\n",
        gains.total_input_tokens
    ));
    out.push_str(&format!(
        "Output tokens:      {:>10}\n",
        gains.total_output_tokens
    ));
    out.push_str(&format!(
        "Tokens saved:       {:>10} ({:.1}%)\n\n",
        gains.total_saved_tokens, gains.savings_pct
    ));

    if !gains.by_subcommand.is_empty() {
        out.push_str("By Subcommand:\n");
        out.push_str(&format!(
            "  {:<20} {:>6} {:>10} {:>10} {:>7}\n",
            "COMMAND", "COUNT", "INPUT", "SAVED", "PCT"
        ));
        for s in &gains.by_subcommand {
            out.push_str(&format!(
                "  {:<20} {:>6} {:>10} {:>10} {:>6.1}%\n",
                s.subcommand, s.count, s.input_tokens, s.saved_tokens, s.savings_pct
            ));
        }
    }

    if !gains.daily.is_empty() {
        out.push_str("\nLast 7 Days:\n");
        for d in gains.daily.iter().take(7) {
            let bar_len = (d.saved_tokens as f64 / 1000.0).min(40.0) as usize;
            let bar: String = "█".repeat(bar_len);
            out.push_str(&format!(
                "  {} {:>4} cmds {:>8} saved  {}\n",
                d.date, d.commands, d.saved_tokens, bar
            ));
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open_db_in_memory(conn: &Connection) -> Result<()> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS command_log (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                timestamp TEXT NOT NULL DEFAULT (datetime('now')),
                command TEXT NOT NULL,
                subcommand TEXT NOT NULL,
                input_bytes INTEGER NOT NULL,
                output_bytes INTEGER NOT NULL,
                exit_code INTEGER NOT NULL DEFAULT 0
            );
            CREATE TABLE IF NOT EXISTS filter_log (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                timestamp TEXT NOT NULL DEFAULT (datetime('now')),
                command TEXT NOT NULL,
                filter_name TEXT NOT NULL,
                invocation_mode TEXT NOT NULL DEFAULT 'unknown',
                project_dir TEXT,
                input_bytes INTEGER NOT NULL,
                output_bytes INTEGER NOT NULL,
                compression_ratio REAL NOT NULL DEFAULT 0.0,
                duration_ms INTEGER NOT NULL DEFAULT 0,
                filter_success INTEGER NOT NULL DEFAULT 1,
                error_message TEXT,
                exit_code INTEGER NOT NULL DEFAULT 0,
                tool_version TEXT
            );
            CREATE TABLE IF NOT EXISTS context_log (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id TEXT NOT NULL,
                timestamp TEXT NOT NULL DEFAULT (datetime('now')),
                file_path TEXT NOT NULL,
                symbol TEXT,
                start_line INTEGER,
                end_line INTEGER,
                byte_count INTEGER NOT NULL DEFAULT 0
            );
            CREATE INDEX IF NOT EXISTS idx_context_session ON context_log(session_id);",
        )?;
        Ok(())
    }

    #[test]
    fn test_tracking_roundtrip() {
        let conn = Connection::open_in_memory().unwrap();
        open_db_in_memory(&conn).unwrap();

        record(&conn, "cargo test", "test-summary", 10000, 500, 0).unwrap();
        record(&conn, "ruff check .", "lint-dedup", 5000, 800, 0).unwrap();

        let gains = query_gains(&conn).unwrap();
        assert_eq!(gains.total_commands, 2);
        assert!(gains.savings_pct > 80.0);
        assert_eq!(gains.by_subcommand.len(), 2);
    }

    #[test]
    fn test_filter_log_roundtrip() {
        let conn = Connection::open_in_memory().unwrap();
        open_db_in_memory(&conn).unwrap();

        record_filter(
            &conn,
            &FilterRecord {
                command: "cargo test",
                filter_name: "test-summary",
                mode: InvocationMode::Run,
                project_dir: Some("/home/user/myproject"),
                input_bytes: 10000,
                output_bytes: 500,
                duration_ms: 12,
                filter_success: true,
                error_message: None,
                exit_code: 0,
                tool_version: "0.0.0-test",
            },
        )
        .unwrap();

        record_filter(
            &conn,
            &FilterRecord {
                command: "ruff check .",
                filter_name: "lint-dedup",
                mode: InvocationMode::Hook,
                project_dir: Some("/home/user/myproject"),
                input_bytes: 5000,
                output_bytes: 800,
                duration_ms: 8,
                filter_success: true,
                error_message: None,
                exit_code: 0,
                tool_version: "0.0.0-test",
            },
        )
        .unwrap();

        let analytics = query_filter_analytics(&conn).unwrap();
        assert_eq!(analytics.total_invocations, 2);
        assert_eq!(analytics.by_filter.len(), 2);
        assert_eq!(analytics.by_mode.len(), 2);
        assert_eq!(analytics.by_project.len(), 1);
        assert!(analytics.failures.is_empty());
    }

    #[test]
    fn test_filter_log_compression_ratio() {
        let conn = Connection::open_in_memory().unwrap();
        open_db_in_memory(&conn).unwrap();

        record_filter(
            &conn,
            &FilterRecord {
                command: "cargo test",
                filter_name: "test-summary",
                mode: InvocationMode::Pipe,
                project_dir: None,
                input_bytes: 10000,
                output_bytes: 500,
                duration_ms: 5,
                filter_success: true,
                error_message: None,
                exit_code: 0,
                tool_version: "0.0.0-test",
            },
        )
        .unwrap();

        let analytics = query_filter_analytics(&conn).unwrap();
        let stats = &analytics.by_filter[0];
        assert!(stats.avg_compression_ratio < 0.1); // 500/10000 = 0.05
        assert!(stats.total_saved_tokens > 0);
    }

    #[test]
    fn test_filter_log_failures_tracked() {
        let conn = Connection::open_in_memory().unwrap();
        open_db_in_memory(&conn).unwrap();

        record_filter(
            &conn,
            &FilterRecord {
                command: "cargo test",
                filter_name: "test-summary",
                mode: InvocationMode::Run,
                project_dir: Some("/home/user/proj"),
                input_bytes: 1000,
                output_bytes: 1000,
                duration_ms: 3,
                filter_success: false,
                error_message: Some("parse error: unexpected format"),
                exit_code: 1,
                tool_version: "0.0.0-test",
            },
        )
        .unwrap();

        let analytics = query_filter_analytics(&conn).unwrap();
        assert_eq!(analytics.failures.len(), 1);
        assert_eq!(
            analytics.failures[0].error_message,
            "parse error: unexpected format"
        );
        assert_eq!(analytics.by_filter[0].failure_count, 1);
    }

    #[test]
    fn test_invocation_mode_display() {
        assert_eq!(InvocationMode::Run.to_string(), "run");
        assert_eq!(InvocationMode::Hook.to_string(), "hook");
        assert_eq!(InvocationMode::Pipe.to_string(), "pipe");
        assert_eq!(InvocationMode::Mcp.to_string(), "mcp");
    }

    #[test]
    fn test_context_roundtrip() {
        let conn = Connection::open_in_memory().unwrap();
        open_db_in_memory(&conn).unwrap();

        record_context(
            &conn,
            "sess-1",
            "src/main.rs",
            Some("fn main"),
            Some(1),
            Some(50),
            2048,
        )
        .unwrap();

        let entries = query_context(&conn, "sess-1").unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].session_id, "sess-1");
        assert_eq!(entries[0].file_path, "src/main.rs");
        assert_eq!(entries[0].symbol.as_deref(), Some("fn main"));
        assert_eq!(entries[0].start_line, Some(1));
        assert_eq!(entries[0].end_line, Some(50));
        assert_eq!(entries[0].byte_count, 2048);
    }

    #[test]
    fn test_context_query_ordered_by_recency() {
        let conn = Connection::open_in_memory().unwrap();
        open_db_in_memory(&conn).unwrap();

        record_context(&conn, "sess-2", "src/a.rs", None, None, None, 100).unwrap();
        record_context(&conn, "sess-2", "src/b.rs", None, None, None, 200).unwrap();
        record_context(&conn, "sess-2", "src/c.rs", None, None, None, 300).unwrap();

        let entries = query_context(&conn, "sess-2").unwrap();
        assert_eq!(entries.len(), 3);
        // Most recent first (ORDER BY id DESC)
        assert_eq!(entries[0].file_path, "src/c.rs");
        assert_eq!(entries[1].file_path, "src/b.rs");
        assert_eq!(entries[2].file_path, "src/a.rs");
    }

    #[test]
    fn test_context_clear() {
        let conn = Connection::open_in_memory().unwrap();
        open_db_in_memory(&conn).unwrap();

        record_context(&conn, "sess-3", "src/x.rs", None, None, None, 500).unwrap();
        record_context(&conn, "sess-3", "src/y.rs", None, None, None, 600).unwrap();
        // Different session should not be affected
        record_context(&conn, "sess-other", "src/z.rs", None, None, None, 700).unwrap();

        clear_context(&conn, "sess-3").unwrap();

        let entries = query_context(&conn, "sess-3").unwrap();
        assert!(entries.is_empty());

        // Other session untouched
        let other = query_context(&conn, "sess-other").unwrap();
        assert_eq!(other.len(), 1);
        assert_eq!(other[0].file_path, "src/z.rs");
    }
}
