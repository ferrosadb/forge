# Forge Token Conservation

> Last updated: 2026-07-21
> Status: Current behavior reference

## Purpose

Forge reduces agent context use by replacing raw command output with bounded,
structured results and by making MCP tool visibility explicit. This document
describes the shipped visibility model, not the earlier 27-tool proposal.

## Current MCP behavior

`frg --mcp` and `frg --mcp-http` register the same tool set. `tools/list`
returns only tools visible for the detected project stack and includes each
tool's input schema, a common output schema, and modern result metadata. The
MCP `list` tool returns the currently visible names, descriptions, and tiers,
and is the authoritative runtime inventory for a client.

Tool definitions have three tiers:

| Tier | Visibility | Shipped use |
|---|---|---|
| 1 | Always listed | General analysis, ingestion, task/checklist state, security/quality scans, and discovery tools |
| 2 | Language-associated; runtime visibility is determined from detected stacks | `cargo`, `clippy`, `go_tools`, `dotnet`, `mix_*`, `npm_tools`, and `python_tools` |
| 3 | Registered but omitted from normal `tools/list` | `docker_status` and `ci_cd` |

The exact count is intentionally not a public API: it varies with detected
stacks, compatibility behavior, and new tools. Clients should discover the
current set instead of hard-coding an inventory.

## Bounded output rules

- Commands emit JSON to stdout by default; `--pretty` is opt-in readability.
- Parsers summarize diagnostics and cap findings, log events, diffs, search
  results, and file traversal rather than returning raw unbounded streams.
- `glob` enforces result/depth limits and a non-overridable secret-file denylist.
- `fetch-url` and `web-search` sanitize hostile web content and bound returned
  text; search requires an explicitly configured trusted SearXNG backend.
- MCP tool results expose structured content and a text fallback so clients do
  not have to parse prose to recover the payload.

## Discovery guidance

1. Start with `project_detect` or `project_summary` on an unfamiliar project.
2. Use `glob`, `digest`, `module_outline`, `find_definition`, or `excerpt` to
   narrow source context before reading whole files.
3. Use a focused analysis or language wrapper only when the evidence points to
   it. The MCP `list` tool provides the current visible inventory.
4. Use `fetch_url` for an ephemeral read and `ingest_url` only when sanitized
   content should become durable ferrosa-memory knowledge.

The server may add tools in a compatible release. Consumers must rely on
`tools/list` schemas rather than tool counts, prose descriptions, or an older
static manifest.
