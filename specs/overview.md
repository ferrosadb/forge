# Forge Overview

> Last updated: 2026-07-21
> Status: Current architecture reference

## Overview

`forge` is a Rust workspace that packages token-saving developer tooling into a single CLI binary and MCP server. The core product is the Forge runtime, shipped via the `frg` executable defined in [`crates/cli`](../crates/cli/Cargo.toml), which exposes direct subcommands, MCP tools over stdio or Streamable HTTP, analytics-backed proxy execution, Claude hook integration, Goose workflow guidance, and knowledge-graph ingestion workflows.

The workspace is organized as a thin orchestration layer over focused library crates. Most crates implement one capability family, while the CLI crate handles argument parsing, MCP registration, config lookup, and output dispatch. This keeps features independently testable while preserving a single-binary distribution model.

## Workspace Shape

```mermaid
graph TD
    CLI[crates/cli<br/>single binary + MCP entrypoint]
    MCP[crates/mcp-server<br/>MCP stdio + Streamable HTTP]
    Shared[crates/shared<br/>tracking filters tee config]
    Devtools[crates/devtools<br/>language toolchain wrappers]
    Digest[crates/digest<br/>digest excerpt lookup]
    ProjectDetect[crates/project-detect]
    Dsm[crates/dsm-analyze]
    Ingest[crates/ingest]
    Tasks[crates/tasks + checklist-state]
    Fmem[fmem-client]
    Quality[quality crates<br/>test-summary log-distill diff-filter lint-dedup log-monitor coverage-gate smell-detect doc-coverage dep-tree outline merge-check concurrency-scan format-fix]

    CLI --> MCP
    CLI --> Shared
    CLI --> Devtools
    CLI --> Digest
    CLI --> ProjectDetect
    CLI --> Dsm
    CLI --> Ingest
    CLI --> Tasks
    CLI --> Fmem
    CLI --> Quality

    Ingest --> Shared
    Ingest --> Fmem
    Devtools --> Shared
    Digest --> Shared
    Quality --> Shared
```

## Runtime Modes

### 1. Direct CLI

The binary runs named subcommands such as `test-summary`, `digest`, `dsm`, `ingest`, `ingest-url`, `ingest-paper`, `run`, `init`, and `discover`.

### 2. MCP Server

`frg --mcp` starts a JSON-RPC stdio server backed by [`crates/mcp-server`](../crates/mcp-server/src/lib.rs). `frg --mcp-http --mcp-http-addr HOST:PORT` exposes the same server at `POST /mcp`. The CLI crate registers tool definitions and handlers, while the MCP server filters visible tools using tier metadata and detected project stacks.

The server supports the draft `2026-07-28` discovery/result shape in addition to
the legacy initialization path. Modern responses advertise result and tool
schemas; the HTTP endpoint validates request metadata, mirrored headers, and
origins before dispatching a tool call.

### 3. Hook Delegator

Claude hook integration resolves to a canonical delegator command:

```text
frg hook 2>/dev/null || true
```

The hook path keeps Claude settings stable while runtime logic decides which filter to apply.

### 4. Knowledge Ingestion

The ingestion pipeline turns codebases, web pages, papers, corpus Markdown, and
skill catalogs into structured entities and edges for ferrosa-memory or JSON
output. `fetch-url` and `web-search` provide bounded, sanitized read/search
results without persistence. This makes architectural context reusable across
sessions while keeping ephemeral web reads separate from durable ingestion.

## Key Constraints

- Single Cargo workspace under [`Cargo.toml`](../Cargo.toml)
- One distributed executable: `frg`
- Library crates stay focused and reusable
- MCP tool visibility is tiered to reduce token overhead
- CLI flows prefer structured JSON output over raw shell output
