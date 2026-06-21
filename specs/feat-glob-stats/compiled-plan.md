# feat-glob-stats — Compiled Execution Plan

> Agent-executable plan. Each task is a self-contained work packet with inputs, outputs, and verification.
> Execution order follows the DAG; siblings in a batch may run in parallel.

## Dependency DAG

```text
       ┌──────────────┐
       │ T1  Skeleton │
       └──────┬───────┘
              │
     ┌────────┴────────┐
     ▼                 ▼
 ┌────────┐       ┌─────────┐
 │T2 Walk │       │T3 Config│
 └───┬────┘       └────┬────┘
     │                 │
     └────────┬────────┘
              ▼
       ┌──────────────┐
       │ T4 Filters   │
       └──────┬───────┘
              │
     ┌────────┴────────┐
     ▼                 ▼
 ┌────────┐       ┌─────────┐
 │T5 Fmts │       │T6 MCP   │
 └───┬────┘       └────┬────┘
     │                 │
     └────────┬────────┘
              ▼
       ┌──────────────┐
       │ T7 Tests+CI  │
       └──────────────┘
```

## Task packets

### T1 — Subcommand skeleton

- **Deliverable:** `frg glob <PATTERN>` compiles and prints matched paths (no filters yet).
- **Files:**
  - `crates/cli/src/main.rs` — add `GlobStats(GlobArgs)` to the subcommand enum + dispatch
  - `crates/cli/src/glob.rs` — new module with `run(args: GlobArgs) -> Result<()>`
  - `crates/cli/src/glob_builtin.toml` — default excludes (`.git/`, `target/`, `vendor/`, `node_modules/`, `*.lock`)
- **Verification:**
  - `cargo build -p cli` succeeds
  - `cargo test -p cli glob::smoke` passes with fixture tree

### T2 — Walker + metadata

- **Deliverable:** Traversal with `ignore::WalkBuilder`, collecting `(path, lines, bytes, modified, is_generated)`.
- **Guards (from `rust-hazards.md`):**
  - No `.unwrap()` / `.expect()` — propagate via `?`
  - `to_string_lossy()` for non-UTF-8 paths, set `path_encoding` marker
  - POSIX separator normalization on all emitted paths
- **Verification:** Unit tests `filters_min_max_lines`, `lossy_utf8_path_produces_marker`, `paths_posix_separator_always` pass

### T3 — Config + builtin excludes

- **Deliverable:** Merge `glob_builtin.toml` excludes with user `--exclude`; secret denylist is additive and non-overridable.
- **Shared source:** Import secret denylist from `crates/secret-scan` (or `crates/shared` if promoted) — do **not** duplicate the list.
- **Verification:** Unit tests `secret_denylist_cannot_be_bypassed`, `secret_denylist_matches_secret_scan_crate` pass

### T4 — Filters + pattern normalization

- **Deliverable:** All filter flags functional (`--min-lines`, `--max-lines`, `--min-bytes`, `--max-bytes`, `--modified-after`, `--modified-before`, `--exclude`, `--allow-absolute`, `--max-results`, `--max-depth`).
- **Critical guard:** Pattern normalization rejects `..` and absolute patterns unless `--allow-absolute`.
- **Short-circuit:** `--max-bytes` check uses `metadata()` only — never opens the file.
- **Verification:** Unit tests `normalize_pattern_*`, `filters_min_max_bytes_short_circuit`, property `prop_normalized_pattern_no_dotdot` pass

### T5 — Output formats

- **Deliverable:** `brief` (streaming), `json`, `csv` (streaming), `table` formats.
- **Guards:** `csv` crate for quoting (F11); control-char scrubbing in `table` (F12); JSON emits `$schema_version: 1`.
- **Verification:** Snapshot tests (`insta`), property `prop_json_roundtrip`, unit `output_csv_rfc4180_quoting`, `output_table_strips_ansi` pass

### T6 — MCP tool registration

- **Deliverable:** `glob` tool registered in `crates/mcp-server/src/lib.rs` with `readOnly: true`, `destructiveHint: false`.
- **Parity:** MCP response body is byte-identical to CLI `--format json` for the same inputs.
- **Verification:** Contract test `mcp_tool_output_matches_cli_json`, `mcp_tool_metadata_readonly_true`, `mcp_tool_rejects_absolute_without_flag` pass

### T7 — Test suite + CI

- **Deliverable:**
  - Integration tests in `tests/glob_integration.rs` (symlink, concurrent delete, depth limit, gitignore)
  - Windows CI job added (gated `#[cfg(windows)]` tests)
  - Clippy config: `unwrap_used = "deny"`, `panic = "deny"` in glob scope
  - RPN-coverage audit script added to `ci/` and wired into workflow
- **Verification:**
  - `cargo test -p cli` green
  - `cargo clippy -- -D warnings` green
  - RPN-coverage audit reports 100% (every FMEA row ≥ 50 referenced in a test name)
  - Coverage ≥ 85% for `glob.rs` (tarpaulin / llvm-cov)

## Parallel execution batches

- **Batch 1:** T1 (must land first)
- **Batch 2:** T2, T3 (parallel — independent files)
- **Batch 3:** T4 (depends on T2+T3)
- **Batch 4:** T5, T6 (parallel — different crates)
- **Batch 5:** T7 (depends on all prior — final verification)

## Three-tier verification

| Tier | Gate | Command |
|------|------|---------|
| Per-task | Task's own tests pass | `cargo test -p cli <pattern>` |
| Feature | Full suite green | `cargo test -p cli` + `cargo test --test glob_integration` |
| System | Workspace build + lint + MCP contract + RPN audit | `make test-all` |

## Exit criteria

All verification checkboxes in `overview.md §Verification` hold; all P0 hazards in `rust-hazards.md` guarded; RPN-coverage audit green; spec moves from `todo/` → `implemented/` with the commit that ships T7.
