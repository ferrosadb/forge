# forge (frg)

Token-saving tools for Claude Code. A single binary (`frg`) with structured-JSON subcommands and a matching MCP server, so Claude Code reads compact machine-readable output instead of raw terminal noise.

**Current version:** 0.13.5

[![CI](https://github.com/ferrosadb/forge/actions/workflows/ci.yml/badge.svg)](https://github.com/ferrosadb/forge/actions/workflows/ci.yml)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)

**Status:** developer preview. Forge is useful today, but the public API and MCP tool surface may still change before a 1.0 release.

## Why forge?

Raw `cargo`, `go test`, `pytest`, and `git diff` output is verbose and token-expensive. `frg` wraps the most common operations and returns structured JSON: errors with file paths and line numbers, warnings grouped by rule, summaries instead of wall-of-text logs. The MCP layer surfaces the same tools directly to Claude Code without shell round-trips.

---

## Installation

```sh
git clone https://github.com/ferrosadb/forge.git
cd forge
cargo build --release
cp target/release/frg ~/.cargo/bin/frg    # or wherever is on your PATH
```

Add to `~/.claude/settings.json` to expose the MCP server:

```json
{
  "mcpServers": {
    "forge": {
      "command": "/path/to/frg",
      "args": ["run"]
    }
  }
}
```

Install Claude Code hooks (pre-tool-call output filtering):

```sh
frg init --install
```

---

## CLI reference

All commands write JSON to stdout unless noted. Pass output through `frg hook` in hooks to filter before Claude reads it.

### Log and text processing

| Command | Description |
|---------|-------------|
| `frg test-summary` | Parse stdin test runner output (cargo test, pytest, jest, go test, mix test) into structured pass/fail/skip counts and error locations |
| `frg log-distill [--context N]` | Extract errors and warnings from verbose build logs with surrounding context lines |
| `frg diff-filter` | Filter git diffs: collapse large hunks, skip noise files (lockfiles, generated), summarize binary changes |
| `frg lint-dedup` | Deduplicate lint warnings by rule ID; group by file for compact review |
| `frg log-monitor [--interval N]` | Detect log stalls, OOM signals, disk pressure, and repeated error patterns in a running log stream |
| `frg mermaid-validate` | Validate Mermaid diagram syntax from stdin |

### Code quality and analysis

| Command | Description |
|---------|-------------|
| `frg coverage-gate [--baseline N]` | Validate test coverage meets baseline; enforces complexity-coverage coupling (high-CC code requires higher coverage) |
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
| `frg glob <pattern> [dir]` | Find files matching a glob pattern; returns per-file size, line count, and symbol summary |
| `frg project-detect [dir]` | Auto-detect project type, languages, frameworks, and applicable forge tools |
| `frg format-fix [dir] [--check]` | Auto-detect language and run the appropriate formatter; `--check` for CI |

### Ingestion and knowledge graph

These commands ingest structured knowledge into ferrosa-memory via the `agent_memory` keyspace.

| Command | Description |
|---------|-------------|
| `frg ingest [dir]` | Ingest codebase structure: modules, functions, types, and their relationships as typed entities and edges |
| `frg ingest-descriptions [file]` | LLM-backed one-line descriptions for public entities; uses a local Ollama model; validates ≤60 words, rejects prompt-leak strings |
| `frg ingest-url <url>` | Fetch a web page and ingest its structure and concepts as entities |
| `frg ingest-paper <source>` | Ingest an academic paper (see [PDF ingestion](#pdf-ingestion)) |
| `frg ingest-corpus <path>` | Ingest corpus markdown distillation files; creates L1/L2/L3 entities with deterministic UUID5 IDs |

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
| `frg checklist <action>` | Persistent workflow checklists with DAG dependencies |
| `frg fmem-skill-ingest [dir]` | Ingest the SKILL.md catalog into ferrosa-memory as typed skill entities |

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
| `frg init --install\|--uninstall` | Install or remove the Claude Code pre-tool-call hook |
| `frg hook` | Process tool output as a Claude Code hook (reads tool name + output from env) |
| `frg tool-aliases [--format FORMAT]` | Return the alias map for tool name mismatches between callers and forge |
| `frg version` | Print installed frg version |

---

## MCP tools

The MCP server (activated by `frg run`) exposes the same functionality directly to Claude Code. Tools are split into two tiers:

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
| `ingest_paper` | `frg ingest-paper` |
| `ingest_corpus` | `frg ingest-corpus` |
| `task_create` | `frg task create` |
| `task_update` | `frg task update` |
| `task_board` | `frg task board` |
| `cargo` (tier 2) | `frg run cargo` |
| `go_tools` (tier 2) | `frg run go` |
| `dotnet` (tier 2) | `frg run dotnet` |
| `npm_tools` (tier 2) | `frg run npm` |
| `python_tools` (tier 2) | `frg run python` |
| `mix_*` (tier 2) | `frg run mix` |

---

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
cargo clippy --workspace -- -D warnings
cargo test --workspace
cargo build --release
```

CI (`forge.yml`) runs: format check, clippy, build, test. All commands must be run from the repository root — the workspace root for this crate tree.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md). Security reports should follow [SECURITY.md](SECURITY.md).

## License

Apache-2.0. See [LICENSE](LICENSE).
