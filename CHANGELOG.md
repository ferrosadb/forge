# Changelog

All notable changes to forge will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.13.5] - 2026-06-13

Three small, independent cleanups (board tasks t_2c779031, t_29e8dc26, t_129ba347).

### Fixed
- **`frg task` output is guaranteed valid JSON even with multi-line bodies**
  (t_2c779031). A task body containing literal control characters (newlines,
  tabs) must not break `frg task list | jq`. The emit path already routes every
  `frg task` command through `forge_shared::emit_json` → `serde_json`, which
  escapes control chars; this release **locks that guarantee with regression
  tests** so a future hand-rolled serializer can't reintroduce the bug. Added
  `emit_json_escapes_control_chars` (shared) and
  `task_with_multiline_body_serializes_to_valid_json` (tasks). Note: no behavior
  change was required — the live path was already correct; the reported failure
  could only have come from an older/hand-built JSON path, which no longer
  exists.

### Changed
- **`frg project-detect` recognizes more C/C++ build systems and distinguishes
  C from C++** (t_29e8dc26). In addition to `CMakeLists.txt`, the detector now
  recognizes `Makefile`/`makefile`, `meson.build`, and autotools
  (`configure.ac`/`configure.in`/`configure`) as C-family build setups, emitting
  the matching build tool (CMake/Make/Meson/Autotools). The language is now
  classified as **C** (only `*.c`/`*.h` present) vs **C++** (any
  `*.cpp`/`*.cc`/`*.cxx`/`*.c++`/`*.hpp`/`*.hxx`/`*.hh`) from the source files
  in the tree; when sources are absent it falls back to the ambiguous `C/C++`
  tag for CMake/Meson/Autotools, while a bare `Makefile` with no C/C++ sources
  records only the `Make` build tool (no language claim). Previously only CMake
  was recognized and everything was tagged `C/C++`.
- **`todo_extract` flags non-actionable findings instead of counting fixtures,
  test code, and its own source as real debt** (t_129ba347). Each `Finding`
  gains an `actionable: bool` and an optional `exclude_reason` code; the report
  gains an `actionable_count`. Findings are **flagged, never dropped** (fail
  loud — nothing is silently hidden). Exclusion reasons: `test-path` (file under
  a `tests/`/`test/`/`__tests__`/`spec` directory or a test-named file),
  `fixture-file` (basename contains `fixture`), and `detector-source` (file
  under a `todo-extract/` path). Skills like `/roadmap` should prioritize off
  `actionable_count` rather than `findings.len()`.
  **Limitation (scoped):** general string-literal detection was deliberately
  *not* implemented — it needs per-language tokenization and risks dropping real
  debt in commented-out code. Instead, the existing token regex already only
  matches a token preceded by a comment marker or whitespace, so directly-quoted
  tokens (`"TODO"`, the dominant keyword-list/regex false positive) are rejected
  upstream and never become findings (covered by
  `directly_quoted_tokens_never_become_findings`). Tokens embedded mid-string
  after whitespace are not specially detected.

## [0.13.4] - 2026-06-12

### Fixed
- **MCP tool results now surface the payload in `structuredContent`.** Previously
  `structuredContent` held only a `{tool, duration_ms, is_error}` metadata envelope, so
  MCP clients that render `structuredContent` (newer Claude Code) showed the envelope and
  hid the actual result that lived in `content[0].text` — forcing agents to fall back to
  grep. Now `structuredContent` is `{ "result": <payload> }` (the data), and call metadata
  moves to `_meta`. `content[0].text` is unchanged as the fallback. (Same fix is needed in
  ferrosa-memory's `dispatch.rs`, where this pattern originated.)

## [0.13.3] - 2026-06-12

### Added
- **`frg task create` gains `--workspace-path` and `--metadata` flags** (and the same
  `metadata` field on the `task_create` MCP tool). `workspace_path` is the per-repo key
  used by the deferred-work `/whats-next` and `/roadmap` skills; `metadata` takes a JSON
  string. Lets the `defer-capture` Stop hook tag the repo natively (in `workspace_path`)
  and stash `{source, session_id}` in `metadata` instead of encoding them in the body.
- **`metadata` now round-trips.** Added the `metadata` field to `CreateTaskRequest`, the
  INSERT now writes it, and the row mapper parses the `metadata` text column back to JSON
  (previously it was write-null / read-skipped). Verified create + read-back.

## [0.13.2] - 2026-06-12

### Added
- **Configurable CQL contact point for the task board.** The CQL `host:port` now
  resolves with precedence: explicit `cql_host` arg → `FORGE_CQL_HOST` env →
  `cql_host` in the nearest `.forge/config.toml` (walking up from the cwd) →
  built-in `127.0.0.1:9042`. Lets the task tools target a port-remapped cluster
  (e.g. `127.0.0.1:19042`) without passing `cql_host` on every call. New
  `forge_tasks::resolve_cql_host` (unit-tested) wired into all task MCP tools and
  `frg task` CLI subcommands; CLI `--cql-host` is now optional.

## [0.13.1] - 2026-06-09

### Fixed
- **`ingest` excludes build artifacts and honors `.gitignore`.** Top-level build
  directories (e.g. `target/`, `node_modules/`, `dist/`) and `.gitignore`d paths
  are skipped during crawling, avoiding wasted work and noisy entities.
- **Bounded LSP initialization.** `rust-analyzer` is started with ingest-tuned
  options that keep it in cheap, syntactic mode so initialization cannot exceed
  the caller's MCP timeout on large repos.
- Apply `cargo fmt` to `ingest` (`ignore_policy.rs`, `lsp.rs`).

## [0.13.0] - 2026-06-04

### Added
- **Prompt-injection-cleansed academic paper ingestion.** Paper metadata,
  abstracts, and extracted sections are sanitized before graph construction so
  adversarial paper text is treated as data and removed before memory ingest.
- **Typed fmem `smart_ingest` client support.** `forge-fmem-client` now exposes
  a typed wrapper for fmem's prediction-error-gated `smart_ingest` path.
- **Smart paper loader.** Paper-derived entities are resolved through fmem
  `smart_ingest`, then relationship edges are remapped to the fmem-resolved
  entity IDs before insertion.

### Changed
- `frg ingest-paper` and the MCP `ingest_paper` handler use the smart paper
  loader for non-dry-run ingestion while preserving dry-run extraction output.
- Integrated outstanding materialization-scan observability and Go-language scan
  support into the Forge release line.

## [0.12.0] - 2026-05-22

### Added
- **`materialization-scan` Go language support.** Extends the static-analysis
  pass to recognize Go's equivalents of the same five unbounded-materialization
  shapes already covered for Rust:
  - Whole-file reads: `io.ReadAll`, `ioutil.ReadAll`, `os.ReadFile`,
    `ioutil.ReadFile`.
  - Query/cursor materialization: `for rows.Next() { ... append(...) }`,
    `cursor.All`, `iter.Scan`, `cursor.Decode`.
  - Collect: `slices.Collect` / `maps.Collect` (Go 1.23+).
  - Slice growth: `var x []T` / `make([]T, ...)` / `:= []T{}` declarations
    combined with `= append(...)` in an I/O-scoped function.
  - Map-of-slice grouping: `map[K][]V` declaration plus
    `m[k] = append(m[k], v)`.
  Function-finding now handles Go's `func Name(` and method-receiver
  `func (r *Recv) Name(` forms. `_test.go` files and `Test`/`Benchmark`/
  `Example`/`Fuzz` functions are skipped by default.

### Changed
- Internal scanner restructure: new `Lang` enum + `detect_lang(file)`;
  `scan_file` dispatches by language, and the pattern helpers
  (`is_whole_file_read`, `io_score`, `contains_*`, evidence-needle and
  reason/remediation lookups) take a `Lang` and return language-specific
  results. No behavior change for Rust scans.
- `should_scan_file` tightened to `.rs` and `.go` only — other extensions
  produced no findings anyway and were just wasted I/O.

## [0.11.0] - 2026-05-22

### Added
- **`frg materialization-scan` subcommand + `materialization_scan` MCP tool.**
  Static-analysis pass that flags likely unbounded materialization in
  disk/storage/query I/O paths: whole-file reads, query
  `rows_or_empty()`/`ALLOW FILTERING` result materialization, `collect()` in
  read paths, growing `Vec`/`Vec<Vec>`, and map-of-`Vec` grouping shapes.
  Use to build a checklist for fixing OOM risks by streaming, paging,
  chunking, server-side aggregates, or bounded buffers.
- **MCP server: `structuredContent` + per-call timing logs.** Each
  `tools/call` response now carries a `structuredContent` block with the
  tool name, `duration_ms`, and `is_error` flag (alongside the existing
  text body), and each call emits stderr log lines on start/finish for
  external observability. Callers that prefer structured output can read
  duration / error state without parsing the text body.

### Documentation
- README: documents the new `materialization-scan` CLI command and MCP tool.
- `skills/task-level/debug/memory/SKILL.md`: cross-references
  `materialization-scan` as a cheap static pre-flight under
  Step 6 "Stream, don't buffer".

## [0.10.2] - 2026-05-06

### Added
- **Memory-first preamble + Forge tools section in every skill.** Restored
  the two H2 sections held back from the 0.10.1 merge — `## Before you
  start (memory-first)` (calls `ferrosa-memory.hybrid_search` /
  `retrieve_skills_for_context` / `check_intentions` before grep/read)
  and `## Forge tools` (per-skill mapping from each step to its `frg`
  command). 52 SKILL.md files have memory-first restored, 51 have the
  Forge-tools table.
- **`## ferrosa-memory Integration` section** in
  `skills/quality/skill-dev-methodology-quality-gates.v2.md` — codifies
  the session-start protocol, the `frg fmem-skill-ingest` typed-bridge
  contract, and the progressive-disclosure rule that the 4-tool memory
  core (`hybrid_search` / `smart_ingest` / `create_edge` /
  `check_intentions`) ships inline while escape-hatch tools defer to
  supplementary files.

## [0.10.1] - 2026-05-06

### Added
- **`frg glob` subcommand + MCP tool.** File discovery with stats — globs
  source paths under a project root, returns per-file size, line count,
  and symbol summary. Path traversal (`..`) and a non-overridable
  secret-file denylist enforced at the boundary. (Originally shipped on
  the parallel `feature/glob-fn-desctiptions` branch.)
- **`frg ingest-descriptions` subcommand.** LLM-backed entity descriptions
  (v1) using a local Ollama instance. Resolves a target file to its
  project root, probes Ollama for available models with a remediation
  banner when the probe fails (`ollama pull` named explicitly), and
  validates every returned description against the schema (≤60 words,
  prompt-leak rejection, redaction count tracked through Provenance).
- **`specs/feat-code-graph-ingest/` blueprint.** Pre-implementation
  design docs for the T1–T15 code-graph ingest series shipped on
  `feat/code-graph-ingest-t11-cache` (PR #55) — overview, project plan,
  compiled execution plan with DAG, FMEA, Rust hazards, ferrosa-memory
  CRUD dependency, and the existing-work-items gap scan that drove the
  sprint sequencing. Status header points at the shipped code as the
  source of truth; specs retained for design traceability and FMEA
  reference.

### Fixed
- **Fail-loud audit on the descriptions pipeline.** Subprocess stderr is
  now surfaced through the error chain instead of being silently
  discarded, and every silent fallback is logged with the path and reason
  (permission denied, stale NFS mount, concurrent deletion).

### Changed
- `cargo fmt --all` pass across the descriptions / glob / ingest /
  paper / skill-ingest crates so `cargo fmt --check` passes in CI. No
  functional changes — line wrapping normalized.
- Merged `main` (0.10.0 — t11-cache code-graph-ingest line) into the
  parallel `feature/glob-fn-desctiptions` branch (0.7.x). Workspace
  bumps 0.10.0 → 0.10.1; the 0.7.x release line is collapsed into this
  entry because the two parallel tracks reconcile here.

## [0.10.0] - 2026-05-05

### Changed
- **Merge `main` into the code-graph-ingest line.** Brings the 0.6.4 PDF
  ingestion pipeline (XY-Cut++ reading-order reconstruction for multi-column
  academic papers) and the `ingest_paper` / `ingest_url` CQL persistence into
  the t11-cache branch.
- **`persist_or_report` ported to MCP transport.** main's helper called the
  removed `forge_ingest::loader::load` (Python/CQL loader, retired in 0.8.0).
  Rewritten to dispatch through `load_report_via_mcp`, preserving both the
  PR #54 abstraction and the 0.8.0 transport unification. Behavior matches
  the existing `ingest` tool — dry-run returns the IngestReport, otherwise
  the load report from ferrosa-memory.

## [0.9.1] - 2026-05-05

### Changed
- **fail-loud-scan: extract `FindingKind` for static finding metadata.** The
  per-pattern `id` / `category` / `severity` / `confidence` / `recommendation`
  tuple is now a `FindingKind` struct with one `const` per pattern. The
  `Scanner::push` helper drops from eight arguments to three, eliminating the
  clippy `too_many_arguments` finding without an `#[allow]`. Behavior is
  unchanged — same finding ids, same evidence, same severities.

## [0.9.0] - 2026-04-27

### Added
- **Dependency-aware Forge checklists.** Checklist items now support optional
  `depends_on`, `batch`, `verification`, `source_refs`, `claimed_by`, and
  `lease_expires_at` fields while remaining backward compatible with existing
  flat checklist JSON.
- **DAG validation and scheduling.** `frg checklist validate` detects missing
  dependencies, duplicate item ids, self-dependencies, and cycles. `ready`
  returns pending items whose dependencies are complete. `claim` atomically
  marks ready items `in_progress` with an agent lease, and `release` returns a
  claimed item to `pending`.
- **MCP checklist scheduler modes.** `checklist_state` now supports
  `create_dag`, `validate`, `ready`, `claim`, and `release` in addition to the
  existing flat checklist operations.
- **`fail-loud-scan` crate + `frg fail-loud-scan` CLI + MCP tool.** AST-based
  scanner (tree-sitter for C#, Elixir, Go, Python, Rust, Swift, TypeScript,
  Java) for swallowed errors, fake success, runtime mock data, placeholder
  implementations, and optimistic status returns. Aliases: `fail_loud`,
  `footguns`.
- **Platform TLS verifier in HTTP fmem-client.** `fmem-client` now configures
  `ureq` with `RootCerts::PlatformVerifier`, fixing TLS verification on
  systems where the bundled root store does not match the operating
  system's trust anchors.
- **`forge-semver-install-reminder.py` post-install hook.** Reminds operators
  to run `cargo install --path crates/cli` after a forge version bump so
  the locally installed `frg` binary tracks the workspace version.

### Changed
- **Manual status changes clear stale claims.** `frg checklist set` clears
  `claimed_by` and `lease_expires_at` when moving an item between states,
  preventing completed or blocked items from retaining stale leases.
- **Cosmetic rustfmt pass over `ingest` and `fmem-client`.** Line wrapping
  normalized; no functional changes.

## [0.8.6] - 2026-04-22

### Fixed
- **HTTP 504 on large ingest chunks.** The byte-only chunk cap (1 MiB)
  packed ~500 entity+edge rows into one request; each row is a CQL
  round-trip server-side, and slow clusters or upstream proxies hit
  their response window before ferrosa-memory could answer. Added a
  row-count cap — `DEFAULT_MAX_ROWS_PER_CHUNK = 100` — that flushes
  the chunk when either cap (bytes or rows) is reached, whichever
  comes first.

### Added
- **`GraphLoader::with_max_rows_per_chunk(n)`** — programmatic override
  for the row cap (min 1).
- **`[client] max_rows_per_chunk` config entry** in
  `~/.config/ferrosa-memory.toml` — lets you tune the cap without a
  rebuild. Lower for slow servers, higher when you've proven throughput.

### Behavior

With the default, a 3,000-row batch splits into ~30 chunks instead of
6 — more round trips, but each finishes well before any realistic
proxy deadline.

## [0.8.5] - 2026-04-22

### Fixed
- **HTTP default timeout raised 30s → 300s.** A single `ingest_entities`
  chunk can hold up to `MAX_PAYLOAD_BYTES` (1 MiB) of entities + edges.
  Each row is a CQL round-trip server-side; at ~500 rows per chunk and
  ~10 ms per upsert, 30s was too tight. Ingest calls that succeeded at
  auth were timing out mid-chunk.

### Added
- **`[client] http_timeout_ms` config override** in
  `~/.config/ferrosa-memory.toml`. Lets you tune the per-call deadline
  without touching code — useful for very large batches or slow CQL
  clusters. Example:
  ```toml
  [client]
  http_timeout_ms = 600000   # 10 minutes
  ```

## [0.8.4] - 2026-04-22

### Added
- **`[client]` section in `~/.config/ferrosa-memory.toml`** — forge now reads
  HTTP Basic credentials from:
  ```toml
  [client]
  http_username = "ferrosa_user"
  http_auth_value = "<plaintext>"
  ```
  The server's `auth_file` stores SHA-256 of the password; the client transmits
  plaintext over HTTP Basic and the server hashes+compares. Plaintext at the
  client is the standard cost for HTTP Basic — same trust class as `.netrc`
  or ssh keys in your home directory. Previous env-var discovery
  (`FERROSA_MEMORY_HTTP_USER` / `FERROSA_MEMORY_HTTP_PASS`) still works and
  takes precedence for scripted/CI runs.
- **Transport label in ingest logs** now reports `(authed)` or `(no-auth)`
  so you can tell at a glance whether credentials were sent.

### Migration
If your ferrosa-memory HTTP server has `auth_file = ...` configured (inspect
its `server.auth_file` runtime config), add a `[client]` section to
`~/.config/ferrosa-memory.toml` with the matching username/password. If
`auth_file` is absent, no client credentials are needed.

## [0.8.3] - 2026-04-22

### Fixed
- **MCP `ingest` tool now surfaces the full error chain** when `ingest_entities` fails. The prior handler did `.map_err(|e| e.to_string())`, which for `anyhow::Error` returns only the topmost `.with_context(...)` wrapper — hiding the underlying server-side cause. A failure chunk 0 would show only `"ingest_entities call failed for chunk 0"` with no hint whether the cause was HTTP 500, a JSON-RPC tool error (code -32602, "bad arg"), a timeout, or a schema mismatch. Switched to `format!("{e:#}")` which renders the full chain (top: middle: root), so callers see e.g. `ingest_entities call failed for chunk 0: fmem tool error (code -32602): entities[3].attrs.source_hash: invalid type ...`.

## [0.8.2] - 2026-04-22

### Fixed (critical)
- **`frg ingest` MCP tool no longer silently returns extraction counts.** In 0.8.0/0.8.1 the tool returned an `IngestReport` (extraction counts) when `mcp_bin` was absent, even though callers expect load counts from a tool named `ingest`. Now the tool auto-discovers the ferrosa-memory HTTP endpoint from `~/.config/ferrosa-memory.toml` and ingests through it. If no transport resolves (neither `mcp_bin` nor a configured HTTP endpoint), the tool **errors out** rather than returning misleading numbers. Use `dry_run: true` if you explicitly want extraction-only output.

### Added
- **`HttpTransport`** in `fmem-client` — JSON-RPC over `POST /mcp` against a running ferrosa-memory HTTP server. Supports optional HTTP Basic auth via `FERROSA_MEMORY_HTTP_USER` / `FERROSA_MEMORY_HTTP_PASS` env vars. Strict id matching per JSON-RPC 2.0 (FMEA F14). Default 30s timeout; configurable via `HttpConfig::timeout`.
- **Config-driven transport resolution** — forge reads `[server] transport / bind_addr / http_port` from `~/.config/ferrosa-memory.toml`. When `transport = "http"`, forge builds an `HttpTransport` against `bind_addr:http_port` automatically. Explicit `--mcp-bin` (stdio) always wins over config-driven HTTP.
- **`frg ingest --dry-run`** flag — extraction-only output (matches the existing `--dry-run` on `ingest-url` / `ingest-paper`). The MCP `ingest` tool also accepts `dry_run: true` for the same purpose.
- **`GraphLoader::from_dyn`** constructor — accepts `&dyn Transport` directly. Used when the caller holds a trait-object (e.g. enum-dispatched stdio/HTTP selection).

### Dependencies
- `ureq = "3"` and `base64 = "0.22"` added to `fmem-client` (HTTP client + Basic auth encoding).

### Migration
- If your other session's forge MCP server is running with a configured HTTP fmem (the default for most setups), **no action needed** — ingest now routes through it automatically.
- If you want extraction-only behavior, add `"dry_run": true` to the MCP `ingest` call or pass `--dry-run` to the CLI.

## [0.8.1] - 2026-04-22

### Added
- **`count_entities_by_type` wire wrapper** in `fmem-client` — typed `CountEntitiesByTypeArgs` / `CountEntitiesByTypeResponse` over the server tool shipped at `../ferrosa-memory/specs/implemented/feat-count-entities-by-type.md`. Response carries `total`, `by_entity_type`, `by_state`, `by_type_and_state`, `duration_ms`. `CountEntitiesByTypeResponse::assert_invariant()` gives callers a defense-in-depth check of the server's sum-of-breakdowns promise.
- **`frg context-check` breakdown restored** — prints the 6-bucket status line (code / docs / sections / bugs open / bugs resolved) that 0.7.x lost. Uses the new MCP tool; `--mcp-bin <path>` flag falls back to `FERROSA_MEMORY_MCP_BIN` env, then `which ferrosa-memory`. Best-effort — silently skips if nothing resolves, so the command stays safe in status-line hooks.

### Dependencies
- Added `which = "7"` to `crates/cli` for best-effort MCP binary discovery in ContextCheck (already a workspace-level dep via forge-ingest).

## [0.8.0] - 2026-04-22

### Removed (breaking)
- **Python CQL loader** (`crates/ingest/src/loader.rs`) is gone. All ingest writes now flow exclusively through the MCP `ingest_entities` path added in 0.7.0. This eliminates the `cassandra-driver` Python dependency, the silent `except: pass` edge drop, and the opaque "CQL loader failed (exit status 1)" error class — the proximate cause of the 0.6.x ingest failure the caller hit.
- **`--cql host:port` flag** removed from `frg ingest`, `frg ingest-url`, `frg ingest-paper`, and the corresponding MCP tools. Use `--mcp-bin <path>` instead, pointing at the ferrosa-memory MCP binary. Without `--mcp-bin` the subcommands print the extracted report and exit (same as no-flag behavior in 0.7.x).
- **`--graph-loader` flag** removed — redundant since the MCP path is now the only path. Any scripts passing `--graph-loader` should drop the flag and add `--mcp-bin <path>`.
- **`count_entities` status breakdown** in `frg context-check` is temporarily a no-op, pending the fmem-side `count_entities_by_type` tool (spec filed at `../ferrosa-memory/specs/todo/feat-count-entities-by-type.md`). ContextCheck does not re-open the direct-CQL path as a fallback — the whole point of this release is to retire it.
- `FerrosaMemoryConfig.cql_contact_point` and related TOML parsing removed.

### Added
- **Shared `load_report_via_mcp` helper** in the CLI — every ingest subcommand now funnels through one function that spawns `StdioTransport`, runs `GraphLoader`, and logs counts. No more four copies of the same try-CQL-else-print branch.

### Migration
- Replace `frg ingest <path> --cql localhost:19042` → `frg ingest <path> --mcp-bin $(which ferrosa-memory)`.
- Replace `frg ingest <path> --graph-loader --mcp-bin <path>` → `frg ingest <path> --mcp-bin <path>`.
- The `~/.config/ferrosa-memory.toml` file's `[ferrosa] contact_points` entry is no longer read by forge. Tenant/session values under `[server]` are still honored.

## [0.7.0] - 2026-04-22

### Added
- **Code-graph ingest**: LSP-backed extraction of functions, methods, types, traits, and parameters with rich edge types (`calls`, `references`, `has_type`, `implements`, `defined_in`, `imports`, `documents`). Tracks call sites with file/line/col metadata and per-edge aggregation.
- **New LSP methods** in `crates/ingest/src/lsp.rs`: `prepare_call_hierarchy`, `call_hierarchy_incoming`, `call_hierarchy_outgoing`, `references`, `type_definition`, `implementation`, `hover`, `definition`.
- **Capability probe + per-method timeouts**: `LspSession::capabilities()` records which providers the server advertises; `request_if_supported()` short-circuits to `Ok(None)` with no round-trip when a capability is missing. `FORGE_LSP_TIMEOUT_*_MS` env overrides per method.
- **UTF-8 position encoding negotiation** at LSP init; `position::LineIndex` converts UTF-16/UTF-32 responses to byte offsets safely.
- **`SourceBuffer`** (`crates/ingest/src/source_buffer.rs`): single-read file access with streaming sha256, strict UTF-8 validation, and 128 KiB cap on body storage for the `file` entity.
- **`file` entity emission** with path / sha256 / source_text / bytes / lines / truncated attrs.
- **Entity/Edge schema additions**: optional `source_text`, `start_byte`, `end_byte`, `start_line`, `end_line`, `visibility`, `signature`, `doc`, `source_hash`, `truncated`, `bytes`, `lines`, `extractor_schema_version`, plus per-edge `metadata` JSON. `EXTRACTOR_SCHEMA_VERSION` constant decoupled from crate semver.
- **`graph_loader`**: MCP client for `ingest_entities` with strict per-chunk count reconciliation, topological edge placement (edges ship with the later of their src/dst endpoint), 1 MiB payload chunking, and individual-row retry on silent drops.
- **`batch_update_entities` + `batch_delete_entities`** in `fmem-client`: typed wrappers for the new CRUD tools with client-side 100-entry cap enforcement.
- **Refresh decision layer** (`crates/ingest/src/refresh.rs`): `classify()` produces `New` / `Changed` / `Unchanged` / `VersionDrift` / `Missing` / `Renamed` / `PendingRecovery` decisions from a filesystem walk vs. on-disk cache state.
- **`apply_decisions`** dispatches refresh decisions to the live ferrosa-memory tools: renames use `batch_update_entities`, deletions use `batch_delete_entities`, extractable decisions are tallied for the pending T12b orchestrator.
- **Local refresh cache** (`crates/ingest/src/cache.rs`): per-project TOML at `.forge/cache/code-graph/<project-id>.toml` with `fs2` advisory exclusive lock, atomic temp-file+rename writes, and `pending_file_id` write-ahead marker for mid-refresh crash recovery.
- **Visibility inference** from LSP detail strings (Rust `pub`/`pub(crate)`/`pub(super)`/`pub(in path)`, Java/TS/C# `public`/`protected`/`private`/`internal`).
- **Rust signature parser** for parameter extraction (generics, `impl Fn(...)`, arrows `->`, path types with `::`, receivers).
- **Hover response parser** handling all three LSP `Hover.contents` shapes: `MarkupContent`, `MarkedString[]`, bare `MarkedString`.
- **Import resolution scaffolding**: `resolve_import_target()` + `build_import_edge()` emit resolved `imports` edges via `textDocument/definition` or fall back to `imports_string` metadata.

### Fixed
- **`IngestEntitiesResponse` shape**: the 0.6.x flat `{requested, succeeded, skipped, failed}` was incorrect and would have failed deserialization against the real ferrosa-memory server. Now matches the deployed contract: nested `{entities, edges, embeddings}` sub-envelopes with independent `accounted()` reconciliation per kind. `entities_updated` in `LoadReport` is now populated accurately (was always 0).

### Dependencies
- Added `fs2 = "0.4"` (advisory file locking).
- Added `url = "2"` (safe file:// URI construction).

### Safety
- `#[allow(dead_code)]` on four `LspSession` test-only constructors that are future-used by timeout/capability integration tests.
- Drop `source[a..b]` direct slicing in favour of `source.get(a..b)` and `position::slice_source()` to eliminate panic paths on non-ASCII source.

## [0.6.4] - 2026-05-05

### Added
- **PDF ingestion pipeline** (`crates/ingest/src/pdf/`): XY-Cut++ reading order reconstruction for multi-column academic PDF layouts. Detects cross-layout headers/footers, segments columns via gap detection, and merges elements in correct reading order.
- **Persistence for `ingest_paper` and `ingest_url`**: Both tools now persist extracted knowledge to ferrosa-memory via CQL when configured (explicit `cql` arg or `ferrosa-memory.toml`). Previously only `ingest` (codebase) persisted; `ingest_paper`/`ingest_url` returned phantom session IDs.

### Fixed
- **PDF reading order Y-axis inversion**: Fixed `try_y_cut` and cross-layout merging to correctly handle PDF coordinates where Y increases upward (bottom-left origin). Previously sorted bottom-to-top, producing reversed reading order.

> Note: 0.6.4 shipped on `main` in parallel with the 0.7.0–0.9.x line on the
> code-graph-ingest branch. Versions are out of chronological order in this
> file because the two release tracks were renumbered when merged.

## [0.6.3] - 2026-04-06

### Added
- **Content sanitization module** (`crates/ingest/src/sanitize.rs`): Defends against prompt injection, hidden text, suspicious Unicode, and encoded content in web-sourced ingestion. Blocks and warns on detection.
  - Pass 1: Strip hidden HTML (display:none, visibility:hidden, aria-hidden, opacity:0, off-screen positioning)
  - Pass 2: Remove suspicious Unicode (zero-width chars, RTL overrides, tag characters, homoglyphs)
  - Pass 3: Detect prompt injection patterns (instruction overrides, role manipulation, system prompt extraction, delimiter injection, jailbreak attempts, data exfiltration)
  - Pass 4: Strip encoded content (base64 blobs, data URIs)
  - Pass 5: Enforce length limits (5000 char max per entity context)
- Sanitization applied to both `ingest_url` and `ingest_paper` pipelines
- Entity names validated separately with stricter limits (200 char max, injection detection)
- Blocked entities and their edges are removed from the report
- 19 tests covering all sanitization passes

## [0.6.2] - 2026-04-06

### Fixed
- **Author name dedup**: Normalize all author names to "First Last" format before ingestion. Handles "Last, First" → "First Last" conversion. Prevents duplicate person entities for the same author across papers.
- **Arxiv author extraction**: Use arxiv search URL query params for canonical author names instead of raw `citation_author` meta tags.
- **LSP symbol names**: Use qualified names (`path/to/file.rs:symbol`) instead of bare names (`new`, `fmt`) to prevent dedup collisions across crates.

## [0.6.1] - 2026-04-05

### Fixed
- **Dangling edge references**: Ingest was creating edges where 90% of destination IDs referenced non-existent entities (external deps, stdlib modules). Now validates all edges against the entity set and drops those with missing endpoints.

## [0.6.0] - 2026-04-05

### Added
- **ingest_url command**: Fetch web pages and extract structured content (headings, concepts, links) into a knowledge graph. SSRF prevention, 5MB body limit, sensitive query param stripping. Supports `--depth 0/1/2` same-domain crawling (max 20 pages).
- **ingest_paper command**: Ingest academic papers from arxiv, Semantic Scholar, IEEE, ACM, bioRxiv, PubMed, DOI, or local PDFs. Extracts authors, references, concepts, sections. Opens paywalled sources in browser for authenticated access.
- **Progressive disclosure**: Three-tier MCP tool filtering reduces token overhead ~60%. Tier 1 always visible, tier 2 auto-detected by project stack, tier 3 hidden but callable.
- **LSP symbol extraction**: Functions, structs, traits extracted via rust-analyzer during codebase ingest.
- **IngestSummary.code_symbols field**: Tracks LSP-extracted entities in summary counts.

### Fixed
- **MCP ingest test isolation**: Uses temp HOME to avoid picking up local ferrosa-memory.toml config.

## [0.5.4] - 2026-03-22

### Fixed
- **MCP analytics negative token savings**: Command-wrapping MCP tools (cargo, go, mix, npm, python, docker) now record raw command output size as `input_bytes` instead of the tiny argument JSON size, producing correct positive savings ratios
- **test-summary false positives**: Commands with `--no-run`, `--list`, or `--list-tests` flags now skip the test-summary filter and fall through to log-distill, preventing parse failures on non-test output
- **Filesystem-analysis MCP tools excluded from savings**: Tools that generate output from filesystem analysis (project_detect, digest, dsm, etc.) no longer record misleading entries in filter_log

### Added
- `InvocationMode::Mcp` variant for proper analytics mode tracking
- `exclude` field on filter rules for pattern-based exclusion (e.g., skip test-summary when `--no-run` is present)
- `raw_input_bytes` field on `ToolOutput` carrying raw command output size through the MCP layer

## [0.5.3] - 2026-03-22

### Changed
- **MCP tool descriptions**: Enriched all ~25 tool descriptions with use cases, output specs, parameter examples, and cross-references to related tools — following Anthropic's tool description guidance for better model tool selection

## [0.5.2] - 2026-03-15

### Fixed
- **diff-filter trailing newline inflation**: Output was consistently 1 byte larger than input when input lacked a trailing newline, due to `.lines()` stripping then unconditionally re-adding `\n` to every line including the last. Affected ~28% of diff-filter invocations (53/186 tracked). Now preserves the input's trailing-newline convention.

## [0.5.1] - 2026-03-13

### Fixed
- **log-distill output inflation**: Filter now returns original input when distilled JSON would be larger than the input, preventing 2-6x output expansion on `gh run view --log-failed`, `cargo clippy`, and `cargo test` output
- **log-distill false positives**: Tightened `is_error()` to require `error:` or `error[` prefix instead of matching `contains("error")` anywhere in a line; same for `is_warning()` requiring `warning:` prefix
- **log-distill success summary exclusion**: Lines like "0 errors", "test result: ok", "Finished dev", and `--explain` hints are no longer classified as errors/warnings
- **log-distill minimum input threshold**: Inputs below 100 bytes skip distillation entirely (JSON envelope overhead exceeds any savings)

## [0.5.0] - 2026-03-11

### Added
- **tool_version tracking**: `filter_log` now records the frg version that created each entry, enabling version-aware analytics and stale-failure triage
- **clear-analytics subcommand**: Delete all `filter_log` and `command_log` rows to reset analytics after upgrades
- **schema migration**: `open_db()` auto-detects and migrates older databases (adds missing columns via ALTER TABLE)

### Fixed
- **gain report now includes hook data**: Previously only queried `command_log` (run mode), missing 100% of hook-mode invocations; now unions both tables
- **filter detection: pipe truncator fallback**: Commands piped through `grep`, `tail`, `head`, etc. now use the fallback filter instead of a specialized filter that would fail on truncated output
- **filter detection: `&&`/`;` chain splitting**: Normalizer now extracts the last command segment from shell chains, preventing false matches from earlier segments (e.g., `cargo fmt && cargo test` correctly detects as test-summary)

## [0.4.0] - 2026-03-11

### Added
- **devtools crate**: Structured MCP tool wrappers for language build/test/lint commands
  - `cargo` (build/check/test/clippy/fmt_check)
  - `go_tools` (build/test/vet/fmt_check/mod_tidy)
  - `mix_compile`, `mix_test`, `mix_format_check`, `mix_deps` (Elixir)
  - `npm_tools` (test/typecheck/lint/format_check/deps/build/audit)
  - `python_tools` (test/lint/format_check/deps/typecheck)
  - `docker_status`, `git_summary`, `ci_cd` (infrastructure)
- **ToolOutput base struct**: Shared result type guaranteeing `hint` and `raw_output` on any failure, preventing LLM stalls on unrecognized errors
- **Full MCP coverage**: Exposed all remaining CLI tools via MCP (31 total, up from 21)
  - `version`, `test_summary`, `log_distill`, `diff_filter`, `lint_dedup`
  - `log_monitor`, `coverage_gate`, `doc_coverage`, `concurrency_scan`, `dsm`
- **Registration macros**: `register_tool!`, `register_path_tool!`, `register_stdin_tool!` reduce per-tool boilerplate from ~20 lines to ~5
- **smell-detect**: Expanded 12-factor config detection to all hardcoded values
- **analytics**: Fixed aggregate ratio for consistent SAVED_TK and RATIO metrics

## [0.3.0] - 2025-12-20

### Added
- **format-fix crate**: Auto-fix formatting issues across languages
- **merge-check crate**: Validate merge readiness
- **smell-detect crate**: 12-factor config smell detection

## [0.2.0] - 2025-12-15

### Added
- **outline crate**: Module outline extraction
- **dep-tree crate**: Dependency tree analysis with subdirectory detection
- **mcp-server crate**: MCP server infrastructure
- **project-summary**: Project detection and summarization
- Elixir type extraction support

## [0.1.0] - 2025-12-01

### Added
- Initial release with core crates: cli, shared, test-summary, log-distill, diff-filter, lint-dedup, log-monitor, coverage-gate, doc-coverage, project-detect, digest, dsm-analyze, concurrency-scan
