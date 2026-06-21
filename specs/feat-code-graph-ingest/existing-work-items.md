# existing-work-items — ingest crate scan

Scan of `crates/ingest/` for incomplete work and feature-relevant gaps. P1 = blocker for feature, P2 = needed, P3 = cleanup, RELATED = not tagged as incomplete but directly relevant.

## Feature-blocking gaps

- [P1] `lsp.rs:215` — Only `document_symbols()` implemented. Missing `callHierarchy/incomingCalls`, `callHierarchy/outgoingCalls`, `textDocument/references`, `textDocument/typeDefinition`, `textDocument/implementation`, `textDocument/hover`. → Add one public method per LSP request type.
- [P1] `lsp.rs:26` — `Symbol` struct lacks `start_byte`, `end_byte`, `visibility`. → Add fields.
- [P1] `extractor.rs:32` — `Entity` struct lacks `source_text`, `start_byte`, `end_byte`, `visibility`, `sha256`. → Add fields per overview "Entity + edge schema".
- [P1] `extractor.rs:40` — `Edge` struct has only `weight`. Missing metadata for `calls` (`call_file`, `call_line`, `call_col`, `call_count`) and `references` (`ref_file`, `ref_line`, `ref_col`). → Add metadata field (JSON or typed).
- [P1] `extractor.rs:62` — No `file` entity is emitted. → Emit one per source file before symbol walk; store sha256 + capped source_text.
- [P1] `extractor.rs:1345` — `extract_lsp_symbols()` walks files but never calls callHierarchy / references. Emits only `contains` edges. → Extend to emit `calls`, `references`, `has_type`, `implements`.
- [P1] `loader.rs:87` — Python loader INSERT column list is frozen and doesn't include any new fields. → Code ingest path moves to `ingest_entities` MCP tool; leave Python loader for non-code ingest.

## Incremental refresh gaps

- [P2] `extractor.rs:62` — Extractor does a full walk every run. No file-hash tracking, no `last_refreshed_at`, no `extractor_version`. → Implement refresh per overview "Refresh model".
- [P2] `extractor.rs:1345` — No reverse-dependency closure on refresh. → Implement 1-hop closure via graph query to ferrosa-memory.

## Schema mismatches

- [P2] `extractor.rs:1487` — `emit_symbol_entities()` uses `sym.kind.entity_type()` generic mapping; doesn't distinguish `method` from `function`, no `visibility`, no `file_id`, no `range`. → Rework per new schema.
- [P2] `extractor.rs:1522` — Entity `context` is a free-form string mixing signature+location+detail. → Move `signature`, `doc`, `visibility` into structured attrs; keep `context` for legacy/human-readable.
- [P2] `descriptions/config.rs:36` — `include_private: bool` toggle exists in config but not honored by extractor. Spec D3 = always on. → Remove toggle OR wire it up; user confirmed "always on" → remove.

## Multi-language readiness

- [RELATED] `lsp.rs:99` — `textDocument/didOpen` hardcodes `"languageId": "rust"`. → Parameterize before non-Rust work; OK to defer per feature scope (Rust-first).

## Test coverage gaps

- [P2] `tests/skill_catalog_smoke.rs:12` — Marked `#[ignore]` ("doesn't run in CI by default"). → Document the gating condition OR wire into a feature-gated test target.
- [P3] `extractor.rs:1572` — `extracts_entities_from_rust_workspace()` only checks crate count. No coverage for function/method/type symbols. → Add tests after callHierarchy + references land.

## Sequencing note

P1 items gate Tier 1 + Tier 2. P2 items gate refresh + Tier 3. P3 cleanup happens during the test pass after Tier 1.
