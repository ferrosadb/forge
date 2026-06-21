//! Compare two schema files (SQL / CQL / Cypher) and classify each difference
//! as breaking, minor, or patch.
//!
//! v1 is a tokenize-don't-parse approach: regex-scan `CREATE TABLE` blocks for
//! SQL/CQL, and do a label / relationship-type inventory for Cypher. No full
//! parser — false positives are accepted in exchange for zero dependencies.

use anyhow::Result;
use regex::Regex;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Dialect {
    Sql,
    Cql,
    Cypher,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Breaking,
    Minor,
    Patch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeKind {
    TableAdded,
    TableDropped,
    ColumnAdded,
    ColumnDropped,
    ColumnTypeChanged,
    NullabilityChanged,
    PrimaryKeyChanged,
    ClusteringKeyChanged,
    LabelAdded,
    LabelRemoved,
    RelTypeAdded,
    RelTypeRemoved,
}

#[derive(Debug, Serialize)]
pub struct Change {
    pub kind: ChangeKind,
    pub table: String,
    pub detail: String,
    pub severity: Severity,
}

#[derive(Debug, Serialize)]
pub struct Summary {
    pub breaking: usize,
    pub minor: usize,
    pub patch: usize,
}

#[derive(Debug, Serialize)]
pub struct DiffReport {
    pub dialect: Dialect,
    pub changes: Vec<Change>,
    pub summary: Summary,
    pub suggested_bump: String,
}

// ---------------------------------------------------------------------------
// Snapshot types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default)]
pub struct Column {
    pub name: String,
    pub type_name: String,
    pub nullable: bool,
    pub default: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct TableDef {
    pub columns: Vec<Column>,
    pub primary_key: Vec<String>,
    pub clustering_key: Vec<String>,
}

#[derive(Debug, Clone, Default)]
pub struct SchemaSnapshot {
    pub tables: BTreeMap<String, TableDef>,
}

// ---------------------------------------------------------------------------
// Dialect sniffing
// ---------------------------------------------------------------------------

fn sniff_dialect(content: &str) -> Dialect {
    let upper = content.to_uppercase();
    if upper.contains("CLUSTERING ORDER BY") || upper.contains("WITH COMPACTION") {
        return Dialect::Cql;
    }
    // Cypher: `CREATE (n:Label)` or similar node patterns.
    let re_cypher_node = Regex::new(r"CREATE\s*\(\s*\w*\s*:").unwrap();
    if re_cypher_node.is_match(content) {
        return Dialect::Cypher;
    }
    // Bare `:Label` tokens are a strong Cypher signal too.
    let re_label = Regex::new(r"\s:[A-Z]\w*").unwrap();
    if re_label.is_match(content) && upper.contains("MATCH") {
        return Dialect::Cypher;
    }
    Dialect::Sql
}

// ---------------------------------------------------------------------------
// SQL / CQL tokenizer
// ---------------------------------------------------------------------------

/// Split a comma-delimited list respecting paren depth. Used to split the
/// column list inside a `CREATE TABLE (...)` body.
fn split_top_level_commas(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut depth: i32 = 0;
    let mut buf = String::new();
    for ch in s.chars() {
        match ch {
            '(' => {
                depth += 1;
                buf.push(ch);
            }
            ')' => {
                depth -= 1;
                buf.push(ch);
            }
            ',' if depth == 0 => {
                let piece = buf.trim().to_string();
                if !piece.is_empty() {
                    out.push(piece);
                }
                buf.clear();
            }
            _ => buf.push(ch),
        }
    }
    let tail = buf.trim().to_string();
    if !tail.is_empty() {
        out.push(tail);
    }
    out
}

/// Parse a single column definition into a Column. Returns None if the piece
/// looks like a constraint (PRIMARY KEY, FOREIGN KEY, etc.) rather than a
/// column declaration.
fn parse_column(piece: &str) -> Option<Column> {
    let trimmed = piece.trim();
    let upper = trimmed.to_uppercase();

    // Skip known constraint keywords.
    if upper.starts_with("PRIMARY KEY")
        || upper.starts_with("FOREIGN KEY")
        || upper.starts_with("UNIQUE")
        || upper.starts_with("CHECK")
        || upper.starts_with("CONSTRAINT")
        || upper.starts_with("PARTITION KEY")
    {
        return None;
    }

    let mut parts = trimmed.split_whitespace();
    let name = parts
        .next()?
        .trim_matches(|c| c == '"' || c == '`')
        .to_string();
    let type_name = parts.next()?.to_string();

    // The remainder holds NOT NULL / DEFAULT / etc.
    let rest: String = parts.collect::<Vec<_>>().join(" ");
    let rest_upper = rest.to_uppercase();

    let nullable = !rest_upper.contains("NOT NULL");

    let default = if let Some(idx) = rest_upper.find("DEFAULT ") {
        let after = &rest[idx + "DEFAULT ".len()..];
        // Take up to the next keyword-ish boundary.
        let end = after.find([',', '\n']).unwrap_or(after.len());
        Some(after[..end].trim().to_string())
    } else {
        None
    };

    Some(Column {
        name,
        type_name,
        nullable,
        default,
    })
}

/// Extract primary key / partition key / clustering key columns from the body
/// of a `CREATE TABLE` block.
fn extract_keys(body: &str) -> (Vec<String>, Vec<String>) {
    let upper = body.to_uppercase();
    let mut primary: Vec<String> = Vec::new();
    let mut clustering: Vec<String> = Vec::new();

    // Look for an inline `PRIMARY KEY (...)` constraint.
    if let Some(idx) = upper.find("PRIMARY KEY") {
        let after = &body[idx + "PRIMARY KEY".len()..];
        if let Some(open) = after.find('(') {
            // Match closing paren with depth tracking.
            let mut depth: i32 = 0;
            let mut end = 0;
            for (i, ch) in after[open..].char_indices() {
                match ch {
                    '(' => depth += 1,
                    ')' => {
                        depth -= 1;
                        if depth == 0 {
                            end = open + i;
                            break;
                        }
                    }
                    _ => {}
                }
            }
            if end > open {
                let inside = &after[open + 1..end];
                // CQL form: `((pk1, pk2), ck1, ck2)` → first inner group is
                // the partition key, trailing items are clustering cols.
                let trimmed = inside.trim();
                if trimmed.starts_with('(') {
                    // Find matching inner paren.
                    let mut idepth: i32 = 0;
                    let mut iend = 0;
                    for (i, ch) in trimmed.char_indices() {
                        match ch {
                            '(' => idepth += 1,
                            ')' => {
                                idepth -= 1;
                                if idepth == 0 {
                                    iend = i;
                                    break;
                                }
                            }
                            _ => {}
                        }
                    }
                    let pk_group = &trimmed[1..iend];
                    primary = split_top_level_commas(pk_group)
                        .into_iter()
                        .map(|s| s.trim().to_string())
                        .collect();
                    // Remainder after the closing paren (and any following comma)
                    // forms the clustering key list.
                    let remainder = trimmed[iend + 1..].trim_start_matches(',').trim();
                    if !remainder.is_empty() {
                        clustering = split_top_level_commas(remainder)
                            .into_iter()
                            .map(|s| s.trim().to_string())
                            .collect();
                    }
                } else {
                    // Plain SQL: all columns are primary key columns.
                    primary = split_top_level_commas(inside)
                        .into_iter()
                        .map(|s| s.trim().to_string())
                        .collect();
                }
            }
        }
    }

    // Also collect inline `PRIMARY KEY` column modifiers.
    for piece in split_top_level_commas(body) {
        let upper_piece = piece.to_uppercase();
        if upper_piece.contains(" PRIMARY KEY") && !upper_piece.trim_start().starts_with("PRIMARY ")
        {
            if let Some(first) = piece.split_whitespace().next() {
                let name = first.trim_matches(|c| c == '"' || c == '`').to_string();
                if !primary.contains(&name) {
                    primary.push(name);
                }
            }
        }
    }

    (primary, clustering)
}

/// Parse all `CREATE TABLE` blocks in a SQL/CQL source into a snapshot.
pub fn parse_sql_like(source: &str) -> SchemaSnapshot {
    let re_create =
        Regex::new(r"(?is)CREATE\s+TABLE\s+(?:IF\s+NOT\s+EXISTS\s+)?([\w\.]+)\s*\(").unwrap();

    let mut snapshot = SchemaSnapshot::default();

    for caps in re_create.captures_iter(source) {
        let whole = caps.get(0).unwrap();
        let table_name = caps[1].to_string();
        // Normalise: strip keyspace prefix, quotes.
        let table_name = table_name
            .rsplit('.')
            .next()
            .unwrap_or(&table_name)
            .trim_matches(|c| c == '"' || c == '`')
            .to_string();

        // Find the matching close paren after the open paren that ended the match.
        let open_idx = whole.end() - 1; // position of the `(`
        let after = &source[open_idx..];
        let mut depth: i32 = 0;
        let mut end_rel: Option<usize> = None;
        for (i, ch) in after.char_indices() {
            match ch {
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        end_rel = Some(i);
                        break;
                    }
                }
                _ => {}
            }
        }
        let Some(end_rel) = end_rel else {
            continue;
        };
        let body = &after[1..end_rel];

        let mut columns = Vec::new();
        for piece in split_top_level_commas(body) {
            if let Some(col) = parse_column(&piece) {
                columns.push(col);
            }
        }
        let (primary_key, clustering_key) = extract_keys(body);

        snapshot.tables.insert(
            table_name,
            TableDef {
                columns,
                primary_key,
                clustering_key,
            },
        );
    }

    snapshot
}

// ---------------------------------------------------------------------------
// Diff for SQL / CQL snapshots
// ---------------------------------------------------------------------------

fn diff_sql_like(before: &SchemaSnapshot, after: &SchemaSnapshot) -> Vec<Change> {
    let mut changes = Vec::new();

    // Dropped and modified tables.
    for (name, before_tbl) in &before.tables {
        match after.tables.get(name) {
            None => {
                changes.push(Change {
                    kind: ChangeKind::TableDropped,
                    table: name.clone(),
                    detail: format!("Table {name} dropped"),
                    severity: Severity::Breaking,
                });
            }
            Some(after_tbl) => {
                diff_table(name, before_tbl, after_tbl, &mut changes);
            }
        }
    }

    // Added tables.
    for name in after.tables.keys() {
        if !before.tables.contains_key(name) {
            changes.push(Change {
                kind: ChangeKind::TableAdded,
                table: name.clone(),
                detail: format!("Table {name} added"),
                severity: Severity::Minor,
            });
        }
    }

    changes
}

fn diff_table(name: &str, before: &TableDef, after: &TableDef, changes: &mut Vec<Change>) {
    let before_cols: BTreeMap<&str, &Column> = before
        .columns
        .iter()
        .map(|c| (c.name.as_str(), c))
        .collect();
    let after_cols: BTreeMap<&str, &Column> =
        after.columns.iter().map(|c| (c.name.as_str(), c)).collect();

    for (col_name, before_col) in &before_cols {
        match after_cols.get(col_name) {
            None => changes.push(Change {
                kind: ChangeKind::ColumnDropped,
                table: name.to_string(),
                detail: format!("Column {col_name} dropped"),
                severity: Severity::Breaking,
            }),
            Some(after_col) => {
                if before_col.type_name.to_uppercase() != after_col.type_name.to_uppercase() {
                    changes.push(Change {
                        kind: ChangeKind::ColumnTypeChanged,
                        table: name.to_string(),
                        detail: format!(
                            "Column {col_name} type: {} -> {}",
                            before_col.type_name, after_col.type_name
                        ),
                        severity: Severity::Breaking,
                    });
                }
                if before_col.nullable != after_col.nullable {
                    // Becoming non-nullable is breaking; relaxing is minor.
                    let sev = if before_col.nullable && !after_col.nullable {
                        Severity::Breaking
                    } else {
                        Severity::Minor
                    };
                    changes.push(Change {
                        kind: ChangeKind::NullabilityChanged,
                        table: name.to_string(),
                        detail: format!(
                            "Column {col_name} nullable: {} -> {}",
                            before_col.nullable, after_col.nullable
                        ),
                        severity: sev,
                    });
                }
            }
        }
    }

    for (col_name, after_col) in &after_cols {
        if !before_cols.contains_key(col_name) {
            // Added: minor if it has a default OR is nullable; breaking otherwise.
            let sev = if after_col.default.is_some() || after_col.nullable {
                Severity::Minor
            } else {
                Severity::Breaking
            };
            changes.push(Change {
                kind: ChangeKind::ColumnAdded,
                table: name.to_string(),
                detail: format!("Column {col_name} added (type {})", after_col.type_name),
                severity: sev,
            });
        }
    }

    if before.primary_key != after.primary_key {
        changes.push(Change {
            kind: ChangeKind::PrimaryKeyChanged,
            table: name.to_string(),
            detail: format!(
                "Primary key: {:?} -> {:?}",
                before.primary_key, after.primary_key
            ),
            severity: Severity::Breaking,
        });
    }
    if before.clustering_key != after.clustering_key {
        changes.push(Change {
            kind: ChangeKind::ClusteringKeyChanged,
            table: name.to_string(),
            detail: format!(
                "Clustering key: {:?} -> {:?}",
                before.clustering_key, after.clustering_key
            ),
            severity: Severity::Breaking,
        });
    }
}

// ---------------------------------------------------------------------------
// Cypher diff
// ---------------------------------------------------------------------------

#[derive(Debug, Default)]
struct CypherInventory {
    labels: BTreeSet<String>,
    rel_types: BTreeSet<String>,
}

fn parse_cypher(source: &str) -> CypherInventory {
    let re_label = Regex::new(r":([A-Z]\w*)").unwrap();
    let re_rel = Regex::new(r"\[:([A-Z_]\w*)").unwrap();

    let mut inv = CypherInventory::default();
    for caps in re_label.captures_iter(source) {
        inv.labels.insert(caps[1].to_string());
    }
    for caps in re_rel.captures_iter(source) {
        let t = caps[1].to_string();
        inv.rel_types.insert(t.clone());
        // The label regex will also match relationship types — strip them from
        // the labels set so a pure rel-type doesn't get counted as a label.
        inv.labels.remove(&t);
    }
    inv
}

fn diff_cypher(before: &CypherInventory, after: &CypherInventory) -> Vec<Change> {
    let mut changes = Vec::new();

    for label in &before.labels {
        if !after.labels.contains(label) {
            changes.push(Change {
                kind: ChangeKind::LabelRemoved,
                table: label.clone(),
                detail: format!("Node label {label} removed"),
                severity: Severity::Breaking,
            });
        }
    }
    for label in &after.labels {
        if !before.labels.contains(label) {
            changes.push(Change {
                kind: ChangeKind::LabelAdded,
                table: label.clone(),
                detail: format!("Node label {label} added"),
                severity: Severity::Minor,
            });
        }
    }
    for rt in &before.rel_types {
        if !after.rel_types.contains(rt) {
            changes.push(Change {
                kind: ChangeKind::RelTypeRemoved,
                table: rt.clone(),
                detail: format!("Relationship type {rt} removed"),
                severity: Severity::Breaking,
            });
        }
    }
    for rt in &after.rel_types {
        if !before.rel_types.contains(rt) {
            changes.push(Change {
                kind: ChangeKind::RelTypeAdded,
                table: rt.clone(),
                detail: format!("Relationship type {rt} added"),
                severity: Severity::Minor,
            });
        }
    }

    changes
}

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

fn summarise(changes: &[Change]) -> (Summary, String) {
    let mut breaking = 0;
    let mut minor = 0;
    let mut patch = 0;
    for c in changes {
        match c.severity {
            Severity::Breaking => breaking += 1,
            Severity::Minor => minor += 1,
            Severity::Patch => patch += 1,
        }
    }
    let bump = if breaking > 0 {
        "major"
    } else if minor > 0 {
        "minor"
    } else {
        "patch"
    }
    .to_string();
    (
        Summary {
            breaking,
            minor,
            patch,
        },
        bump,
    )
}

/// Diff two schema sources. If `dialect` is `None`, it is sniffed from the
/// `after` content (falling back to `before` if that is ambiguous).
pub fn diff_schemas(before: &str, after: &str, dialect: Option<Dialect>) -> Result<DiffReport> {
    let dialect = dialect.unwrap_or_else(|| {
        let d = sniff_dialect(after);
        if matches!(d, Dialect::Sql) {
            sniff_dialect(before)
        } else {
            d
        }
    });

    let changes = match dialect {
        Dialect::Sql | Dialect::Cql => {
            let b = parse_sql_like(before);
            let a = parse_sql_like(after);
            diff_sql_like(&b, &a)
        }
        Dialect::Cypher => {
            let b = parse_cypher(before);
            let a = parse_cypher(after);
            diff_cypher(&b, &a)
        }
    };

    let (summary, suggested_bump) = summarise(&changes);

    Ok(DiffReport {
        dialect,
        changes,
        summary,
        suggested_bump,
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sql_drop_column_is_breaking() {
        let before = "CREATE TABLE users (id INT, email VARCHAR, legacy_id INT);";
        let after = "CREATE TABLE users (id INT, email VARCHAR);";
        let report = diff_schemas(before, after, Some(Dialect::Sql)).unwrap();
        assert!(report
            .changes
            .iter()
            .any(|c| matches!(c.kind, ChangeKind::ColumnDropped)
                && matches!(c.severity, Severity::Breaking)));
        assert_eq!(report.suggested_bump, "major");
    }

    #[test]
    fn sql_add_column_with_default_is_minor() {
        let before = "CREATE TABLE users (id INT, email VARCHAR);";
        let after =
            "CREATE TABLE users (id INT, email VARCHAR, email_verified BOOLEAN DEFAULT false);";
        let report = diff_schemas(before, after, Some(Dialect::Sql)).unwrap();
        let added: Vec<_> = report
            .changes
            .iter()
            .filter(|c| matches!(c.kind, ChangeKind::ColumnAdded))
            .collect();
        assert_eq!(added.len(), 1);
        assert!(matches!(added[0].severity, Severity::Minor));
        assert_eq!(report.suggested_bump, "minor");
    }

    #[test]
    fn sql_add_column_not_null_no_default_is_breaking() {
        let before = "CREATE TABLE users (id INT);";
        let after = "CREATE TABLE users (id INT, name VARCHAR NOT NULL);";
        let report = diff_schemas(before, after, Some(Dialect::Sql)).unwrap();
        let added: Vec<_> = report
            .changes
            .iter()
            .filter(|c| matches!(c.kind, ChangeKind::ColumnAdded))
            .collect();
        assert_eq!(added.len(), 1);
        assert!(matches!(added[0].severity, Severity::Breaking));
    }

    #[test]
    fn cql_partition_key_reorder_is_breaking() {
        let before = "CREATE TABLE events (tenant TEXT, user_id UUID, ts TIMEUUID, PRIMARY KEY ((tenant, user_id), ts));";
        let after = "CREATE TABLE events (tenant TEXT, user_id UUID, ts TIMEUUID, PRIMARY KEY ((user_id, tenant), ts));";
        let report = diff_schemas(before, after, Some(Dialect::Cql)).unwrap();
        assert!(report
            .changes
            .iter()
            .any(|c| matches!(c.kind, ChangeKind::PrimaryKeyChanged)
                && matches!(c.severity, Severity::Breaking)));
        assert_eq!(report.suggested_bump, "major");
    }

    #[test]
    fn cql_add_clustering_column_is_breaking() {
        let before = "CREATE TABLE events (tenant TEXT, ts TIMEUUID, PRIMARY KEY ((tenant), ts));";
        let after = "CREATE TABLE events (tenant TEXT, ts TIMEUUID, kind TEXT, PRIMARY KEY ((tenant), ts, kind));";
        let report = diff_schemas(before, after, Some(Dialect::Cql)).unwrap();
        assert!(report
            .changes
            .iter()
            .any(|c| matches!(c.kind, ChangeKind::ClusteringKeyChanged)
                && matches!(c.severity, Severity::Breaking)));
    }

    #[test]
    fn cypher_remove_label_is_breaking() {
        let before = "CREATE (n:User {id: 1}) CREATE (m:LegacyAccount {id: 2})";
        let after = "CREATE (n:User {id: 1})";
        let report = diff_schemas(before, after, Some(Dialect::Cypher)).unwrap();
        assert!(report
            .changes
            .iter()
            .any(|c| matches!(c.kind, ChangeKind::LabelRemoved)
                && matches!(c.severity, Severity::Breaking)));
        assert_eq!(report.suggested_bump, "major");
    }

    #[test]
    fn sniff_detects_cql() {
        let src = "CREATE TABLE events (id UUID, ts TIMEUUID, PRIMARY KEY (id, ts)) WITH CLUSTERING ORDER BY (ts DESC);";
        assert_eq!(sniff_dialect(src), Dialect::Cql);
    }

    #[test]
    fn sniff_detects_cypher() {
        let src = "CREATE (n:User {id: 1})-[:FOLLOWS]->(m:User)";
        assert_eq!(sniff_dialect(src), Dialect::Cypher);
    }

    #[test]
    fn sniff_falls_back_to_sql() {
        let src = "CREATE TABLE users (id INT PRIMARY KEY);";
        assert_eq!(sniff_dialect(src), Dialect::Sql);
    }

    #[test]
    fn suggested_bump_patch_when_no_changes() {
        let src = "CREATE TABLE t (id INT);";
        let report = diff_schemas(src, src, Some(Dialect::Sql)).unwrap();
        assert_eq!(report.suggested_bump, "patch");
        assert_eq!(report.summary.breaking, 0);
    }

    #[test]
    fn suggested_bump_minor_on_new_table() {
        let before = "CREATE TABLE a (id INT);";
        let after = "CREATE TABLE a (id INT); CREATE TABLE b (id INT);";
        let report = diff_schemas(before, after, Some(Dialect::Sql)).unwrap();
        assert_eq!(report.suggested_bump, "minor");
    }

    #[test]
    fn report_serializes_to_json() {
        let before = "CREATE TABLE t (id INT);";
        let after = "CREATE TABLE t (id BIGINT);";
        let report = diff_schemas(before, after, Some(Dialect::Sql)).unwrap();
        let json = serde_json::to_string(&report).unwrap();
        assert!(json.contains("\"suggested_bump\":\"major\""));
        assert!(json.contains("\"dialect\":\"sql\""));
    }
}
