# feat-code-graph-ingest — Compiled Execution Plan

> Agent-executable plan. Each task is a self-contained work packet with inputs, outputs, verification, and guard references to `fmea.md` (Fn) and `rust-hazards.md` (Pn-m).
> **Model guidance:** `[sonnet]` tasks are well-scoped implementation — dispatch to Sonnet. `[opus]` tasks need design judgment — keep with primary session. `[sonnet+review]` means Sonnet implements, primary reviews before commit.
> Siblings in the same batch may run in parallel.

## Dependency DAG

```text
                     ┌──────────────────────────┐
                     │  T1  Schema + encoding   │  [opus]
                     │  (Entity/Edge attrs,     │
                     │   utf-8 negotiation,     │
                     │   file entity)           │
                     └────────────┬─────────────┘
                                  │
             ┌────────────────────┼────────────────────┐
             ▼                    ▼                    ▼
    ┌────────────────┐   ┌────────────────┐   ┌────────────────┐
    │ T2 ingest_     │   │ T3 Capability  │   │ T4 Oversize +  │
    │    entities    │   │    probe +     │   │    atomic read │
    │    client      │   │    timeouts    │   │    [sonnet]    │
    │    [opus]      │   │    [sonnet]    │   │                │
    └───────┬────────┘   └────────┬───────┘   └────────┬───────┘
            │                     │                    │
            └────────┬────────────┼────────────────────┘
                     ▼            ▼
          ┌──────────────────────────────────┐
          │ Tier 1 — call/ref/type/impl      │
          │                                  │
          │  T5 callHier  T6 references      │
          │  T7 typeDef   T8 implementation  │
          │  [sonnet each]                   │
          └───────────────┬──────────────────┘
                          │
             ┌────────────┼────────────┐
             ▼            ▼            ▼
     ┌────────────┐ ┌───────────┐ ┌────────────┐
     │ T9  Private│ │T10 Topo   │ │ T11 Cache  │
     │    symbols │ │    batch  │ │    file +  │
     │   [sonnet] │ │   [sonnet]│ │    lock    │
     └──────┬─────┘ └─────┬─────┘ │   [sonnet] │
            │             │       └─────┬──────┘
            └─────┬───────┴─────────────┘
                  ▼
         ┌────────────────────────────────┐
         │ T12  Refresh (hash diff + 1-hop│
         │      closure + rename/delete + │
         │      WAL + version)  [opus]    │
         └────────────────┬───────────────┘
                          │
             ┌────────────┼────────────┐
             ▼            ▼            ▼
     ┌────────────┐ ┌───────────┐ ┌────────────┐
     │T13 hover/  │ │T14 import │ │T15 param   │
     │   doc      │ │   resolve │ │   entities │
     │   [sonnet] │ │   [sonnet]│ │   [sonnet] │
     └──────┬─────┘ └─────┬─────┘ └─────┬──────┘
            │             │             │
            └─────────────┼─────────────┘
                          ▼
               ┌──────────────────────┐
               │ T16  MCP ingest_refr │  [sonnet]
               │ T17  Benchmark+CI    │  [sonnet]
               │ T18  Docs            │  [sonnet]
               └──────────────────────┘
```

## Parallel batches

| Batch | Tasks | Gating |
|---|---|---|
| B0 | T1 | foundation — must land first |
| B1 | T2, T3, T4 | all depend on T1 only |
| B2 | T5, T6, T7, T8 | all depend on B1 complete |
| B3 | T9, T10, T11 | depend on B2 complete |
| B4 | T12 | depends on B3 (refresh reads from cache T11 + topo batch T10) |
| B5 | T13, T14, T15 | depend on T12 OR on B2 + B3 (T15 only needs Tier 1) |
| B6 | T16, T17, T18 | depend on everything else |

## Task packets

Every task below uses the same schema so a Sonnet agent can pick one up cold:

- **Deliverable** — what exists after the task
- **Files** — where changes land
- **Depends** — prior task ids
- **Guards** — FMEA + hazards ids the task must satisfy
- **Verification** — concrete `cargo test` patterns that must pass before marking complete
- **Out of scope** — explicit non-goals to prevent scope creep

---

### T1 — Schema foundations + utf-8 encoding [opus]

- **Deliverable:** `Entity` and `Edge` structs carry the new shape; LSP init negotiates `positionEncodingKind: ["utf-8", "utf-32", "utf-16"]`; on utf-16 fallback, a conversion helper produces byte offsets from line-index; `file` entity is emitted per ingested source file.
- **Files:**
  - `crates/ingest/src/extractor.rs` — extend `Entity` struct; add `emit_file_entities()` pass
  - `crates/ingest/src/lsp.rs` — extend `initialize` params; add `negotiated_position_encoding: PositionEncoding` on `LspSession`; add `pos_to_byte(&self, pos: Position) -> Option<usize>` helper
  - `crates/ingest/src/lib.rs` — `pub` new types
- **Depends:** none
- **Guards:** F2 (RPN 432), F7 (RPN 360), P0-1, P0-2, P1-3
- **Invariants:**
  - No unchecked `source_text[a..b]` — all slicing goes through `source_text.get(a..b).ok_or(...)`.
  - Every symbol range asserts `end_byte <= source_text.len()` before storage.
  - File larger than `MAX_FILE_BYTES` (128 KiB) ⇒ `source_text = None`, `truncated = true`, and no per-symbol byte ranges are emitted for that file (symbols still carry line ranges for hover/nav).
- **Verification:**
  - Unit: `lsp::negotiates_utf8_when_advertised`, `lsp::falls_back_to_utf16_and_converts`, `extractor::emits_file_entity_with_sha256`, `extractor::oversized_file_has_no_source_text_no_byte_ranges`
  - Property: for any fixture file, every stored symbol `source_text[range]` is a valid char boundary slice
- **Out of scope:** calling the new LSP methods; schema changes to anything outside `ingest`.

---

### T2 — Replace code-entity load path with `ingest_entities` MCP client [opus]

- **Deliverable:** A new `crates/ingest/src/graph_loader.rs` (or equivalent) that submits entities + edges via the `ingest_entities` MCP tool on ferrosa-memory. **Count reconciliation is enforced.** `loader.rs` (Python) remains for non-code ingest only.
- **Files:**
  - `crates/ingest/src/graph_loader.rs` — new
  - `crates/ingest/src/lib.rs` — wire new loader behind `IngestMode::CodeGraph`
  - `crates/fmem-client/src/tools/` — add `ingest_entities` request/response types
- **Depends:** T1
- **Guards:** F3 (RPN 567), F12, P0-6
- **Invariants:**
  - `response.inserted + response.updated + response.skipped + len(response.failed) == len(entities_sent)`. On mismatch, retry failed rows individually; if still unreconciled, **hard-fail the run** (non-zero exit, no silent success).
  - Edge batches never reference entity ids that weren't in this or an earlier committed batch (topological ordering — see T10 for split logic).
  - Payload chunking at 1 MiB hard limit.
- **Verification:**
  - Integration (mock MCP): drops every 3rd row → loader detects, retries, eventually hard-fails with actionable error
  - Integration: 10k-entity batch chunked correctly, all entities accounted for
- **Out of scope:** schema changes on the server side; deletion tool.

---

### T3 — LSP capability probe + per-request timeouts [sonnet]

- **Deliverable:** At `LspSession::start`, inspect `initializeResult.capabilities` and record which of `callHierarchyProvider`, `referencesProvider`, `typeDefinitionProvider`, `implementationProvider`, `hoverProvider` are present. Per-method timeouts wrap every request.
- **Files:**
  - `crates/ingest/src/lsp.rs` — `ServerCapabilities` struct on `LspSession`; `request_with_timeout(method, params, timeout) -> Result<Value>` helper
- **Depends:** T1
- **Guards:** F6 (RPN 336), F1 (RPN 336), hazards P0-5
- **Timeouts (config defaults, overridable via env):**
  - `references` — 10s
  - `hover` — 5s
  - `callHierarchy/*` — 15s
  - `typeDefinition` / `implementation` — 5s
- **Invariants:**
  - Missing capability ⇒ method returns `Err(CapabilityUnsupported)` **immediately** (no round trip); caller logs loud warning and sets `report.tier1_degraded: true`.
  - File entity gets `extraction_mode: "full" | "symbols_only" | "partial"` attr so queries can filter.
- **Verification:**
  - Unit: `init_without_call_hierarchy_marks_degraded`, `request_respects_timeout`
  - Integration against a mock LSP that sleeps past timeout → returns `TimedOut`, other calls unaffected.
- **Out of scope:** implementing the new methods themselves (T5–T8).

---

### T4 — Oversize-file + atomic-read handling [sonnet]

- **Deliverable:** A single `read_source(path) -> Result<SourceBuffer>` that reads once, returns `{bytes, sha256, text_or_none, truncated, lines, byte_length}`. All downstream consumers (file entity, LSP `didOpen`, symbol range slicing) use the same `SourceBuffer`. Files larger than `MAX_FILE_BYTES` skip body storage entirely.
- **Files:**
  - `crates/ingest/src/source_buffer.rs` — new
- **Depends:** T1
- **Guards:** F7, F14 (RPN 245), F20, P1-3
- **Invariants:**
  - One file read per file per run. The LSP `didOpen` payload is `&SourceBuffer.text`, not a second `fs::read_to_string`.
  - Caller must **not** re-read the file between sha256 and LSP interaction.
- **Verification:**
  - Unit: `oversized_file_marks_truncated_no_text`, `read_once_hash_matches_text`, `binary_file_detected_and_skipped`
- **Out of scope:** mmap, incremental re-read on editor save.

---

### T5 — `callHierarchy` incoming + outgoing [sonnet]

- **Deliverable:** For each function/method symbol, issue `textDocument/prepareCallHierarchy` → `callHierarchy/incomingCalls` and `outgoingCalls`; emit `calls(caller→callee)` edges with metadata `{call_file, call_line, call_col, call_count}`.
- **Files:**
  - `crates/ingest/src/lsp.rs` — `pub fn prepare_call_hierarchy(...)`, `incoming_calls(...)`, `outgoing_calls(...)`
  - `crates/ingest/src/extractor.rs` — `emit_call_edges_for_symbol(...)`
- **Depends:** T3
- **Guards:** F1, F12, F16
- **Invariants:**
  - Result count capped at `MAX_REFERENCES_PER_SYMBOL` (500); on truncation, set `calls_truncated: true` on the symbol entity.
  - Aggregate by `(caller_id, callee_id)` to a single edge with `call_count`; store every call_site in metadata up to `MAX_CALL_SITES_PER_EDGE` (20) then flag `call_sites_truncated`.
- **Verification:**
  - Integration fixture: crate with known call graph (`fn a() { b(); b(); c(); }`) → expected edges + counts
  - Timeout path: mock LSP sleeps on one symbol → edge for that symbol skipped with log, others intact.
- **Out of scope:** typeDefinition, hover (T7, T13).

---

### T6 — `textDocument/references` [sonnet]

- **Deliverable:** Emit `references(referencing_symbol → target_symbol)` edges with `{ref_file, ref_line, ref_col}` metadata. Use LSP `includeDeclaration: false`.
- **Files:**
  - `crates/ingest/src/lsp.rs` — `pub fn references(...)`
  - `crates/ingest/src/extractor.rs` — `emit_reference_edges_for_symbol(...)`
- **Depends:** T3
- **Guards:** F1, F12
- **Invariants:** same cap/truncation rules as T5. Per-reference resolution to a target symbol done via `textDocument/definition` — if unresolvable, store `references` edge with `target_id: null` and `unresolved_target_name: <string>` metadata rather than dropping.
- **Verification:** integration fixture with known reference pattern.

---

### T7 — `textDocument/typeDefinition` + `has_type` edges [sonnet]

- **Deliverable:** For each function, method, and (later) parameter symbol, emit `has_type` edges to the type symbol.
- **Files:** `crates/ingest/src/lsp.rs`, `crates/ingest/src/extractor.rs`
- **Depends:** T3
- **Guards:** F17 (edge F17 is for `imports`, not `has_type`, but the same principle — resolve via LSP, don't guess)
- **Verification:** fixture with generic function → `has_type` edges point at generic parameter entities; non-generic → concrete type.

---

### T8 — `textDocument/implementation` → `implements` edges [sonnet]

- **Deliverable:** For each trait/interface symbol, query implementations. Emit `implements(concrete_type → trait)` edges.
- **Files:** `crates/ingest/src/lsp.rs`, `crates/ingest/src/extractor.rs`
- **Depends:** T3
- **Verification:** Rust fixture with trait + N impls → N `implements` edges; empty trait → zero edges with no error.

---

### T9 — Include private symbols [sonnet]

- **Deliverable:** Remove public-only filter at `extractor.rs:338`. Every symbol the LSP returns gets an entity with `visibility: "pub" | "crate" | "private"` attr derived from LSP `SymbolTag` or by parsing the symbol detail for `pub`/`pub(crate)`.
- **Files:** `crates/ingest/src/extractor.rs`
- **Depends:** B2 complete
- **Guards:** overview D3
- **Verification:** fixture with `fn pub_a()`, `pub(crate) fn b()`, `fn private_c()` → 3 entities with matching `visibility` values.

---

### T10 — Topological batching for `ingest_entities` [sonnet]

- **Deliverable:** `GraphBatch::split(max_bytes)` produces an ordered vec of batches where every edge's src/dst is either in that batch or a prior batch. Entities come before edges that reference them.
- **Files:** `crates/ingest/src/graph_loader.rs`
- **Depends:** T2
- **Guards:** F12, F3
- **Verification:** property test — for any random entity+edge set, `split` output respects the topological invariant.

---

### T11 — Local cache + exclusive lock [sonnet]

- **Deliverable:** `.forge/cache/code-graph/<project-id>.toml` stores `{path: {sha256, last_refreshed_at, extractor_version, pending_file_id?}}`. Access guarded by `fs2::FileExt::try_lock_exclusive`. Writes are atomic (temp file + rename).
- **Files:**
  - `crates/ingest/src/cache.rs` — new
  - `Cargo.toml` (forge-ingest) — add `fs2`
- **Depends:** T1
- **Guards:** F13
- **Verification:**
  - Integration: spawn two `frg ingest` processes against same project → second exits with `EAGAIN` and clear message.
  - Unit: `corrupted_toml_is_detected_and_reset_loudly` (not silently).

---

### T12 — Refresh pipeline [opus]

- **Deliverable:** `frg ingest <path> --refresh [--deep]` and a library `fn refresh(project, opts) -> RefreshReport`.
- **Files:**
  - `crates/ingest/src/refresh.rs` — new
  - `crates/cli/src/main.rs` — flag wiring
- **Depends:** T10, T11
- **Guards:** F8 (RPN 336), F9, F10, F11, F15
- **Pipeline:**
  1. Load cache. If cache `extractor_version < current`, mark all its files for re-extract regardless of hash (F15).
  2. Walk current tree. For each file: compute sha256. Compare to cache.
  3. **New** files → extract + add to cache.
  4. **Changed** (hash differs) files → re-extract.
  5. **Missing** (in cache, not on disk) → rename-detect: any new file with matching sha256? If yes, either re-emit the desired full file entity via `ingest_entities` or defer rename handling behind a partial-update dependency (F10). If no, record the deletion in the refresh report and stop short of hard delete until a dedicated delete or soft-delete contract exists (F9).
  6. **Unchanged** → skip.
  7. Write-ahead: before extracting a file, write `pending_file_id` to cache. Clear and update `sha256` + `last_refreshed_at` only after all its entities+edges commit successfully (F11).
  8. For each changed file, query ferrosa-memory for symbols with edges pointing *into* that file; mark those symbols' files for re-extract (1-hop closure, F8).
  9. `--deep`: iterate step 8 up to 5 times or until fixpoint; cycle guard via visited set.
  10. Emit `RefreshReport { scanned, changed, renamed, deleted, symbols_added, symbols_updated, symbols_stale, edges_added, edges_deleted, staleness_estimate, extractor_version_upgrade_files, duration_ms }`.
- **Verification:**
  - Fixture: ingest, touch one file → refresh re-extracts only it (+1-hop).
  - Fixture: `git mv` file → path updated, entity id preserved, inbound edges preserved.
  - Fixture: `rm` file → file + its symbols marked stale; external references preserved.
  - Fixture: `kill -9` mid-refresh → restart re-extracts the file whose `pending_file_id` was set; no files falsely marked "refreshed".
  - Property: after refresh, for every file whose sha256 is unchanged, no symbol in that file was written this run (unless forced by version bump or 1-hop closure).
- **Out of scope:** ferrosa-memory-side implementation of generic `delete_entities` / `update_entities`. This task must not assume they exist.

---

### T13 — `hover` → `doc` attr [sonnet]

- **Deliverable:** For each code symbol, issue `textDocument/hover`; store `doc` (string) and `doc_format` (`"markdown" | "plaintext" | null`). Handle all three LSP hover shapes: `MarkedString`, `MarkedString[]`, `MarkupContent`.
- **Files:** `crates/ingest/src/lsp.rs`, `crates/ingest/src/extractor.rs`
- **Depends:** T3
- **Guards:** F18, F19
- **Invariants:** hover content stored verbatim (no sanitization at ingest; downstream consumers must treat as untrusted).
- **Verification:** unit tests for each of the three hover response shapes + property test that no shape panics.

---

### T14 — Resolved `imports` edges [sonnet]

- **Deliverable:** For each import/use statement detected by the regex extractor, issue `textDocument/definition` on the import path to resolve it to a real entity id. Emit `imports(module → module)` edge if resolved; `imports_string` attr only if unresolved.
- **Files:** `crates/ingest/src/extractor.rs`, `crates/ingest/src/lsp.rs`
- **Depends:** Tier 1 complete (reuses `definition` call path from T6)
- **Guards:** F17 (RPN 280)
- **Verification:** fixture with shadowed names → edges point at LSP-resolved target, not first name match.

---

### T15 — `parameter` entities [sonnet]

- **Deliverable:** For each function/method, emit child `parameter` entities with `name`, `type_name`, `position`. Each parameter has a `has_type` edge to its type.
- **Files:** `crates/ingest/src/extractor.rs`
- **Depends:** T5, T7
- **Verification:** fixture `fn foo(x: u32, y: &str)` → two parameter entities with correct names, positions, `has_type` edges.

---

### T16 — MCP `ingest_refresh` tool [sonnet]

- **Deliverable:** New tool in the forge MCP surface that wraps `refresh()`. Returns the `RefreshReport` as JSON.
- **Files:**
  - `crates/mcp-server/src/tools/` — add `ingest_refresh.rs`
  - `crates/cli/src/main.rs` — ensure library-callable `refresh()` (no side-effects via main)
- **Depends:** T12
- **Verification:** MCP integration test: first call ingests, second call returns zero changes in < 1s on a 1k-file fixture.

---

### T17 — Benchmark + CI regression threshold [sonnet]

- **Deliverable:** `cargo bench` target on a 1k-symbol fixture; CI fails if ingest time > 2× committed baseline.
- **Files:**
  - `crates/ingest/benches/code_graph.rs` — new
  - `.github/workflows/` — wire into CI
- **Guards:** F16
- **Verification:** bench produces a baseline file; regression test runs in CI.

---

### T18 — Documentation [sonnet]

- **Deliverable:**
  - `crates/ingest/README.md` — code graph section, refresh semantics, Tier 1/2/3 capability matrix
  - `frg ingest --help` text updated
  - MCP tool descriptions (`ingest_refresh`) reflect the refresh contract
  - Brief note in `specs/feat-code-graph-ingest/decisions/` for D1–D5 once resolved
- **Verification:** `frg ingest --help` output contains the `--refresh` and `--deep` flags with descriptions.

---

## Guards cheat sheet (cross-reference)

| FMEA id | RPN | Addressed in |
|---|---|---|
| F1 | 336 | T3 (timeout), T5, T6 (cap) |
| F2 | 432 | T1 |
| F3 | 567 | T2, T10 |
| F4 | 210 | chunked sessions — add to T12 follow-up if > 500 files common |
| F5 | 224 | T12 write-ahead |
| F6 | 336 | T3 |
| F7 | 360 | T1, T4 |
| F8 | 336 | T12 1-hop + `--deep` |
| F9 | 294 | T12 missing-file detection |
| F10 | 294 | T12 rename heuristic |
| F11 | 256 | T12 write-ahead cache |
| F12 | 216 | T5, T6, T10 caps |
| F13 | 140 | T11 lock |
| F14 | 245 | T4 atomic read |
| F15 | 240 | T12 version bump |
| F16 | 96 | T17 benchmark |
| F17 | 280 | T14 |
| F18 | 168 | T13 note (verbatim storage, downstream sanitizes) |
| F19 | 84 | T13 shape parser |
| F20 | 81 | accepted — documented |

## Open decisions (surface during T2 / T12)

- **D1 — stale-symbol mechanism.** Resolved for now: `ferrosa-memory` ships `ingest_entities`, not a generic CRUD family. T12 should ship add/update refresh first and treat deletions as a separate upstream dependency or a later explicit soft-delete design. `ingest_entities` remains the bulk upsert path for new + changed.
- **D2 — `MAX_FILE_BYTES`.** 128 KiB is a starting guess. Revisit after T17 benchmark on a real project.
- **D3 — private symbols.** Confirmed always-on per user; T9 removes the toggle.
- **D4 — extractor_version format.** **Decoupled from crate semver.** Use a dedicated `EXTRACTOR_SCHEMA_VERSION: u32` constant in forge-ingest, bumped only on stored entity/edge shape changes (not on internal refactors). Additive changes keep the version; breaking changes (rename/remove field) bump it and force re-extract of affected files. Add ADR under `decisions/` when T12 lands.
- **D5 — LSP parallelism.** Sequential per-session for now (stateful protocol). Revisit after T17 if ingest > 10 min on typical repos — options: multiple LSP processes per language, shard by file, pipeline independent request types within a session.
