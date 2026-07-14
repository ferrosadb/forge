# sheet-sync Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A forge crate that syncs a Google Sheet of bugs to the CQL task board (pull) and writes fix status back to the sheet (push), with all safety rules baked in.

**Architecture:** One Rust crate `crates/sheet-sync`. A **pure sync engine** (id-normalization, header mapping, reconciliation planning, cell-edit computation) sits behind a `SheetsApi` trait and a `BoardOps` plan, so 100% of decision logic is unit-tested against fakes with no network/CQL. Thin shells — `GoogleSheets` (ureq) + hand-rolled OAuth, a `TaskStore` executor, and CLI/MCP wiring — carry no branching logic. Sheet↔task join lives in a sidecar `.forge/sheets/<alias>.state.toml`.

**Tech Stack:** Rust 2021, `ureq` (blocking HTTP), `serde`/`serde_json`/`toml`, `anyhow`, `clap`, `base64`, `dirs`, `chrono`; `forge-tasks` (CQL board), `forge-shared` (`emit_json`). Dev: `tempfile`. No Google API crates.

## Global Constraints

- forge workspace version bump: `0.14.0 → 0.15.0` (verbatim, in root `Cargo.toml [workspace.package]`).
- Coverage floors (workspace-wide, CI-enforced): **lines ≥ 70, functions ≥ 66, regions ≥ 68**. The new crate must carry enough tests to not drag these.
- `cargo clippy --workspace --all-targets -- -D warnings` — zero warnings.
- `cargo fmt --all -- --check` clean; `cargo doc --workspace --no-deps` warning-free (`RUSTDOCFLAGS=-D warnings`); `cargo deny check` clean (minimal allowlisted deps only).
- Fail loud — no fake success, no silent fallback. Deterministic machine-readable JSON output. Secrets/tokens redacted from all diagnostics.
- Conventional commits `feat(sheet-sync): …` / `test(sheet-sync): …` / `docs: …`; TDD (failing test first); never commit to `main` (we are on `feat/gsheet-task-sync`).
- Write blast radius: engine may only ever emit edits for columns whose canonical field ∈ mapping `writable`.

## File Structure

```
crates/sheet-sync/
  Cargo.toml
  src/lib.rs            # entry fns pull()/push()/auth(); Options/Report; re-exports
  src/model.rs          # Grid, Row, CellEdit, CanonicalRow, CanonicalField, BoardOp, reports
  src/config.rs         # SheetMapping (.forge/sheets/<alias>.toml) parse + validate; alias path resolution
  src/normalize.rs      # normalize_id(); duplicate detection
  src/mapping.rs         # Grid + SheetMapping -> Vec<CanonicalRow>; header resolution; terminal-skip
  src/board_plan.rs      # CanonicalRow + State + existing statuses -> Vec<BoardOp> (pull planning); never-move-backward
  src/push_plan.rs       # push request + sheet Row + mapping + state -> Vec<CellEdit>; lifecycle handoff; blast-radius
  src/state.rs           # sidecar .state.toml read/write
  src/sheets/mod.rs      # SheetsApi trait + FakeSheets (cfg(test)/test-support)
  src/sheets/google.rs   # GoogleSheets (ureq): read_grid + write_cells (batchUpdate)
  src/oauth.rs           # installed-app loopback OAuth; token cache in dirs::config_dir()/forge/sheet-sync
  src/board_exec.rs      # apply Vec<BoardOp> to forge_tasks::TaskStore (thin)
```

Wiring (existing files): `crates/cli/Cargo.toml` (path dep), `crates/cli/src/main.rs` (`Commands::Sheet` + match arm + `register_tool!` blocks), root `Cargo.toml` (version), `CHANGELOG.md`, `skills/sheet-sync/SKILL.md`, `.forge/sheets/spoton-qa.toml` (example).

---

### Task 1: Crate scaffold, version bump, canonical model, id normalization

**Files:**
- Create: `crates/sheet-sync/Cargo.toml`, `crates/sheet-sync/src/lib.rs`, `crates/sheet-sync/src/model.rs`, `crates/sheet-sync/src/normalize.rs`
- Modify: `crates/cli/Cargo.toml` (add `forge-sheet-sync = { path = "../sheet-sync" }`), root `Cargo.toml` (`version = "0.15.0"`)
- Test: inline `#[cfg(test)]` in `normalize.rs`

**Interfaces:**
- Produces: `normalize_id(&str) -> String`; `find_duplicate_ids(&[String]) -> Vec<String>` (normalized ids appearing >1×, sorted, deduped). `model::CanonicalField` enum with `parse(&str)->Option<Self>` and the field set from the spec. `model::CanonicalRow { id: String, fields: BTreeMap<CanonicalField,String>, sheet_row_index: usize }`.

- [ ] **Step 1: Write failing tests** in `normalize.rs`:
```rust
#[test]
fn strips_trailing_parenthetical_and_whitespace() {
    assert_eq!(normalize_id("QA-005 (This may not be a bug)"), "QA-005");
    assert_eq!(normalize_id("  QA-016 "), "QA-016");
    assert_eq!(normalize_id("QA-005"), "QA-005");
}
#[test]
fn detects_the_two_qa005_rows_as_duplicate() {
    let ids = vec!["QA-004","QA-005","QA-026","QA-005"].into_iter().map(normalize_id).collect::<Vec<_>>();
    assert_eq!(find_duplicate_ids(&ids), vec!["QA-005".to_string()]);
}
#[test]
fn no_duplicates_when_all_unique() {
    let ids = ["QA-1","QA-2"].map(normalize_id).to_vec();
    assert!(find_duplicate_ids(&ids).is_empty());
}
```
- [ ] **Step 2: Run — expect FAIL** (unresolved). Run: `cargo test -p forge-sheet-sync normalize`
- [ ] **Step 3: Implement** `normalize_id` (trim, drop a trailing `(...)` group via a non-regex scan or `regex` — prefer a manual `rfind('(')` slice to avoid a new dep; re-trim) and `find_duplicate_ids` (count in a `BTreeMap<String,usize>`, collect keys with count>1). Write `model.rs` enums/structs. `lib.rs`: `pub mod model; pub mod normalize;`.
- [ ] **Step 4: Run — expect PASS.**
- [ ] **Step 5: Verify workspace builds + version bumped.** Run: `cargo build -p forge-sheet-sync && grep '0.15.0' Cargo.toml`
- [ ] **Step 6: Commit** `feat(sheet-sync): scaffold crate, canonical model, id normalization; bump 0.15.0`

---

### Task 2: Per-sheet mapping config

**Files:** Create `crates/sheet-sync/src/config.rs`; Test: inline.

**Interfaces:**
- Produces: `SheetMapping { spreadsheet_id, tab, id_column, columns: BTreeMap<String,CanonicalField>, writable: BTreeSet<CanonicalField>, status_map: BTreeMap<String,TaskStatus>, dev_writable_status: BTreeSet<String>, terminal_status: BTreeSet<String> }`; `SheetMapping::from_toml_str(&str) -> anyhow::Result<Self>` (validates: id_column present, id_column∈columns values as `id`, every writable∈columns, non-empty); `SheetMapping::alias_path(alias) -> PathBuf` (walk up for `.forge/sheets/<alias>.toml`).

- [ ] **Step 1: Failing tests**: valid TOML (spec example) parses; missing `id_column` → `Err` containing "id_column"; `writable` naming an unmapped field → `Err`; unknown status_map target → `Err`.
- [ ] **Step 2: Run — FAIL.** `cargo test -p forge-sheet-sync config`
- [ ] **Step 3: Implement** with a `#[derive(Deserialize)] struct RawMapping` then a validating `TryFrom`. Use `forge_tasks::TaskStatus::parse` for status_map targets; fail loud on `None`.
- [ ] **Step 4: Run — PASS.**
- [ ] **Step 5: Commit** `feat(sheet-sync): per-sheet mapping config with validation`

---

### Task 3: Grid → canonical rows (header resolution + terminal skip + dup refusal)

**Files:** Create `crates/sheet-sync/src/mapping.rs`; Test: inline.

**Interfaces:**
- Consumes: `model::Grid { headers: Vec<String>, rows: Vec<Vec<String>> }`, `SheetMapping`.
- Produces: `map_grid(&Grid, &SheetMapping) -> anyhow::Result<MappedGrid>` where `MappedGrid { rows: Vec<CanonicalRow>, skipped_terminal: Vec<String>, duplicate_ids: Vec<String> }`. Missing mapped header → `Err` naming the header (hard error). Rows with normalized id in `duplicate_ids` are excluded from `rows`. Rows whose Status∈`terminal_status` excluded and recorded in `skipped_terminal`.

- [ ] **Step 1: Failing tests** using a small in-code Grid built from the real header row: a Bug row maps all fields; a `Verified/Closed` row lands in `skipped_terminal`; the two `QA-005` rows land in `duplicate_ids` and neither appears in `rows`; a Grid missing the `Status` header → `Err`.
- [ ] **Step 2: Run — FAIL.**
- [ ] **Step 3: Implement**: build header→index map; assert every `mapping.columns` key exists (else Err). For each data row, read id column, `normalize_id`; compute duplicates once across all ids; skip terminal by reading mapped Status value ∈ `terminal_status`.
- [ ] **Step 4: Run — PASS.**
- [ ] **Step 5: Commit** `feat(sheet-sync): map grid to canonical rows with fail-loud header + dup handling`

---

### Task 4: Board op planning — task construction + status map + never-move-backward

**Files:** Create `crates/sheet-sync/src/board_plan.rs`; Test: inline.

**Interfaces:**
- Consumes: `Vec<CanonicalRow>`, `state::State` (row_id→StateEntry), and a lookup of existing task status `&dyn Fn(&str)->Option<TaskStatus>` (so tests inject; live code passes a closure over fetched tasks).
- Produces: `plan_pull(rows, state, existing_status) -> Vec<BoardOp>` where `BoardOp = Create { row_id, req: CreateTaskRequest } | Update { row_id, task_id, patch: UpdateTaskPatch } | Skip { row_id, reason }`. Helpers: `build_create_request(&CanonicalRow, &SheetMapping) -> CreateTaskRequest` (title `[id] Title`, body = Description+Steps+Expected+Actual+Environment+evidence, priority from Priority P0..P3→0..3, `skills`/tags `type:<>`,`severity:<>`,`mvp-blocker`, status via `status_map`, metadata JSON {source,sheet_id,row_id}); `content_hash(&CanonicalRow) -> String` (sha-free: stable hash of sorted field kv → hex; use `std::hash` via a documented FNV, or `format!`+`base64`).

- [ ] **Step 1: Failing tests**: new row (state miss) → `Create` with title `[QA-016] …`, priority mapped, status from map; existing row unchanged content → `Skip{reason:"unchanged"}`; existing row content changed but task already `in_progress` → `Update` patch leaves status `None` (never move backward); existing row content changed and task `triage` → `Update` may set status from map.
- [ ] **Step 2: Run — FAIL.**
- [ ] **Step 3: Implement.** `UpdateTaskPatch` status left `None` when `existing_status` ∈ dev-owned set {InProgress, Blocked, Complete}. Compose body deterministically.
- [ ] **Step 4: Run — PASS.**
- [ ] **Step 5: Commit** `feat(sheet-sync): pull planning with status map + never-move-backward`

---

### Task 5: Sidecar state file

**Files:** Create `crates/sheet-sync/src/state.rs`; Test: inline with `tempfile`.

**Interfaces:**
- Produces: `State { rows: BTreeMap<String, StateEntry> }`, `StateEntry { task_id, content_hash, last_push_status: Option<String> }`; `State::load(path)->Result<State>` (missing file → empty), `State::save(path)->Result<()>` (create parents, 0600 dir), `State::upsert(row_id, entry)`.

- [ ] **Step 1: Failing tests**: load-missing → empty; save then load round-trips a two-entry state; upsert overwrites.
- [ ] **Step 2: Run — FAIL.** **Step 3: Implement** (toml serialize/deserialize). **Step 4: PASS.**
- [ ] **Step 5: Commit** `feat(sheet-sync): sidecar state file for row↔task join`

---

### Task 6: Push edit computation — lifecycle handoff + blast radius

**Files:** Create `crates/sheet-sync/src/push_plan.rs`; Test: inline.

**Interfaces:**
- Consumes: `PushRequest { row_id, status: Option<String>, fix_ver: Option<String>, notes: Option<String> }`, the sheet `Grid`, `SheetMapping`, `State`.
- Produces: `plan_push(&PushRequest, &Grid, &SheetMapping, &State) -> anyhow::Result<Vec<CellEdit>>` where `CellEdit { a1: String, header: String, old: String, new: String }`. Rules enforced (all fail-loud): row_id not found → `Err`; row_id ∈ duplicate set → `Err`; a writable header missing from Grid → `Err`; status written only if `new_status ∈ dev_writable_status` AND current sheet Status ∉ (terminal ∪ client-owned) — else status edit omitted while fix_ver/notes still emitted; every produced edit's field ∈ `mapping.writable` (assert, else `Err`).

- [ ] **Step 1: Failing tests**: (a) status `In Progress` on a `New` row → 1 status edit; (b) status `Verified/Closed` requested → status edit omitted (not dev-writable), fix_ver still emitted; (c) sheet row already `Won't Fix` → status omitted, notes+fix_ver emitted; (d) push to unknown row_id → Err; (e) attempt where mapping.writable lacks `status` but request sets status → status skipped (defense), documented.
- [ ] **Step 2: Run — FAIL.** **Step 3: Implement.** **Step 4: PASS.**
- [ ] **Step 5: Commit** `feat(sheet-sync): push edit computation with lifecycle handoff + blast radius`

---

### Task 7: SheetsApi trait + FakeSheets + end-to-end dry-run

**Files:** Create `crates/sheet-sync/src/sheets/mod.rs`; extend `lib.rs`; Test: inline integration using FakeSheets.

**Interfaces:**
- Produces: `trait SheetsApi { fn read_grid(&self, spreadsheet_id:&str, tab:&str)->Result<Grid>; fn write_cells(&self, spreadsheet_id:&str, edits:&[CellEdit])->Result<()>; }`; `FakeSheets { grid: RefCell<Grid>, writes: RefCell<Vec<CellEdit>> }`. `PullOptions`/`PushOptions` carry `dry_run: bool`. `lib::pull`/`lib::push` accept a `&dyn SheetsApi` + a board executor closure so they're testable without CQL/network.

- [ ] **Step 1: Failing tests**: full `pull` over a FakeSheets grid produces expected BoardOps and, when `dry_run`, records **zero** writes and makes **zero** board mutations; full `push` with `dry_run` returns the CellEdits and FakeSheets `writes` stays empty; non-dry push calls `write_cells` exactly once with the computed edits.
- [ ] **Step 2: Run — FAIL.** **Step 3: Implement** wiring engine↔trait; board executor injected as `dyn FnMut(&[BoardOp])`. **Step 4: PASS.**
- [ ] **Step 5: Commit** `feat(sheet-sync): SheetsApi seam + FakeSheets + dry-run end-to-end`

---

### Task 8: GoogleSheets (ureq) + OAuth loopback + token cache

**Files:** Create `crates/sheet-sync/src/sheets/google.rs`, `crates/sheet-sync/src/oauth.rs`, `crates/sheet-sync/src/board_exec.rs`.

**Interfaces:**
- Produces: `GoogleSheets::new(token: AccessToken) -> Self` impl `SheetsApi` (read via `GET .../values/<tab>`, write via `POST .../values:batchUpdate` with `valueInputOption=RAW`, A1 built from resolved column index + row); `oauth::authorize(alias, &OAuthClient) -> Result<()>` (loopback flow, persist refresh token 0600 at `dirs::config_dir()/forge/sheet-sync/<alias>.json`); `oauth::access_token(alias, &OAuthClient) -> Result<AccessToken>` (refresh); `OAuthClient::load()` from `FORGE_GOOGLE_OAUTH_CLIENT` or `.forge/config.toml [google]` — **fail loud if absent**. `board_exec::apply(&TaskStore, &[BoardOp]) -> Result<Vec<AppliedOp>>`.

- [ ] **Step 1: Tests**: unit-test pure helpers — A1 builder (`col_index_to_a1(0)=="A"`, `col_index_to_a1(26)=="AA"`), token-response JSON parsing, `OAuthClient::load` Err when unset. Add a `#[ignore]` live round-trip test gated on `FORGE_GOOGLE_OAUTH_CLIENT` + a scratch sheet id env (pattern: `crates/tasks/tests/board_health_live.rs`).
- [ ] **Step 2: Run — FAIL.** **Step 3: Implement** ureq calls (Agent config with 30s timeout, retry transport-only), loopback `TcpListener`, base64url for state. Redact tokens in any error/log. **Step 4: PASS** (`--include-ignored` only when creds present).
- [ ] **Step 5: Commit** `feat(sheet-sync): GoogleSheets ureq client, OAuth loopback, board executor`

---

### Task 9: CLI subcommands + MCP tools

**Files:** Modify `crates/cli/src/main.rs` (add `Commands::Sheet` per `main.rs:43` enum + dispatch per `main.rs:5357`; `register_tool!` blocks per `main.rs:3249`).

**Interfaces:**
- Consumes: `lib::{pull,push,auth}`, `SheetMapping::alias_path`, `forge_tasks::{resolve_cql_hosts,TaskStore}`, `forge_shared::emit_json`.
- Produces CLI: `frg sheet auth <alias>`, `frg sheet pull <alias> [--dry-run] [--cql-host]`, `frg sheet push <alias> <row_id> [--status][--fix-ver][--notes][--dry-run][--cql-host]`. MCP: `sheet_auth{alias}`, `sheet_pull{alias,dry_run?,cql_host?}`, `sheet_push{alias,row_id,status?,fix_ver?,notes?,dry_run?,cql_host?}`, tier-1, handlers return `to_string_pretty(&report)`.

- [ ] **Step 1: Test**: a CLI-level test that `frg sheet pull nonexistent-alias` exits non-zero with a JSON error (alias file not found) — assert fail-loud, no panic. (Use `assert_cmd`-style over the built binary, or a `--help` smoke matching existing CLI tests.)
- [ ] **Step 2: Run — FAIL.** **Step 3: Implement** enum variant, match arm building Options and calling lib with a real `GoogleSheets` + `board_exec::apply`; register the three MCP tools with inline `json!` schemas. **Step 4: Run — PASS**; `cargo build --workspace --all-targets`.
- [ ] **Step 5: Commit** `feat(sheet-sync): frg sheet CLI + sheet_pull/push/auth MCP tools`

---

### Task 10: Example mapping, skill, CHANGELOG

**Files:** Create `.forge/sheets/spoton-qa.toml` (the spec's mapping, committed as example), `skills/sheet-sync/SKILL.md`, modify `CHANGELOG.md`.

- [ ] **Step 1:** Write `.forge/sheets/spoton-qa.toml` exactly as the spec's config block (spreadsheet_id `EXAMPLE_SPREADSHEET_ID`, tab `QA Log`, full columns/status_map/handoff sets).
- [ ] **Step 2:** Write `skills/sheet-sync/SKILL.md`: name `sheet-sync`, description "sync a Google Sheet of bugs to the forge task board and push fix status back"; procedure — `frg sheet auth <alias>` once → `frg sheet pull <alias>` → pick an open task, fix it (TDD) → `frg sheet push <alias> <row_id> --status … --fix-ver … --notes …`; document dev-owned status handoff + dry-run first + the dirty-ID caveat.
- [ ] **Step 3:** CHANGELOG `## 0.15.0` entry: "feat(sheet-sync): Google Sheets ↔ task board two-way sync (`frg sheet`, `sheet_pull/push/auth`)".
- [ ] **Step 4: Commit** `docs(sheet-sync): example mapping, sync skill, changelog`

---

### Task 11: Full gate sweep + local install + PR

- [ ] **Step 1:** `cargo fmt --all -- --check` (fix), `cargo clippy --workspace --all-targets -- -D warnings` (fix to zero).
- [ ] **Step 2:** `cargo test --workspace` green.
- [ ] **Step 3:** `cargo llvm-cov --workspace report --fail-under-lines 70 --fail-under-functions 66 --fail-under-regions 68` — add engine tests until green.
- [ ] **Step 4:** `cargo doc --workspace --no-deps` with `RUSTDOCFLAGS=-D warnings`; `cargo deny check`.
- [ ] **Step 5:** `cargo build --release`; install: `cp target/release/frg ~/.cargo/bin/frg`; verify `frg sheet --help`.
- [ ] **Step 6:** Move `specs/todo/feat-sheet-sync.md` → `specs/implemented/`; commit; push branch; open PR to `ferrosadb/forge` (title `feat(sheet-sync): Google Sheets ↔ task board two-way sync`).

## Self-Review

- **Spec coverage:** pull/push/auth (T7–T9), mapping (T2/T3), safety rules — dup refusal (T1/T3), header-resolved + blast radius + handoff (T6), never-move-backward (T4), dry-run (T7), sidecar join (T5), OAuth scope/token (T8), config precedence (T2), version bump+install (T1/T11), skill+example+PR (T10/T11). All spec sections map to a task.
- **Placeholder scan:** each code step shows concrete tests/logic; wiring tasks cite exact forge `file:line` anchors from recon rather than restating boilerplate.
- **Type consistency:** `BoardOp`, `CellEdit`, `CanonicalRow`, `SheetMapping`, `State`/`StateEntry`, `SheetsApi` names are used identically across T3–T9.
- **Deferred:** OQ-1 (prod auto-tagging) explicitly out of scope; OQ-3 (sheet cleanup) is human-side.
