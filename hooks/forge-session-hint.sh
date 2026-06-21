#!/bin/bash
# forge-session-hint.sh — SessionStart hook.
#
# Injects a short reminder that the forge MCP server is available and
# enumerates the token-saving tools worth preferring over raw bash.
# Runs on session start, resume, clear, and compact.
#
# Installed by setup.sh into:
#   ~/.claude/hooks/forge-session-hint.sh                (global)
#   <project>/.claude/hooks/forge-session-hint.sh         (per-project)
#
# Exits 0 always. Outputs additionalContext JSON on stdout for Claude
# Code to merge into session context.

set -u

if ! command -v jq >/dev/null 2>&1; then
  exit 0
fi

# Only emit the hint if forge is actually reachable — no point telling
# Claude to use a server that isn't running.
if ! command -v frg >/dev/null 2>&1; then
  exit 0
fi

MSG=$(cat <<'EOF'
forge MCP server is available. Prefer these tools over raw bash for the listed operations (structured JSON, big token savings):

- Tests  →  mcp__forge__test_summary    (cargo/pytest/jest/go/mix)
- Builds →  mcp__forge__log_distill     (cargo build, tsc, mix compile)
- Lints  →  mcp__forge__lint_dedup      (clippy, eslint, ruff, golangci)
- Logs   →  mcp__forge__log_monitor     (live)  /  log_distill (post-hoc)
- TODOs  →  mcp__forge__todo_extract    (with git blame, staleness)
- Secrets→  mcp__forge__secret_scan     (AWS/GCP/GitHub/Stripe/JWT/keys)
- Deps   →  mcp__forge__deps_audit      (offline CVE + supply-chain DB)
- Threats→  mcp__forge__threat_scan     (STRIDE pattern catalog)
- Concur →  mcp__forge__concurrency_scan
- Mermaid→  mcp__forge__mermaid_validate  (before writing diagrams)
- Schema →  mcp__forge__schema_diff     (SQL/CQL/Cypher breaking diff)
- API    →  mcp__forge__api_contract_diff  (public surface diff)
- DSM    →  mcp__forge__dsm             (architecture analysis)
- Outline→  mcp__forge__module_outline / outline / digest
- Project→  mcp__forge__project_detect / project_summary
- Smells →  mcp__forge__smell_detect
- Docs   →  mcp__forge__doc_coverage
- Git    →  mcp__forge__git_summary
- State  →  mcp__forge__checklist_state  (persistent workflow state)

Full mapping: specs/hooks-integration.md
EOF
)

jq -n --arg msg "$MSG" '{
  "hookSpecificOutput": {
    "hookEventName": "SessionStart",
    "additionalContext": $msg
  }
}'

exit 0
