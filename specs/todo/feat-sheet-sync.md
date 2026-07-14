# feat: sheet-sync — Google Sheets ↔ task board sync

**Status:** todo → (in-process → implemented → verified)
**Crate:** `crates/sheet-sync` (`forge-sheet-sync`)
**Surface:** `frg sheet {pull,push,auth}` + MCP tools `sheet_pull`, `sheet_push`, `sheet_auth`
**Target version:** forge `0.15.0` (minor — additive feature)

## Problem

Teams track bugs/QA items in a Google Sheet that non-engineers own and edit. We
want **agentic access**: an agent pulls open items into the forge task board to
drive development, and pushes fix status back into the sheet so the sheet's
owners see progress in the tool they already use — without ever clobbering their
cells. This must be **generic** (any sheet, per-sheet column mapping), with the
SpotOn "QA Tracker" as the first consumer.

## Goals (v1)

- Read a Google Sheet, apply a **per-sheet column mapping**, upsert **open** rows
  as forge tasks (idempotent — re-running does not duplicate).
- Write back **only three fields** — `status`, `fix_ver`, `resolution_notes` —
  to the row's own cells, located by resolving the header name at write time.
- Concrete Google Sheets adapter with a clean internal seam (a `SheetsApi`
  trait) — **no** premature multi-source abstraction (YAGNI).
- Fail loud everywhere (dirty IDs, missing headers, terminal-state conflicts).

## Non-goals (v1)

- No generic `TaskSource` trait / Jira/Linear/CSV adapters (add when a 2nd source
  is real).
- No cron/daemon — sync is explicit (agent- or human-triggered). Pull-only
  scheduling can come later.
- **No automatic release-tagging inside the workflow.** The agent supplies the
  `fix_ver` value (git SHA for demo, version tag for prod); the sync does not cut
  tags. (Deferred — see Open Questions.)

## Architecture

Single Rust crate, wired the standard forge way (per `specs/` recon):

- **Library** `forge-sheet-sync`: `Options` / `Report` structs + entry fns
  `pull(&PullOptions) -> Result<PullReport>` and
  `push(&PushOptions) -> Result<PushReport>`, plus `auth(&AuthOptions)`.
  Deterministic, synchronous public surface (matches `forge-todo-extract`).
- **CLI**: `Commands::Sheet { … }` variant + match arm in `crates/cli/src/main.rs`;
  path dep `forge-sheet-sync = { path = "../sheet-sync" }` in `crates/cli/Cargo.toml`.
- **MCP**: `register_tool!` blocks in `run_mcp_server` for `sheet_pull` /
  `sheet_push` / `sheet_auth`, each with an inline `json!` input schema, handler
  returning `serde_json::to_string_pretty(&report)`.
- **Task board**: in-process via `forge_tasks::TaskStore::connect(&hosts, None)`
  (synchronous; store owns its tokio runtime). Uses `create_task`, `update_task`,
  `list_tasks`, `add_comment`.
- **Google I/O**: `ureq` (blocking) behind a `SheetsApi` trait so tests use a fake.
- **No heavy Google crates.** Hand-rolled OAuth 2.0 installed-app loopback flow
  (std `TcpListener` + `ureq` + `serde_json`) to keep `cargo deny` clean and match
  forge's lean-deps ethos. New deps kept to allowlist-friendly minimum
  (`base64` is already vendored; add `sha2` only if PKCE is chosen — see Auth).

### Boundary / testability

`SheetsApi` trait (`read_grid(sheet_id, tab) -> Grid`,
`write_cells(sheet_id, edits) -> ()`) isolates all network + OAuth. The sync
engine (mapping, id normalization, status handoff, reconciliation) is pure and
unit-tested against a `FakeSheets`. Only `GoogleSheets` (the real impl) touches
the network — kept thin so the coverage floor is met by testing the engine.

## Data model & idempotency — sidecar state file

The task store has **no upsert-by-external-key** (`create_task` always mints a new
`t_xxxxxxxx`; `update_task` cannot write `metadata`). So the join lives in a
**sidecar** committed nowhere (per-project, git-ignored):

`.forge/sheets/<alias>.state.toml`
```toml
[rows."QA-016"]
task_id         = "t_ab12cd34"
content_hash    = "sha256:…"     # detect sheet-side content edits → update task
last_push_status = "In Progress" # what we last wrote to the sheet (handoff bookkeeping)
```

- **Pull**: for each open row, look up `row_id` in state. Miss → `create_task`,
  record mapping. Hit → if `content_hash` changed, `update_task` (title/body/
  priority) — but **never move status backward** (see Semantics).
- **Push**: resolve `task_id` ← state[`row_id`]; write cells; update
  `last_push_status`.
- On create, we also stamp `metadata` (source/sheet_id/row_id) for observability,
  but the **state file is authoritative** because metadata isn't updatable.

## Config

Two layers, following forge's per-crate `.forge/config.toml` precedence
(explicit arg → env → nearest `.forge/config.toml` → default; blanks skipped):

1. **`.forge/config.toml`** — Google OAuth client (see Auth) and defaults.
2. **Per-sheet mapping** — `.forge/sheets/<alias>.toml`:
```toml
# NOTE: TOML top-level keys (including these arrays) MUST precede any [table];
# a bare `writable = […]` after [columns] would nest as `columns.writable`.
spreadsheet_id = "EXAMPLE_SPREADSHEET_ID"
tab            = "QA Log"
id_column      = "QA Log ID"

writable = ["status", "fix_ver", "resolution_notes"]
# Which sheet-side states are ours to advance (handoff); everything else is client-owned.
dev_writable_status = ["In Progress", "In Review", "Fixed - Needs Verification"]
# Terminal on the sheet: skip on import, and never push status onto these.
terminal_status = ["Verified/Closed", "Won't Fix", "Duplicate"]

[columns]                       # sheet header -> canonical field
"QA Log ID"          = "id"
"Title"              = "title"
"Type"               = "type"
"Category"           = "category"
"Description"        = "description"
"Steps to Reproduce" = "steps"
"Expected Result"    = "expected"
"Actual Result"      = "actual"
"Environment"        = "environment"
"Severity"           = "severity"
"Priority"           = "priority"
"MVP Blocker?"       = "mvp_blocker"
"Status"             = "status"
"Build/Version Fixed (git commit for demo, version tag for prod)" = "fix_ver"
"Resolution Notes"   = "resolution_notes"

# Status lifecycle → forge TaskStatus (many-to-one on pull)
[status_map]
"New"                       = "triage"
"Triaged"                   = "ready"
"In Progress"               = "in_progress"
"In Review"                 = "in_progress"
"Fixed - Needs Verification" = "complete"
"Verified/Closed"           = "archived"
"Won't Fix"                 = "archived"
"Duplicate"                 = "archived"
"Deferred"                  = "blocked"
```

## Auth

- **Client credentials**: a Google Cloud OAuth **Desktop** client (`client_id` +
  `client_secret`), provided via `FORGE_GOOGLE_OAUTH_CLIENT` (path to the
  downloaded `client_secret.json`) or a `[google] client_secret_path = …` key in
  `.forge/config.toml`. **Fail loud** if absent — no silent skip.
- **Flow**: `frg sheet auth <alias>` runs the installed-app **loopback** flow —
  spin a `std::net::TcpListener` on `127.0.0.1:<ephemeral>`, open/print the consent
  URL, capture the `code`, exchange at Google's token endpoint (`ureq` POST),
  store the **refresh token**.
- **Scope**: `https://www.googleapis.com/auth/spreadsheets` (read+write on sheets
  the signed-in user can access — deliberately not Drive-wide).
- **Token at rest**: `dirs::config_dir()/forge/sheet-sync/<alias>.json`
  (0600). Access tokens refreshed on demand from the refresh token.
- **Never logged**: tokens/secrets are redacted from all diagnostics (AGENTS.md
  rule). PKCE optional (adds `sha2`); classic Desktop-client secret flow is the
  v1 default.

## CLI + MCP surface

```
frg sheet auth <alias>                 # one-time OAuth; caches refresh token
frg sheet pull <alias> [--dry-run]     # sheet -> board upsert; JSON report
frg sheet push <alias> <row_id> \      # board -> sheet write-back
     [--status <sheet-value>] [--fix-ver <s>] [--notes <s>] [--dry-run]
```
- All commands accept `--cql-host` (task store) and emit structured JSON via
  `forge_shared::emit_json`.
- MCP mirrors: `sheet_pull {alias, dry_run?}`, `sheet_push {alias, row_id,
  status?, fix_ver?, notes?, dry_run?}`, `sheet_auth {alias}`. Tier-1 (always
  visible).

## Sync semantics

**Pull (sheet → board), idempotent:**
- Normalize IDs: strip trailing parentheticals/whitespace (`QA-005 (…)` → `QA-005`).
- **Duplicate ID → fail loud**: the two `QA-005` rows are reported and **skipped**
  for both import and any future push (ambiguous join). Reported in JSON, not
  silently merged.
- **Filter**: skip rows whose Status ∈ `terminal_status`; everything else imports
  (all Types — Bug / UX Issue / Feature Enhancement / …), tagged
  `type:<…>`, `severity:<…>`, `mvp-blocker` on the task (via `skills`/labels or
  title prefix; see Test plan).
- **Title** `[<id>] <Title>`; **body** = Description + Steps + Expected + Actual +
  Environment + evidence links.
- **Never move backward**: if the existing task is already in a dev-owned state,
  refresh content/priority but leave status alone (dev owns it now).

**Push (board → sheet):** writes only `status` / `fix_ver` / `resolution_notes` to
the `row_id`-matched row.
- Column located by **resolving the header string at write time**; missing header
  → hard error, no partial write.
- **Lifecycle handoff**: push `status` only when the new value ∈
  `dev_writable_status` **and** the current sheet Status ∉ client-owned/terminal
  states. Otherwise push `fix_ver` + `notes` but **not** status.
- `fix_ver` value is agent-supplied (SHA on demo, tag on prod).
- `--dry-run` prints the exact `A1:cell → old ⇒ new` diff and writes nothing.

**Source-of-truth split:** sheet owns bug content + triage (Title, Desc, Severity,
Priority, MVP, early/terminal Status); board owns dev progress (middle Status, our
notes). This split is what makes two-way safe.

## Threat model (brief — new OAuth/network surface)

- **Scope minimization**: `spreadsheets` only, not Drive.
- **Write blast radius**: engine can only emit edits for the 3 writable columns of
  a matched row; `GoogleSheets.write_cells` asserts every edit's column ∈ mapped
  `writable` before issuing the batch (defense in depth).
- **Token theft**: refresh token 0600 in user config dir; never in repo, never
  logged; access tokens in-memory only.
- **Confused deputy / wrong row**: dup/normalized-missing IDs refuse to write.
- **Supply chain**: minimal, allowlisted deps; `cargo deny` in CI.

## Test plan (must clear workspace floors: lines ≥70 / fn ≥66 / regions ≥68)

Pure-engine unit tests against `FakeSheets` (no network):
- id normalization + duplicate detection (incl. the two `QA-005`s).
- column mapping incl. missing-header hard error.
- status_map many-to-one; never-move-backward.
- lifecycle handoff: dev-owned advance writes; terminal-state push skips status,
  still writes fix_ver/notes.
- write blast-radius assertion (attempt to write an unmapped column → error).
- sidecar state round-trip (create → hit → content-change update).
- `--dry-run` emits diff, issues zero writes.

`GoogleSheets`/OAuth kept thin; covered by a `#[ignore]` live test gated on creds
(pattern: `crates/tasks/tests/board_health_live.rs`).

## Versioning, release, local install

- Bump `[workspace.package] version` `0.14.0 → 0.15.0`; add CHANGELOG entry.
- After CI-green: `cargo build --release` and install `frg` to PATH
  (`~/.cargo/bin/frg`). Tag a stable forge release per maintainer flow.

## Deliverables

1. `crates/sheet-sync` (lib + tests).
2. CLI + MCP wiring in `crates/cli`.
3. `0.15.0` bump + CHANGELOG.
4. `.forge/sheets/spoton-qa.toml` mapping (first consumer; committed as example).
5. **Claude skill** driving the loop (pull → work → push) for Google Sheets,
   shipped **in the forge repo** (e.g. `skills/sheet-sync/`) so it installs with
   forge.
6. This spec advanced todo → verified; PR to `ferrosadb/forge`.

## Open questions

- **OQ-1 (deferred):** auto-cut a prod version tag during push for
  prod-environment fixes and record it as `fix_ver` (interpretation #2 of "tag a
  release"). Out of v1; revisit after the manual loop is proven.
- **OQ-2 (resolved):** skill ships in the forge repo (`skills/sheet-sync/`).
- **OQ-3:** the sheet's two `QA-005` rows and the parenthetical-in-ID-cell need a
  human cleanup pass regardless of code.
