# forge (frg)

Token-saving tools for coding agents. A single binary (`frg`) with structured-JSON subcommands and an MCP server, so agents can consume compact, machine-readable output instead of raw terminal noise. Forge also includes a Claude Code hook integration and a Goose workflow skill.

**Current version:** 0.14.0

[![CI](https://github.com/ferrosadb/forge/actions/workflows/ci.yml/badge.svg)](https://github.com/ferrosadb/forge/actions/workflows/ci.yml)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)

**Status:** developer preview. Forge is useful today, but the public API and MCP tool surface may still change before a 1.0 release.

## Community and support

Join the [Ferrosa Discord](https://discord.gg/BTpKMp9HRM) to discuss workflows,
ask questions, and report issues with the community and maintainers.

## Why forge?

Raw `cargo`, `go test`, `pytest`, and `git diff` output is verbose and token-expensive. `frg` wraps the most common operations and returns structured JSON: errors with file paths and line numbers, warnings grouped by rule, summaries instead of wall-of-text logs. The MCP layer surfaces the same tools directly to Claude Code without shell round-trips.

---

## Installation

```sh
git clone https://github.com/ferrosadb/forge.git
cd forge
cargo install --path crates/cli --locked
```

Add to `~/.claude/settings.json` to expose the MCP server:

```json
{
  "mcpServers": {
    "forge": {
      "command": "/path/to/frg",
      "args": ["--mcp"]
    }
  }
}
```

Install Claude Code hooks (pre-tool-call output filtering):

```sh
frg init --global
```

Use `frg init --show` to inspect the installed hook and `frg init --global --uninstall`
to remove it. The Goose integration is a repository-local skill at
[`integrations/goose/forge`](integrations/goose/forge). Configure that directory
through the Goose host's normal extension or skill-discovery settings.

---

## CLI reference

Commands that produce structured reports write JSON to stdout unless noted;
commands with an explicit text format, such as `glob --format brief`, are the
exception. Pass output through `frg hook` in hooks to filter before Claude
reads it.

### Log and text processing

| Command | Description |
|---------|-------------|
| `frg test-summary` | Parse stdin test runner output (cargo test, pytest, jest, go test, mix test) into structured pass/fail/skip counts and error locations |
| `frg log-distill [--context N]` | Extract errors and warnings from verbose build logs with surrounding context lines |
| `frg diff-filter` | Filter git diffs: collapse large hunks, skip noise files (lockfiles, generated), summarize binary changes |
| `frg lint-dedup` | Deduplicate lint warnings by rule ID; group by file for compact review |
| `frg log-monitor [--stall-threshold N] [--repeat-threshold N] [--max-events N]` | Detect log stalls, OOM signals, disk pressure, and repeated error patterns in a log stream |
| `frg mermaid-validate` | Validate Mermaid diagram syntax from stdin |

### Code quality and analysis

| Command | Description |
|---------|-------------|
| `frg coverage-gate --coverage <lcov.info> --source <dir> [--baseline N]` | Validate test coverage meets a baseline; enforces complexity-coverage coupling (high-CC code requires higher coverage) |
| `frg smell-detect [paths...]` | Find code smells: long functions, high cyclomatic complexity, deep nesting |
| `frg doc-coverage [paths...]` | Check public API documentation coverage; report undocumented exports |
| `frg threat-scan [paths...]` | Scan for STRIDE attack patterns (spoofing, tampering, repudiation, info disclosure, DoS, elevation) |
| `frg fail-loud-scan [paths...]` | Find swallowed errors, fake success returns, silent fallbacks, and mock-data leaks |
| `frg concurrency-scan [paths...] [--categories ...]` | Detect locks, mutexes, channels, consensus patterns, replication concerns |
| `frg materialization-scan <paths...> [--include-tests] [--max-findings N]` | Find likely unbounded materialization in disk/storage/query I/O paths (Rust + Go): whole-file reads (`read_to_string`/`io.ReadAll`/`os.ReadFile`), query result materialization (`rows_or_empty`/`ALLOW FILTERING` / `for rows.Next()` / `cursor.All`), `collect()` / `slices.Collect`, growing `Vec`/slice via `append`, and map-of-`Vec` / `map[K][]V` grouping |
| `frg todo-extract [paths...]` | Extract TODO/FIXME/HACK comments with git blame attribution and age |
| `frg secret-scan [path]` | Find leaked API keys, credentials, private keys, and tokens |
| `frg deps-audit [path] [--min-severity LEVEL]` | Audit dependency lockfiles (Cargo.lock, package-lock.json, etc.) for known CVEs |

### Architecture and dependencies

| Command | Description |
|---------|-------------|
| `frg dsm extract\|analyze [dir]` | Design Structure Matrix: `extract` returns raw dependency edges; `analyze` runs the full pipeline with cycle detection, cluster identification, metrics, and refactoring suggestions |
| `frg dsm dead-code [dir]` | Find unreachable declarations via BFS from entry points; `--min-confidence definite\|possible\|all`, `--include-tests`, `--exclude <glob>` (repeatable), skips generated files and `#[cfg(test)]` items |
| `frg dep-tree [dir]` | Build per-module dependency map showing import fan-in and fan-out |
| `frg api-diff` | Diff public API surface between two refs; detect breaking changes |
| `frg schema-diff` | Diff SQL/CQL/Cypher schemas between two files or refs for breaking migration detection |
| `frg merge-check` | Test branch merge-ability without side effects |

### Code discovery and structure

| Command | Description |
|---------|-------------|
| `frg digest [paths...]` | Token-efficient structural summary: functions, types, imports — no bodies. Best first call on an unfamiliar file |
| `frg excerpt <target> [--context N]` | Extract a single named symbol with surrounding context lines |
| `frg lookup <symbol> [dir]` | Find symbol definitions across the project |
| `frg outline <file>` | Extract function signatures, type definitions, and module structure from a single file |
| `frg glob <pattern> [--format brief\|json\|csv\|table]` | Find files matching a glob pattern; returns bounded per-file path, size, line count, modification time, and generated-file metadata |
| `frg project-detect [dir]` | Auto-detect project type, languages, frameworks, and applicable forge tools |
| `frg format-fix [dir] [--check]` | Auto-detect language and run the appropriate formatter; `--check` for CI |

### Ingestion and knowledge graph

These commands ingest structured knowledge into ferrosa-memory via the `agent_memory` keyspace.

| Command | Description |
|---------|-------------|
| `frg ingest [dir]` | Ingest codebase structure: modules, functions, types, and their relationships as typed entities and edges |
| `frg ingest-descriptions [dir]` | LLM-backed one-line descriptions for public entities; supports a configured provider and validates ≤60 words while rejecting prompt-leak strings |
| `frg ingest-url <url>` | Fetch a web page and ingest its structure and concepts as entities |
| `frg fetch-url <url>` | Fetch a web page through Forge's trusted HTTP path and return compact read-only text, sections, and links without persistence |
| `frg web-search <query>` | Search for candidate URLs through an explicitly configured trusted SearXNG backend (`FORGE_WEB_SEARCH_URL` or `SEARXNG_URL`) |
| `frg ingest-paper <source>` | Ingest an academic paper (see [PDF ingestion](#pdf-ingestion)) |
| `frg ingest-corpus <path>` | Ingest corpus markdown distillation files; creates L1/L2/L3 entities with deterministic UUID5 IDs |

`ingest-url` and `fetch-url` share Forge's SSRF guard, sensitive-query stripping,
5 MB response cap, prompt-injection sanitization, and relative-link resolution.
Use `ingest-url` when the page should become durable ferrosa-memory knowledge;
use `fetch-url` when an agent only needs to read the page in the current turn.
Forge ships with no default search provider: `web-search` fails loud until a
user-owned/trusted SearXNG endpoint is configured. SearXNG result titles,
snippets, engines, and URLs are treated as hostile web content: Forge strips
active/hidden markup, removes control characters, decodes entities, blocks
prompt-injection patterns, strips sensitive URL query parameters, caps returned
text, and returns only a deterministic extractive `summary` generated from the
scrubbed text. No LLM summarizer is invoked in the search path.

### Task and workflow management

| Command | Description |
|---------|-------------|
| `frg task create <title>` | Create a task in the CQL-backed kanban store |
| `frg task update <id>` | Update task status, assignee, priority, or body |
| `frg task get <id>` | Fetch a single task by ID |
| `frg task list [--status STATUS]` | List tasks; filter by status |
| `frg task link <src> <dst> <type>` | Create a typed link between two tasks |
| `frg task unlink <src> <dst> <type>` | Remove a task link |
| `frg task comment <id> <body>` | Add a comment to a task |
| `frg task board` | Render the kanban board grouped by status |
| `frg checklist <action>` | Persistent workflow checklists with dependencies, leases, attempts, waiting gates, reviews, and scoring |
| `frg fmem-skill-ingest [--root <dir>]` | Ingest the SKILL.md catalog into ferrosa-memory as typed skill entities |

#### Pointing the board at a database

The task store resolves its CQL contact point in this order, first match wins:

1. an explicit `--cql-host` / `cql_host` argument
2. the `FORGE_CQL_HOST` environment variable
3. `cql_host` in the nearest `.forge/config.toml`, walking up from the working
   directory — use this to pin a repo to its own board
4. `cql_host` in `~/.config/forge.toml` — machine-wide, for when a database
   lives somewhere other than the default
5. the built-in default, `127.0.0.1:9042`

```toml
# ~/.config/forge.toml
cql_host = "127.0.0.1:47017"
```

Same schema as the project file, and it sits beside `~/.config/ferrosa-memory.toml`,
which forge already reads for its memory client settings.

The global layer exists because every other layer depends on **where forge is
run from**: an installer that provisions a database on a non-default port could
previously only reach forge by asking the user to export a variable or by
dropping a file into every repo they own. Anything else met
`connect to CQL ... Connection refused` against a database that was running fine.

### Google Sheets sync

Two-way sync between a Google Sheet of bugs and the CQL task board. See [Google Sheets sync](#google-sheets-sync-1) for setup.

| Command | Description |
|---------|-------------|
| `frg sheet auth <alias>` | Run the one-time Google OAuth consent flow and cache a refresh token |
| `frg sheet pull <alias> [--dry-run]` | Upsert open sheet rows into the task board (idempotent, never-move-backward) |
| `frg sheet push <alias> <row_id> [--status S] [--fix-ver V] [--notes N] [--dry-run]` | Write status/fix-version/resolution-notes back to the row's own cells |

### Language-specific toolchain wrappers

Each wrapper parses native output into structured JSON with error locations, warning counts, and an actionable `hint` field on failure.

| Command | Subcommands |
|---------|-------------|
| `frg run cargo <cmd>` | `build`, `check`, `test`, `clippy`, `fmt_check` |
| `frg run go <cmd>` | `build`, `test`, `vet`, `fmt_check`, `mod_tidy` |
| `frg run dotnet <cmd>` | `build`, `test`, `format_check` |
| `frg run npm <cmd>` | `test`, `typecheck`, `lint`, `format_check`, `deps`, `build`, `audit` |
| `frg run python <cmd>` | `test`, `lint`, `format_check`, `deps`, `typecheck` |
| `frg run mix <cmd>` | `compile`, `test`, `format_check`, `deps` |

### Analytics and hooks

| Command | Description |
|---------|-------------|
| `frg gain [--json]` | Show token savings analytics since last reset |
| `frg analytics [--json]` | Detailed filter analytics per tool |
| `frg clear-analytics` | Reset analytics counters |
| `frg discover [dir]` | Scan project and suggest forge optimization opportunities |
| `frg init --global [--uninstall\|--show]` | Install, remove, or inspect the Claude Code hook configuration |
| `frg hook` | Process tool output as a Claude Code hook (reads tool name + output from env) |
| `frg context [--session ID] [--clear]` | Show or clear tracked files and symbols for an agent session |
| `frg context-check [dir]` | Report available ferrosa-memory context for a project |
| `frg tool-aliases [--format FORMAT]` | Return the alias map for tool name mismatches between callers and forge |
| `frg version` | Print installed frg version |

---

## MCP tools

The MCP server exposes Forge's agent-facing functionality directly to MCP clients. Use `frg --mcp` for stdio clients, or run `frg --mcp-http --mcp-http-addr 127.0.0.1:9977` to expose a Streamable HTTP endpoint at `POST /mcp`.

Forge supports the draft `2026-07-28` MCP discovery/result shape: `server/discover`, `resultType: "complete"`, server identity in result `_meta`, `outputSchema` on advertised tools, and cache metadata on `tools/list`. The HTTP endpoint requires the draft mirror headers (`MCP-Protocol-Version`, `Mcp-Method`, and Base64-safe `Mcp-Name` for `tools/call`), validates `Origin`, returns `405` for legacy GET/DELETE MCP endpoint traffic, and rejects client JSON-RPC response objects.

Tools are split into two tiers:

**Tier 1 — always visible:** core analysis, log processing, code quality, architecture, ingestion, task management.

**Tier 2 — stack-detected:** language toolchain wrappers appear only when the detected project stack matches (e.g., `cargo` only in Rust projects).

| MCP tool | Maps to |
|----------|---------|
| `project_detect` | `frg project-detect` |
| `project_summary` | `frg project-detect --summary` |
| `find_definition` | `frg lookup` |
| `module_outline` | `frg outline` |
| `dependency_tree` | `frg dep-tree` |
| `digest` | `frg digest` |
| `excerpt` | `frg excerpt` |
| `glob` | `frg glob` |
| `smell_detect` | `frg smell-detect` |
| `format_fix` | `frg format-fix` |
| `git_summary` | git status / log / diff (structured) |
| `list` | list all available forge tools |
| `tool_aliases` | `frg tool-aliases` |
| `version` | `frg version` |
| `test_summary` | `frg test-summary` |
| `log_distill` | `frg log-distill` |
| `diff_filter` | `frg diff-filter` |
| `lint_dedup` | `frg lint-dedup` |
| `log_monitor` | `frg log-monitor` |
| `coverage_gate` | `frg coverage-gate` |
| `doc_coverage` | `frg doc-coverage` |
| `concurrency_scan` | `frg concurrency-scan` |
| `materialization_scan` | `frg materialization-scan` |
| `dsm` | `frg dsm` |
| `dead_code` | `frg dsm dead-code` |
| `merge_check` | `frg merge-check` |
| `schema_diff` | `frg schema-diff` |
| `api_contract_diff` | `frg api-diff` |
| `threat_scan` | `frg threat-scan` |
| `fail_loud_scan` | `frg fail-loud-scan` |
| `secret_scan` | `frg secret-scan` |
| `deps_audit` | `frg deps-audit` |
| `todo_extract` | `frg todo-extract` |
| `checklist_state` | `frg checklist` |
| `fmem_skill_ingest` | `frg fmem-skill-ingest` |
| `mermaid_validate` | `frg mermaid-validate` |
| `ingest` | `frg ingest` |
| `ingest_url` | `frg ingest-url` |
| `fetch_url` | `frg fetch-url` |
| `web_search` | `frg web-search` |
| `ingest_paper` | `frg ingest-paper` |
| `ingest_corpus` | `frg ingest-corpus` |
| `task_create` | `frg task create` |
| `task_update` | `frg task update` |
| `task_get` / `task_list` | `frg task get` / `frg task list` |
| `task_link` / `task_unlink` / `task_comment` | `frg task link` / `frg task unlink` / `frg task comment` |
| `task_board` | `frg task board` |
| `sheet_auth` | `frg sheet auth` |
| `sheet_pull` | `frg sheet pull` |
| `sheet_push` | `frg sheet push` |
| `cargo` (tier 2) | `frg run cargo` |
| `clippy` (tier 2) | `frg run cargo clippy` |
| `go_tools` (tier 2) | `frg run go` |
| `dotnet` (tier 2) | `frg run dotnet` |
| `npm_tools` (tier 2) | `frg run npm` |
| `python_tools` (tier 2) | `frg run python` |
| `mix_*` (tier 2) | `frg run mix` |

---

## Google Sheets sync

Drive development from a Google Sheet of bugs: pull open rows into the CQL task
board, work them, and push fix status back to the sheet — writing only the
columns you designate, never touching the owner's other cells. Useful when a
client or QA team files bugs in a spreadsheet they own and you want an agentic
loop over them.

### One-time setup

1. **Create a Google OAuth client.** In the
   [Google Cloud Console](https://console.cloud.google.com/): *APIs & Services →
   enable the **Google Sheets API** → Credentials → Create credentials → OAuth
   client ID → Application type: **Desktop app*** → download the
   `client_secret.json`.
2. **Point forge at it** via env var (or `[google] client_secret_path` in
   `.forge/config.toml`):
   ```sh
   export FORGE_GOOGLE_OAUTH_CLIENT=/path/to/client_secret.json
   ```
3. **Write a per-sheet mapping** at `.forge/sheets/<alias>.toml` — copy and edit
   [`crates/sheet-sync/examples/spoton-qa.toml`](crates/sheet-sync/examples/spoton-qa.toml).
   It declares the `spreadsheet_id`, `tab`, the `id_column` (the sheet's stable
   per-row id), a header→field `[columns]` map, the `writable` columns (the only
   ones `push` may ever write), a `status_map` (sheet status → task status), and
   the `dev_writable_status` / `terminal_status` sets that drive the write-back
   handoff. Top-level array keys must appear **before** the `[columns]` /
   `[status_map]` tables (a TOML rule). `.forge/` is git-ignored, so your real
   ids and mappings stay local.
4. **Authorize once** — opens a browser consent page and caches a refresh token
   under `~/.config/forge/sheet-sync/` (stored `0600`, never logged):
   ```sh
   frg sheet auth <alias>
   ```
   The OAuth scope is limited to `spreadsheets`.

### Headless auth (service account)

For agents/CI, or anyone who'd rather skip the browser consent flow
entirely: run
[`crates/sheet-sync/examples/provision-service-account.sh`](crates/sheet-sync/examples/provision-service-account.sh)
(requires `gcloud`, already logged in) to create a Google service account
and download its key JSON, then **share the target Google Sheet with the
printed service-account email as Editor** — a fresh service account has no
access to anything until you do this. Point forge at the key —
`export FORGE_GOOGLE_SERVICE_ACCOUNT=/path/to/sa-key.json` (or
`[google] service_account_path` in `.forge/config.toml`) — and skip
`frg sheet auth` entirely: it's a no-op once a service account is
configured, and `frg sheet pull`/`push` mint tokens directly, no browser
required.

### Workflow

```sh
frg sheet pull <alias> --dry-run     # preview what would import
frg sheet pull <alias>               # upsert open rows into the task board
# ...pick a task, fix the bug, then:
frg sheet push <alias> <row_id> \
    --status "In Progress" --fix-ver "<git sha on demo | version tag on prod>" \
    --notes "what was done" --dry-run          # preview the exact cell diff
frg sheet push <alias> <row_id> --status "In Progress" --fix-ver "abc1234" --notes "..."
```

`pull` is idempotent — keyed by the sheet's row id, tracked in a sidecar
`.forge/sheets/<alias>.state.toml` — and never moves a task backward. The task
board is the CQL store (`FORGE_CQL_HOST`, same as `frg task`).

### Write-back safety

- Only the configured `writable` columns are ever written — enforced in the
  planner **and** re-checked at the write boundary.
- Rows are matched by normalized id; **duplicate ids, or duplicate mapped-header
  text, fail loud** rather than risk writing the wrong cell.
- Status is only ever advanced to a **dev-owned** value (e.g. *In Progress*, *In
  Review*, *Fixed - Needs Verification*); client-owned or terminal states (*New*,
  *Verified/Closed*, *Won't Fix*, …) are never overwritten. `fix_ver` and
  resolution notes are always yours to write.
- `--dry-run` on both commands prints the effect and writes nothing.

## PDF ingestion

`frg ingest-paper` accepts:

- arxiv URLs (e.g. `https://arxiv.org/abs/2310.01234`)
- DOI links (`doi:10.xxxx/yyyy`)
- Semantic Scholar links
- bioRxiv / medRxiv / PubMed URLs
- IEEE / ACM URLs (opens browser for paywalled access)
- Generic web URLs
- **Local PDF files** (full structured extraction)

For local PDFs, the pipeline does structured extraction — not simple `pdftotext`. It is comparable in intent to [Docling](https://github.com/DS4SD/docling):

### What it extracts

The `crates/ingest/src/pdf/` module provides a typed element taxonomy and XY-Cut++ reading-order reconstruction:

| Element type | What it captures |
|-------------|-----------------|
| `Heading` | Section headings with depth level (1 = title, 2 = section, 3 = subsection, …) |
| `Paragraph` | Body text blocks with bounding box and font metadata |
| `Table` | Tabular content with detected row/column shape |
| `List` | Bulleted or numbered list items |
| `Image` | Figure regions (bounding box only; no raster content) |
| `Caption` | Figure and table captions |
| `Formula` | Mathematical expressions |
| `HeaderFooter` | Running headers and footers (filtered from body) |
| `Watermark` | Background watermarks (filtered from body) |

Each element carries: page number, bounding box (left/bottom/right/top), text content, font name, font size, heading depth, and for tables, detected row/column count.

### Reading order

The `reading_order.rs` module implements **XY-Cut++**: it recursively bisects the page's bounding-box tree, first horizontally then vertically, to recover logical reading order from spatial layout. This correctly handles multi-column academic papers where a naïve top-to-bottom scan would interleave columns.

### What gets ingested into ferrosa-memory

After extraction, `ingest-paper` writes to ferrosa-memory:

- **Document entity** — title, authors, abstract, year, DOI/URL
- **Person entities** — one per author with affiliation
- **Section entities** — heading hierarchy with depth, linked via `contains` edges
- **Concept entities** — key terms extracted from abstract and section headings
- **Typed edges** — `wrote` (author→paper), `references`, `discusses`, `affiliated_with`, `contains` (paper→section)

### Compared to Docling

| Capability | forge `ingest-paper` | Docling |
|-----------|---------------------|---------|
| Reading order reconstruction | XY-Cut++ | DiT layout model + XY-Cut |
| Table detection | Row/col shape heuristics | Table Transformer (ML) |
| Formula handling | Type-tagged, not rendered | MathML export |
| Output target | ferrosa-memory knowledge graph | JSON / Markdown / Docling Doc |
| Image content | Bounding box only | Image export |
| OCR | No (text-layer PDFs only) | Yes (Tesseract / EasyOCR) |

If you need OCR'd PDFs or ML-quality table extraction, pre-process with Docling and ingest the Markdown output via `frg ingest-url` or a direct ferrosa-memory `smart_ingest` call.

---

## Project structure

```
crates/
  cli/          frg binary — subcommand dispatch, MCP server, hook runner
  ingest/       Ingestion pipelines
    src/
      pdf/        PDF structured extraction (element.rs, reading_order.rs, processors/)
      paper.rs    Academic paper ingestion orchestrator
      corpus.rs   Corpus markdown distillation ingestion
      url.rs      Web page fetching and ingestion
      graph_loader.rs  Code graph → ferrosa-memory loader
      lsp.rs      LSP-backed symbol resolution
      descriptions/   LLM-backed entity description generation
      skill_ingest/   SKILL.md catalog ingestion
  tasks/        CQL-backed task store (TaskStore, schema, MCP tools)
hooks/          Claude Code hook scripts
specs/          Architecture decision records and feature specs
docs/           Extended documentation
```

## Development

```sh
git clone https://github.com/ferrosadb/forge.git
cd forge
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo build --workspace --all-targets
```

Before opening a PR or release, use `cargo fmt --all -- --check` rather than
the mutating formatter command above. CI ([`ci.yml`](.github/workflows/ci.yml))
runs format checking, clippy, build, and tests. All commands must be run from
the repository root — the workspace root for this crate tree.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md). Security reports should follow [SECURITY.md](SECURITY.md).

## License

Apache-2.0. See [LICENSE](LICENSE).
