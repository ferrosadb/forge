# Forge ↔ Claude Code Hooks Integration

> How forge MCP tools integrate with Claude Code hooks so that a tool failure automatically reminds Claude of the correct forge tool.

## Overview

Claude Code supports `PostToolUseFailure` hooks that fire when a tool call fails. A hook can return `additionalContext` JSON that Claude then sees on its next turn. We use this mechanism to convert recoverable failures into hints about which forge tool to use instead.

## Installation

Installed via `setup.sh` in the research repo:

1. Script `forge-failure-hint.sh` copied to `~/.claude/hooks/`.
2. `~/.claude/settings.json` gains a `PostToolUseFailure` Bash-matcher entry pointing at the script.
3. A `SessionStart` reminder listing forge tools is already installed via `forge-mcp-hint.sh`.

Each per-project `.claude/settings.json` for `ferrosa`, `ferrosa-dbaas`, `ferrosa-memory` also installs a project-local copy so the hint fires even when global hooks are disabled.

## Failure → forge-tool mapping (canonical)

The hook script pattern-matches the failing Bash command against the left column and injects the right-column hint. Patterns are matched in order; the first match wins.

| Failing command pattern | Preferred forge tool | Reason |
|---|---|---|
| `cargo test`, `cargo nextest run`, `pytest`, `mix test`, `go test`, `jest`, `vitest` | `mcp__forge__test_summary` | Summarizes test output deterministically; strips ANSI, groups failures, saves ~80% of tokens vs raw output |
| `cargo build`, `mix compile`, `tsc`, `go build` with errors | `mcp__forge__log_distill` | Extracts actionable errors/warnings with context, drops linker noise |
| `cargo clippy`, `eslint`, `ruff check`, `golangci-lint` | `mcp__forge__lint_dedup` | Groups identical lints by rule; massive dedup for big codebases |
| `grep -r "TODO"`, `rg TODO`, `grep -rn "FIXME"` | `mcp__forge__todo_extract` | Structured inventory with git blame, staleness buckets |
| `grep -r "AKIA"`, `grep -r "BEGIN.*PRIVATE KEY"` | `mcp__forge__secret_scan` | All secret patterns in one pass, redacts snippets |
| `osv-scanner`, `npm audit`, `cargo audit` failures | `mcp__forge__deps_audit` | Embedded vuln DB, works offline |
| `cat huge.log`, `tail -n 10000 *.log` | `mcp__forge__log_distill` or `mcp__forge__log_monitor` | Log summarization; distill for post-hoc, monitor for live watching |
| `grep -E "SELECT.*\\+"`, hand-rolled SQL injection scans | `mcp__forge__threat_scan` | Full STRIDE pattern catalog including TAMPER-001 SQL concat |
| `mermaid-cli -i`, `mmdc` failures | `mcp__forge__mermaid_validate` | Pure-Rust syntax check, catches errors before rendering |
| `diff -u schema-v1.sql schema-v2.sql` | `mcp__forge__schema_diff` | Semantic diff with breaking/minor/patch severity |
| `git diff main HEAD -- src/*.rs` for API review | `mcp__forge__api_contract_diff` | Extracts public API surface, classifies changes |
| `wc -l crates/*/src/*.rs` for sizing | `mcp__forge__project_summary` | LOC + module inventory |
| `git log --format=...`, `git shortlog` | `mcp__forge__git_summary` | Structured summary |
| `grep -r "goroutine\|Mutex\|channel"` for concurrency review | `mcp__forge__concurrency_scan` | Full concurrency pattern catalog |
| `find . -name "*.rs" -exec wc -l {} \\;` for smell detection | `mcp__forge__smell_detect` | Cyclomatic complexity, nesting, long-function detection |
| `find . -name "Cargo.toml" -exec ...`, hand-rolled dep tree walks | `mcp__forge__dependency_tree` | Per-module dependency graph |
| `tree -L 3`, manual directory inventory | `mcp__forge__digest` or `mcp__forge__module_outline` | Token-bounded structural summary |
| Stale checklist: "where was I in the blueprint phases?" | `mcp__forge__checklist_state` (`mode: show`) | Multi-session workflow state |

## Hook script contract

`~/.claude/hooks/forge-failure-hint.sh` reads `PostToolUseFailure` JSON from stdin. The relevant fields:

```json
{
  "tool_name": "Bash",
  "tool_input": { "command": "cargo test --release", "description": "Run tests" },
  "error": "Command exited with non-zero status code 1"
}
```

On match, the script writes a JSON object to stdout:

```json
{
  "hookSpecificOutput": {
    "hookEventName": "PostToolUseFailure",
    "additionalContext": "tool failure — prefer mcp__forge__test_summary for Rust test output (handles compile+test, ~80% token savings)"
  }
}
```

On no-match or error, the script exits 0 with empty stdout — no injection.

## Design rules

1. **One-line hints.** Claude's next turn sees a short instruction, not a tutorial. A sentence with the tool name and reason.
2. **Don't block.** `PostToolUseFailure` can't block anyway, but the hook never exits nonzero — a broken hook must not poison Claude's session.
3. **Pattern-match, don't fuzzy-match.** The command string is matched with literal substrings or anchored regex. No semantic parsing. Ambiguous matches are skipped.
4. **Idempotent.** Running the hook twice on the same input produces the same output.
5. **Single source of truth.** This document *is* the mapping. The hook script is generated from it (or manually kept in sync — the script has a comment pointing here).
6. **No network.** The hook runs on every Bash failure; it must not block, network, or disk-touch anything beyond stdin/stdout.

## Extension policy

Adding a new mapping:

1. Add a row to the table above.
2. Add the corresponding `case` branch to `forge-failure-hint.sh` with a literal substring or anchored pattern.
3. No test required — the hook is best-effort; worst case is "no hint fired" which is what the user sees today.

Removing a mapping: delete the row, delete the case branch. Past sessions won't see the hint but there's no cleanup required.

## Related files

- Script: `hooks/forge-failure-hint.sh` (source of truth — installed into `~/.claude/hooks/` and per-project `.claude/hooks/` by `setup.sh`)
- Global installer: `setup.sh` in the research repo
- Per-project installer: also `setup.sh` (copies into the three ferrosa repos that have `.claude/` directories)
- Session-start reminder (different hook): `hooks/forge-mcp-hint.sh`
