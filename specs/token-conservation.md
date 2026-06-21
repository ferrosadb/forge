# Forge Token Conservation Spec

## Problem

Forge registers 27 MCP tools. At ~350 tokens per tool definition, that's ~9,500 tokens consumed in every conversation just for tool definitions — before the LLM does anything. Combined with ferrosa-memory's 39 tools (now tiered to 15 visible), the total MCP overhead can exceed 25,000 tokens.

Many forge are language-specific (mix_compile, npm_tools, python_tools, go_tools) or situational (docker_status, ci_cd, merge_check). Loading all 27 for every project wastes tokens.

## Current Tool Inventory (27 tools)

### Analysis Tools (used often, any project)
- `digest` — codebase summarizer
- `dsm` — dependency structure matrix
- `ingest` — codebase→memory ingestion
- `smell_detect` — code smell detection
- `module_outline` — module structure
- `find_definition` — symbol lookup
- `dependency_tree` — dep tree visualization
- `git_summary` — git history summary
- `project_detect` — detect project type/stack
- `project_summary` — project overview

### Quality Tools (used sometimes)
- `coverage_gate` — coverage threshold check
- `concurrency_scan` — race condition detection
- `lint_dedup` — deduplicate lint warnings
- `doc_coverage` — documentation coverage
- `test_summary` — test result summary
- `diff_filter` — filter diffs by pattern
- `format_fix` — auto-format code
- `merge_check` — merge readiness check

### Language-Specific Tools (only relevant per stack)
- `cargo` — Rust build/test
- `go_tools` — Go build/test/vet
- `mix_compile` / `mix_test` / `mix_format_check` / `mix_deps` — Elixir
- `npm_tools` — Node.js
- `python_tools` — Python

### Infrastructure Tools
- `ci_cd` — CI pipeline status
- `docker_status` — container status
- `log_distill` / `log_monitor` — log analysis

## Proposed Tiering

### Tier 1: Always Visible (~10 tools, ~3,500 tokens)
The tools every project needs:
```
digest, dsm, ingest, project_detect, project_summary,
module_outline, find_definition, git_summary, test_summary, diff_filter
```

### Tier 2: Stack-Detected (loaded based on project_detect result)

On first call, `project_detect` identifies the stack. Then load only the relevant language tools:

| Stack | Tools Added |
|-------|------------|
| Rust | `cargo` |
| Elixir | `mix_compile`, `mix_test`, `mix_format_check`, `mix_deps` |
| Node.js | `npm_tools` |
| Python | `python_tools` |
| Go | `go_tools` |

This means a Rust project sees 11 tools instead of 27.

### Tier 3: On-Demand (loaded via hints)
```
coverage_gate, concurrency_scan, lint_dedup, doc_coverage,
format_fix, merge_check, smell_detect, dependency_tree,
ci_cd, docker_status, log_distill, log_monitor
```

These get suggested by Tier 1 tool responses:
- `test_summary` with failures → hint: "Try `concurrency_scan` for race conditions or `lint_dedup` to consolidate warnings"
- `git_summary` before merge → hint: "Run `merge_check` to verify merge readiness"
- `digest` on unfamiliar codebase → hint: "Run `smell_detect` for code quality or `doc_coverage` for documentation gaps"

## Implementation

### 1. Stack-Aware Tool Loading

In the MCP server's `tools/list` handler:

```rust
fn tools_for_tier(detected_stack: Option<&str>) -> Vec<ToolDef> {
    let mut tools = tier1_tools(); // always visible
    if let Some(stack) = detected_stack {
        tools.extend(stack_tools(stack)); // language-specific
    }
    tools
}
```

The server runs `project_detect` on initialize (using the MCP roots path) and caches the result.

### 2. Progressive Disclosure Hints

Same pattern as ferrosa-memory: tool responses include `_hint` when conditions suggest a Tier 3 tool.

### 3. Token Budget

| Config | Tool Count | Tokens |
|--------|-----------|--------|
| Current (all) | 27 | ~9,500 |
| Tier 1 only | 10 | ~3,500 |
| Tier 1 + Rust | 11 | ~3,850 |
| Tier 1 + Elixir | 14 | ~4,900 |
| **Savings** | | **~5,000-6,000 tokens** |

### 4. Combined with ferrosa-memory

| Service | Current | Tiered | Savings |
|---------|---------|--------|---------|
| ferrosa-memory | ~13,500 (39 tools) | ~5,250 (15 tools) | ~8,250 |
| forge | ~9,500 (27 tools) | ~3,850 (11 tools, Rust) | ~5,650 |
| **Total** | **~23,000** | **~9,100** | **~13,900** |

**60% reduction in MCP token overhead.**

### 5. Integration with ferrosa-memory

When forge `ingest` runs, it should store results in ferrosa-memory via `smart_ingest` + `batch_create_edges`. This way:
- `ingest` indexes a codebase once
- Future sessions find architecture knowledge in memory
- No need to re-run `digest` or `module_outline` every time

The `ingest_url` command (new, for web content) follows the same pattern.

## Progressive Disclosure Rules

| Tier 1 Tool | Condition | Suggested Tier 3 Tool |
|-------------|-----------|----------------------|
| `test_summary` | failures > 0 | `concurrency_scan`, `lint_dedup` |
| `git_summary` | uncommitted + remote ahead | `merge_check` |
| `digest` | first run on project | `smell_detect`, `doc_coverage` |
| `project_detect` | (always) | loads stack-specific tools |
| `diff_filter` | large diff | `format_fix` to auto-clean |
| `module_outline` | high coupling | `dsm` for deeper analysis |
| `ingest` | complete | `smell_detect` for quality scan |

## Files to Modify

- `crates/cli/src/main.rs` — add tier field to registrations, filter in tools/list
- `crates/mcp-server/src/lib.rs` — add tier support to ToolDef, filter logic
- Tool handlers — add `_hint` fields to responses
