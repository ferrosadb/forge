# feat-code-graph-ingest — Project Plan

> Risk-weighted sequencing across Tier 1 (LSP edges), Tier 2 (source preservation + private), Tier 3 (docstrings/imports/parameters), and Refresh.

## Sprint 0 — Foundations (blocks everything)

| # | Item | Drives | Source |
|---|---|---|---|
| 0.1 | Negotiate `positionEncodingKind: utf-8` at LSP init; convert UTF-16 fallback through a line index | F2 — byte-range panics/mojibake (RPN 432) | hazards P0-1 |
| 0.2 | Introduce `Entity` schema additions (`source_text`, `start_byte`, `end_byte`, `sha256`, `visibility`, `doc`, `signature`) | Every downstream task | scan P1 |
| 0.3 | Introduce `Edge` metadata (JSON-typed attrs for call-site/ref location) | All Tier 1 edges | scan P1 |
| 0.4 | `file` entity emission pass (path, sha256, bytes, lines, source_text capped 128 KiB, truncation flag) | Tier 2 storage, refresh hash | scan P1, F7 |
| 0.5 | Replace code-entity INSERT path — switch from Python `loader.rs` to `ingest_entities` MCP tool; enforce count reconciliation | F3 — silent per-row drop (RPN 567) | FMEA F3 |

## Sprint 1 — Tier 1 (call graph + type graph)

| # | Item | Depends | Source |
|---|---|---|---|
| 1.1 | Capability probe for `callHierarchyProvider`, `referencesProvider`, `typeDefinitionProvider`, `implementationProvider`, `hoverProvider`; degrade-loud per missing capability | Sprint 0 | F6 |
| 1.2 | `prepareCallHierarchy` → `incomingCalls` / `outgoingCalls` with `MAX_REFERENCES_PER_SYMBOL` cap (500) and per-request timeout | 1.1 | F1, F12 |
| 1.3 | `textDocument/references` with same cap + timeout | 1.1 | F1, F12 |
| 1.4 | `textDocument/typeDefinition` → `has_type` edges | 1.1 | — |
| 1.5 | `textDocument/implementation` → `implements`/`extends` edges | 1.1 | — |
| 1.6 | Edge metadata (file:line:col at call/ref site) + `call_count` aggregation | 0.3 | — |

## Sprint 2 — Tier 2 (storage + private)

| # | Item | Depends | Source |
|---|---|---|---|
| 2.1 | Drop public-only filter in `extract_lsp_symbols`; include private symbols | Sprint 0 | scan P2, D3 |
| 2.2 | Oversize-file handling: if `len() > MAX_FILE_BYTES`, skip source_text + mark `extraction_skipped: oversized` OR apply `body_offset` + strict `end_byte <= source_text.len()` invariant | 0.4 | F7 |
| 2.3 | Atomic read-once: hash + LSP `didOpen` text + stored `source_text` all come from one buffer | 0.4 | F14, hazards P1-3 |
| 2.4 | Topological batch ordering for `ingest_entities`: entities first, edges only for persisted entities; split batches by payload size | 0.5 | F12, hazards P0-6 |

## Sprint 3 — Refresh

| # | Item | Depends | Source |
|---|---|---|---|
| 3.1 | Per-file sha256 cache in `.forge/cache/code-graph/<project-id>.toml` with `fs2` exclusive lock | Sprint 0 | F13 |
| 3.2 | `frg ingest --refresh` path: diff hashes → re-extract changed files only | 3.1 | overview §Refresh |
| 3.3 | Deleted-file detection: compare stored paths vs current walk → report deletions and gate delete handling behind a separate dependency or explicit soft-delete contract | 3.2 | F9 |
| 3.4 | Rename detection: new file with sha256 matching a newly-absent file → either re-emit the file entity through `ingest_entities` with the desired full attrs or defer until a partial-update contract exists | 3.2 | F10 |
| 3.5 | Write-ahead cache: `pending_file_id` marker; sha256/`last_refreshed_at` only updated after commit | 3.2 | F11 |
| 3.6 | 1-hop reverse-reference closure via graph query | 3.2 | F8 |
| 3.7 | `--deep` flag: transitive closure with iteration cap (5) + cycle guard | 3.6 | F8 |
| 3.8 | `extractor_version` bump on every entity; mismatched versions force re-extract | 3.2 | F15 |

## Sprint 4 — Tier 3 (semantics)

| # | Item | Depends | Source |
|---|---|---|---|
| 4.1 | `textDocument/hover` → `doc` attr; parse all three LSP hover shapes (MarkedString / array / MarkupContent); store `doc_format` | 1.1 | F19 |
| 4.2 | Resolved `imports` edges via `textDocument/definition` (not name-match); unresolved → `imports_string` attr | Sprint 1 | F17 |
| 4.3 | `parameter` entities as children of functions with `has_type` edges | Sprint 1 | overview §entity kinds |

## Sprint 5 — Ship

| # | Item | Depends | Source |
|---|---|---|---|
| 5.1 | `ingest_refresh` MCP tool wrapper over CLI `--refresh` | Sprint 3 | overview §interface |
| 5.2 | Progress reporting: `$/progress` notifications or streamed summary lines | Sprint 1 | F16 |
| 5.3 | Chunk-restart policy: LSP session restart every N=500 files | Sprint 1 | F4, F5 |
| 5.4 | Benchmark: 1k-symbol fixture baseline + regression threshold | All | F16 |
| 5.5 | Documentation: `crates/ingest/README.md` update, `frg ingest --help` text, MCP tool descriptions | All | — |

## Out of this project

- Languages beyond Rust (Python/TS/Go/Elixir): per-language follow-ons
- Generic delete/update MCP tools on ferrosa-memory: still unavailable, so delete-oriented refresh remains an upstream dependency
- Full content-addressed blob store (storage option c)
- Control-flow / data-flow edges

## Critical risk forcing functions

From `fmea.md` (RPN ≥ 200) — every one of these has a corresponding sprint item and test fixture:

| RPN | Failure mode | Addressed by |
|-----|---|---|
| 567 | Silent per-row drop in `ingest_entities` | 0.5 |
| 432 | UTF-16 vs byte offset panic/mojibake | 0.1 |
| 360 | Oversize file invariant violation | 2.2 |
| 336 | References hang on popular symbol | 1.3 |
| 336 | callHierarchy missing on some servers | 1.1 |
| 336 | 1-hop closure transitive staleness | 3.6, 3.7 |
| 294 | Deleted file not detected | 3.3 |
| 294 | Rename confusion | 3.4 |
| 280 | Wrong import resolution | 4.2 |
| 256 | Partial refresh inconsistency | 3.5 |
| 245 | Editor save race | 2.3 |
| 240 | Extractor-version drift | 3.8 |
| 224 | LSP mid-batch crash | 5.3 |
| 216 | Unbounded reference result | 1.2, 1.3 |
| 210 | LSP OOM on full crawl | 5.3 |
