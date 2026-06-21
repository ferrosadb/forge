# feat-glob-stats — Overview

> Status: Blueprint
> Source spec: `../todo/feat-glob-stats.md`
> Pipeline: `todo/` (moves to `in-process/` on sprint pickup)

## Purpose

Provide a `frg glob` subcommand and matching MCP tool that lists files matching a glob pattern with structured metadata (lines, bytes, mtime, is_generated), plus filters for size and age. Eliminates the two-call "find then read" pattern agents use today.

## Value

- Cuts ≥1 tool call per discovery task (agents skip Bash + parsing)
- Output is machine-readable by default → pipeable into `digest`, `smell_detect`, `dsm`
- Deterministic ignore semantics (gitignore-aware) without re-implementing per-skill

## Scope

**In scope**

- New CLI subcommand `GlobStats` (exposed as `frg glob`)
- New MCP tool `glob` (readOnly, returns JSON)
- Filters: `--min-lines`, `--max-lines`, `--min-bytes`, `--max-bytes`, `--modified-after`, `--modified-before`, `--exclude`
- Output formats: `brief` (default), `json`, `csv`, `table`
- Default excludes via `glob_builtin.toml` (`.git/`, `target/`, `vendor/`, `node_modules/`, `*.lock`)
- gitignore / .ignore respect via the `ignore` crate (already a workspace dep for `digest`)

**Out of scope**

- Content search (that is `grep` / `ripgrep`)
- AST-level metadata (stays in `digest` / `module_outline`)
- Cross-repo globbing
- Watch mode / incremental updates

## Dependencies

- Reuses the `ignore` crate already in the workspace (confirm in Phase T1)
- Reuses `shared::config` for the builtin TOML lookup pattern
- No new external crates unless line-counting benchmarks justify `bytecount` or `memchr`

## Interface contract

```text
frg glob <PATTERN> [FILTERS] [--format brief|json|csv|table]

Exit codes:
  0 — success (including zero matches)
  2 — invalid pattern or filter argument
  3 — IO error during traversal
```

JSON schema (stable, versioned via `$schema_version`):

```json
{
  "$schema_version": 1,
  "pattern": "<input pattern>",
  "filters": { "...echoed filter values..." },
  "results": [
    { "path": "...", "lines": N, "bytes": N, "modified": "ISO8601", "is_generated": bool }
  ],
  "total_matched": N,
  "total_skipped": N,
  "skipped_reasons": { "too_large": N, "ignored": N, "not_a_file": N }
}
```

## Constraints

- Memory must stay bounded independent of match count (stream results to stdout in `brief`/`csv`; buffer only for `json`/`table`)
- Must handle non-UTF-8 paths without panic (lossy conversion with a `path_encoding` field when lossy)
- Large files (> `--max-bytes`) must be skipped **without** opening them for line-counting
- Symlinks: do not follow by default; add `--follow` flag only if a concrete use case appears

## References

- Sibling: `feat-ingest-function-descriptions` (agents will chain `glob → ingest`)
- Prior art: the `ignore` crate's `WalkBuilder` powers `frg digest`
