# feat-glob-stats — Test Plan

> 7-layer test spec, feature-scoped. Every FMEA row with RPN ≥ 50 has a dedicated test.

## Layer 1 — Unit tests (`crates/cli/src/glob.rs` + inline `#[cfg(test)]`)

| Test | Covers | Notes |
|------|--------|-------|
| `normalize_pattern_rejects_parent_escape` | F1 / R-P0-3 | Input `../../**` → `Err(PatternEscape)` |
| `normalize_pattern_rejects_absolute_by_default` | F1 | `/etc/**` without `--allow-absolute` → error |
| `normalize_pattern_allows_absolute_with_flag` | F1 | `/tmp/**` with flag → ok |
| `secret_denylist_cannot_be_bypassed` | F7 / R-P0-4 | With `--exclude ''` passed, `.env` still excluded |
| `secret_denylist_matches_secret_scan_crate` | F7 | Assert list equality at compile time (shared const) |
| `filters_min_max_lines` | spec §3 | Fixture with 10, 100, 1000 line files; `--min-lines 50 --max-lines 500` → only the 100 |
| `filters_min_max_bytes_short_circuit` | F3 | 200MB sparse file; assert `metadata()` called once, `open()` never |
| `lossy_utf8_path_produces_marker` | F4 | Unix-only; assert `path_encoding: "lossy"` |
| `output_brief_streams` | F5 / R-P1-1 | Generate 15k matches with `--max-results 10000` → output truncated, marker set |
| `output_json_is_valid_schema` | spec §2 | Parse back via serde; assert `$schema_version == 1` |
| `output_csv_rfc4180_quoting` | F11 | Filename `a,b"c.rs` → correctly quoted |
| `output_table_strips_ansi` | F12 | Filename with `\x1b[31m` → sanitized |
| `paths_posix_separator_always` | F10 | Assert no `\\` in any output path |

## Layer 2 — Property tests (`proptest`)

| Test | Covers |
|------|--------|
| `prop_normalized_pattern_no_dotdot` | F1 — for all inputs, normalized pattern lacks `..` segments unless flag set |
| `prop_json_roundtrip` | spec §2 — emit JSON, parse it, re-emit; stable |
| `prop_filter_monotonicity` | Increasing `--min-lines` never grows result set |
| `prop_exclude_subsumption` | Adding `--exclude` only removes results (never adds) |

## Layer 3 — Fixture / integration tests (`tests/glob_integration.rs`)

| Test | Covers |
|------|--------|
| `gitignore_respected_by_default` | spec §5 — target/, node_modules/ excluded |
| `symlink_not_followed_by_default` | F2 — symlink to external dir is reported as symlink, not descended |
| `symlink_loop_bounded` | F2 — self-referential symlink does not hang |
| `concurrent_deletion_no_crash` | F6 — spawn thread that deletes files during walk; run completes with skip tally |
| `depth_limit_enforced` | R-P1-6 — 50-deep nested dirs, `--max-depth 10` halts at 10 |
| `generated_file_classification` | F9 — fixtures with and without `@generated` sentinel |
| `windows_paths_normalized` | F10 — gated on `#[cfg(windows)]` |

## Layer 4 — CLI snapshot tests (`insta` crate)

- `frg glob --help` output — detect breaking interface change
- `frg glob 'src/**/*.rs' --format json` on a deterministic fixture tree
- `frg glob 'src/**/*.rs' --format table` on same

## Layer 5 — MCP contract tests

| Test | Covers |
|------|--------|
| `mcp_tool_metadata_readonly_true` | T7 — `readOnly: true`, `destructiveHint: false` |
| `mcp_tool_output_matches_cli_json` | Parity check: CLI `--format json` == MCP tool response |
| `mcp_tool_rejects_absolute_without_flag` | F1 — same guard in MCP path |

## Layer 6 — Performance smoke (`criterion`, opt-in via `--features bench`)

| Bench | Target |
|-------|--------|
| `glob_10k_files_json` | ≤ 500ms on M-series laptop for 10k-file workspace |
| `glob_large_file_skip` | `--max-bytes 1MB` against tree with 200MB files: no regression vs empty tree |

Perf is NOT gating in CI; used for regression watch.

## Layer 7 — Security regression

- F1 corpus: 50 crafted patterns (fuzzing seed) that must all be rejected or anchored
- F7 corpus: filenames of common secret files (`.env`, `.env.local`, `id_rsa`, `*.pem`, `credentials.json`) — all must be absent from output under any CLI invocation

## Coverage gate

- ≥ 85% line coverage for `crates/cli/src/glob.rs` (feature is isolated; higher bar than baseline)
- CC ≥ 15 functions require docstrings + 90% local coverage (per language-skill rule)

## RPN coverage audit

CI script: for every FMEA row with RPN ≥ 50, grep that its row ID (`F1`, `F2`, …) appears in at least one `#[test]` name or doc comment. Missing ID fails CI.
