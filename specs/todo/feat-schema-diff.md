# Feat: schema_diff — Compare SQL/CQL/graph schemas for breaking migrations

**Priority:** High
**Component:** new crate `forge-schema-diff`, CLI subcommand `schema-diff`, MCP tool `schema_diff`

## Goal

Complement `api_contract_diff` for database schemas. `sql-create`, `cql-create`, and `graph-create` skills hand-write migration review. This tool compares two schema files (or git revs) and classifies each difference as breaking/minor/patch.

## Input

- `--before`: path or git-ref pointing to old schema
- `--after`: path or git-ref pointing to new schema
- `--dialect`: `sql`, `cql`, `cypher` (auto-detected from extension or content)

## Dialect detection

- `.sql` → SQL. If contains `CLUSTERING ORDER BY` or `WITH COMPACTION` → CQL.
- `.cql` → CQL.
- `.cypher` or content contains `CREATE (n:` → Cypher.

## Classification (SQL/CQL)

| Change | Severity |
|---|---|
| `DROP TABLE` / `DROP COLUMN` | **Breaking** |
| Column type widened (`INT` → `BIGINT`) | Minor |
| Column type narrowed (`BIGINT` → `INT`) | **Breaking** |
| `NOT NULL` added to existing nullable column | **Breaking** |
| `NOT NULL` removed | Minor |
| Primary key changed | **Breaking** |
| Partition key changed (CQL) | **Breaking** |
| Index added/dropped | Minor (non-breaking for readers) |
| `CREATE TABLE` added | Minor |
| `CREATE INDEX` added | Minor |
| Column added with default | Minor |
| Column added without default (NOT NULL) | **Breaking** |

## Classification (Cypher / graph schema)

| Change | Severity |
|---|---|
| Node label removed | **Breaking** |
| Relationship type removed | **Breaking** |
| Required property removed | **Breaking** |
| Unique constraint removed | Minor |
| Node label added | Minor |
| Property added | Patch |

## Parser approach (v1)

Tokenize, don't parse. For SQL/CQL: regex-match `CREATE TABLE`, `ALTER TABLE`, column definitions between parens. Build a `SchemaSnapshot { tables: Map<Name, TableDef> }` for each side and diff.

`TableDef` = `{ columns: Vec<Column>, primary_key: Vec<String>, clustering_key: Vec<String> }` where `Column = { name, type, nullable, default }`.

## Output

```json
{
  "dialect": "cql",
  "changes": [
    {"kind": "column_dropped", "table": "users", "column": "legacy_id", "severity": "breaking"},
    {"kind": "column_added", "table": "users", "column": "email_verified", "default": "false", "severity": "minor"},
    {"kind": "clustering_key_changed", "table": "events", "before": ["ts"], "after": ["ts", "kind"], "severity": "breaking"}
  ],
  "summary": {"breaking": 2, "minor": 1, "patch": 0},
  "suggested_bump": "major"
}
```

## Dependencies

- `regex`, `forge-shared`, `anyhow`.

## Test plan

- SQL fixture pairs exercising each change kind.
- CQL fixture with partition/clustering key reorder (breaking).
- Cypher fixture with label rename (breaking).
- End-to-end: compare two CQL files from `ferrosa-dbaas/specs/`.

## Out of scope (v1)

- Full SQL parser (use sqlparser-rs in v2 if false-positive rate is too high).
- Migration script generation.
- Trigger/procedure/view handling.
