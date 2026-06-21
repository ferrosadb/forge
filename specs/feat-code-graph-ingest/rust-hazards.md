# feat-code-graph-ingest — Rust Correctness Hazards

> Derived from `corpus/correctness-hazards/correctness-hazard-reference.md`.
> Focus: Rust-specific footguns introduced by extending `crates/ingest/` with callHierarchy,
> references, typeDefinition, implementation, hover, file entities, source ranges, and
> incremental refresh. Scan before merge; CI must enforce P0/P1.

## P0 — Must not merge without

| ID | Hazard | Location risk | Guard |
|----|--------|---------------|-------|
| R-P0-1 | **UTF-16 vs byte index mismatch in source-slice panics** — LSP `Position` defaults to UTF-16 code units (`offset` in `utf16`); `lsp.rs:179-199` sends `capabilities` with **no `general.positionEncodings`** negotiation, so server will pick UTF-16. Spec says agents slice `source_text[start_byte..end_byte]` (`overview.md:61`); computing byte offset from a UTF-16 `(line, character)` via naive `line_starts[l] + c` is wrong for any non-ASCII source and will **panic on char boundary** (`str` indexing requirement). | every call site that turns LSP `Range` → `(start_byte, end_byte)` | Negotiate `"general": { "positionEncodings": ["utf-8", "utf-16"] }` in `initialize`; assert `serverInfo.positionEncoding == "utf-8"` where supported (rust-analyzer does). When UTF-16 is unavoidable, convert via a **line-indexed char iterator** that steps UTF-16 code units, never by byte arithmetic on `character`. Unit test with a source file containing `é`, `🦀`, CRLF. |
| R-P0-2 | **`source_text[start..end]` panic on non-boundary** (F1 correctness-hazard) — `str::Index` panics if either bound is not a UTF-8 char boundary. | retrieval contract in `overview.md:61`, every symbol emission | Use `source_text.get(start..end)` returning `Option`; on `None` emit `range_invalid: true` attribute and fail loud with file+symbol in the log — never fall back to empty. Power of 10 Rule 7 (check all returns). |
| R-P0-3 | **`.unwrap()` / `.expect()` on LSP response parsing** — extensions to `parse_symbols` (`lsp.rs:459-507`) for callHierarchy/references/typeDefinition will touch many `Option` chains; current code uses `?` on `Option` in `parse_one_symbol` which **silently drops malformed entries**. | new parsers for `callHierarchy/*`, `references`, `typeDefinition`, `implementation`, `hover` | Forbid `unwrap`/`expect` via `#![deny(clippy::unwrap_used, clippy::expect_used)]` at crate root. Replace silent `?` on `Option` in parsers with explicit `ok_or_else` + logged skip counter (`malformed_symbols`, `malformed_calls`, …) surfaced in `IngestSummary`. Fail-loud per safety rules. |
| R-P0-4 | **Unbounded result sets from callHierarchy/references** — popular symbols (`Result::ok`, trait methods on `Iterator`) can return tens of thousands of incoming/outgoing edges; no cap today. | new `call_hierarchy_incoming/outgoing`, `references` methods | Hard cap per query (default 1024); truncate with `truncated: true` metadata on the edge-group; log when cap hit. Power of 10 Rule 2 (fixed loop bounds) + Rule 3 (bounded allocation). |
| R-P0-5 | **Shared mutable LSP session across threads** — `document_symbols(&mut self, …)` (`lsp.rs:215`) is inherently single-threaded (`pending: HashMap<i64, ()>` + sequential stdout reader). Adding five more methods tempts an `Arc<Mutex<LspSession>>` for parallelism; that re-entrancy races `request()`'s id-loop at `lsp.rs:302-314`. | any future parallel file pipeline | Keep `&mut self` on all new methods. If parallelism is wanted, **one session per worker**, not one session behind a mutex. Benchmark sequential first (spec D5, `overview.md:173`). Document in module header. |
| R-P0-6 | **Orphan edges from split ingest batches** — `ingest_entities strict_edges: true` holds intra-batch, but the extractor will split payloads; a `calls` edge in batch N+1 whose target entity was dropped in batch N now fails the batch. | new edge emission in extractor | Topological-order the emission: all `file` + symbol entities first, then all edges in a second batch (or batches) keyed on entity ids already acknowledged by ferrosa-memory. Record pre-flight the set of emitted ids; reject edges referencing unknown ids with a loud error, not a silent drop. |

## P1 — Should not merge without

| ID | Hazard | Guard |
|----|--------|-------|
| R-P1-1 | **`u32` line/col overflow** (`lsp.rs:31-32, 480-481`) — `as u32` cast from `u64` silently wraps on generated files with >4 B lines. Rare but violates Power of 10 Rule 7. | Use `u32::try_from(…).map_err(|_| …)?`; document the line-count ceiling. |
| R-P1-2 | **`start_byte`/`end_byte` type inconsistency** — spec uses plain names (`overview.md:52`); Rust slicing wants `usize`; LSP gives `u64`. Mixing `u32` (line) and `usize` (byte) across entity serialization risks silent truncation on 32-bit targets. | Canonicalize: `start_byte: u32` in the on-wire schema (files ≤ `MAX_FILE_BYTES = 128 KiB ≪ u32`), convert to `usize` only at the slice site with `usize::try_from`. Assert `end_byte <= source_text.len() as u32`. |
| R-P1-3 | **File I/O race: sha256 vs LSP content vs source_text snapshot** — extractor will (a) `read_to_string` for sha256, (b) send `didOpen` with text read **again** at `lsp.rs:220`, (c) persist `source_text` as a third read. A save between steps produces a `file` entity whose `sha256` doesn't match its `source_text` and whose ranges are stale. | Read once into a `String`; derive sha256, `source_text`, and the `didOpen` text from that single buffer. Remove the second read in `document_symbols` (take text as a parameter). |
| R-P1-4 | **`read_to_string` OOM on huge generated files** (`lsp.rs:220`) — no size check; a 2 GB lockfile or checked-in artifact kills the process. | Stat first, enforce a **hard file-size cap** (e.g. `2 * MAX_FILE_BYTES`); files above cap emit a `file` entity with `truncated: true`, no LSP call, no symbols. Log-and-skip, don't panic. |
| R-P1-5 | **`String::from_utf8_lossy` hides encoding bugs** (`lsp.rs:379, 396, 416`) — fine for error-message context, but **must not** be reused for `source_text` persistence or sha256 input. Lossy replacement chars change the hash and break incremental diff. | Use `fs::read` (bytes) → sha256 first, **then** `std::str::from_utf8` (strict); reject non-UTF-8 files with a clear `non_utf8_source` skip reason. |
| R-P1-6 | **Path normalization across platforms** — `file://{path.display()}` (`lsp.rs:178, 216`) uses `Display`, which on Windows embeds `\` and drive letters, producing invalid `file://` URIs and non-round-trippable `file_id` keys. | Use `url::Url::from_file_path` (round-trips on all platforms); store canonical `file_id` as the URL string; unit test with a Windows-style path via fake root. |
| R-P1-7 | **Stale cache corruption on concurrent `frg ingest`** — `.forge/cache/code-graph/<project-id>.toml` (`overview.md:94`) written without file lock; two invocations produce a torn TOML. | Write to a tempfile in the same dir then `rename` (atomic on POSIX/NTFS); take an advisory lock (`fs2::FileExt::try_lock_exclusive`) for the whole refresh; fail loud on contention. |
| R-P1-8 | **`Drop` leak window on panic between `shutdown` request and `exit` notify** (`lsp.rs:261-285`) — current `shutdown` is by-value (`mut self`) so any `?` between `request("shutdown")` and `child.wait` returns, skipping the kill. The `Drop` impl (`lsp.rs:445-456`) catches the child but **not** the JoinHandle of the stderr drainer, which is already detached — acceptable, but new per-language sessions multiply the risk. | Ensure every early-return path in `shutdown` still falls through to `child.kill`; the existing `Drop` already covers this — keep it. Add a panic-safety test (`std::panic::catch_unwind` around a forced panic mid-shutdown, assert no zombie via `child.try_wait`). |
| R-P1-9 | **`.unwrap()` on `entity_id`** (`extractor.rs:17`) — a `UUID_NS` parse failure panics the whole ingest. | Use `Uuid::parse_str(…).expect("…")` with message, or a `OnceLock<Uuid>`; Power of 10 Rule 5 (assertion with context). |
| R-P1-10 | **Function length creep (Power of 10 Rule 4)** — `extract_lsp_symbols` and `emit_symbol_entities` (`extractor.rs:1487-1553`) are the extension target for five new LSP methods; each added method pushes them past the 60-line cap. | Extract per-method helpers: `collect_call_edges(&mut session, file, symbols) -> Vec<Edge>`, likewise for references/typeDefinition/implementation/hover. Cap each at ~40 lines. CI grep for functions > 60 non-blank lines inside `crates/ingest/src/`. |
| R-P1-11 | **Blocking I/O under a tokio caller** — `ingest/` is pure sync (`std::process`, `BufReader`); MCP server front-end may be async. A direct call from an async handler into `LspSession::start` blocks the runtime. | If `forge-mcp` invokes extraction, wrap in `tokio::task::spawn_blocking`. Do **not** introduce `tokio::sync::Mutex` inside `LspSession`. Document in crate header. |
| R-P1-12 | **sha256 streaming not bounded** — even when hashing, reading the whole file into RAM is the risk, not the hasher. | Stream with `sha2::Sha256::update` over a 64 KiB chunked `BufReader`, subject to the R-P1-4 size cap. |

## P2 — Monitor

| ID | Hazard | Guard |
|----|--------|-------|
| R-P2-1 | mmap vs `read_to_string` — mmap avoids double-copy for large files but adds SIGBUS risk if file is truncated mid-read. | Stay on `read` unless profiling shows a win; if adopted, use `memmap2` with explicit error handling, never `Mmap::as_ref().unwrap()`. |
| R-P2-2 | Line-count drift when `end_line < start_line` from buggy servers. | Validate `end >= start`; skip with logged reason. |
| R-P2-3 | Hover markdown bloat inflating entity `doc` attr. | Cap `doc` at 4 KiB; mark `doc_truncated`. |
| R-P2-4 | `extractor_version` bump forgotten → stale cache masquerades as fresh. | CI check: if `lsp.rs` or `extractor.rs` `emit_symbol_entities` changes, require a bumped `EXTRACTOR_VERSION` const. |
| R-P2-5 | `positionEncoding` negotiation divergence across language servers (pyright, gopls, elixir-ls differ). | Per-language capability check; record chosen encoding on the `file` entity for forensics. |

## CI enforcement

```toml
# crates/ingest/Cargo.toml
[lints.clippy]
unwrap_used       = "deny"
expect_used       = "deny"
panic             = "deny"   # #[allow] with justification where unavoidable
indexing_slicing  = "deny"   # force .get() on source_text ranges
as_conversions    = "warn"   # catch u64-as-u32 drift
await_holding_lock = "deny"
```

Grep gates (run in CI against `crates/ingest/src/`):

```bash
# No raw str indexing for source slices
rg -n 'source_text\s*\[' crates/ingest/src/ && exit 1

# Every LSP response .get chain must terminate in ok_or / context, not bare ?
rg -n 'parse_.*_response' crates/ingest/src/lsp.rs | xargs -I {} rg 'ok_or|context|bail' {}

# Function-length cap
awk '/^fn |^pub fn |^pub\(crate\) fn /{name=$0;count=0;next} /^}/{if(count>60)print FILENAME":"NR": "name" ("count" lines)";count=0;next} {count++}' \
    crates/ingest/src/extractor.rs crates/ingest/src/lsp.rs
```

## Related standards

- **Power of 10** rules 2 (bounded loops), 3 (bounded allocation), 4 (short functions), 5 (assertions), 7 (check returns), 10 (max warnings) — all triggered by this feature.
- **CERT C/C++ STR-series** analogues in Rust: UTF-8 boundary invariants, bounded strings.
- **MISRA C Rule 21.x** analogues: avoid unchecked standard-library returns — maps to `unwrap_used = deny`.
