#!/bin/bash
# forge-failure-hint.sh — PostToolUseFailure hook for Claude Code.
#
# When a Bash tool call fails, this script inspects the failing command and
# injects an additionalContext hint pointing at the right forge MCP tool.
#
# Source of truth for the mapping:
#   specs/hooks-integration.md
#
# Installed by setup.sh into:
#   ~/.claude/hooks/forge-failure-hint.sh                  (global)
#   <project>/.claude/hooks/forge-failure-hint.sh           (per-project, optional)
#
# Wired via settings.json:
#   {
#     "hooks": {
#       "PostToolUseFailure": [
#         {
#           "matcher": "Bash",
#           "hooks": [
#             {"type": "command", "command": "\"$CLAUDE_PROJECT_DIR\"/.claude/hooks/forge-failure-hint.sh"}
#           ]
#         }
#       ]
#     }
#   }
#
# Exit behavior:
#   0 — always. Stdout is either a JSON injection or empty (no-match).
#
# Design rules (see hooks-integration.md):
#   - One-line hints.
#   - Don't block.
#   - Pattern-match, don't fuzzy-match.
#   - Idempotent.
#   - No network, no disk writes, no subshells beyond the stdin read.

set -u

# Read stdin into a variable. Missing `jq` means no-op (we won't guess).
if ! command -v jq >/dev/null 2>&1; then
  exit 0
fi

INPUT="$(cat 2>/dev/null || true)"
[ -z "$INPUT" ] && exit 0

TOOL_NAME=$(printf '%s' "$INPUT" | jq -r '.tool_name // empty' 2>/dev/null)
[ "$TOOL_NAME" = "Bash" ] || exit 0

COMMAND=$(printf '%s' "$INPUT" | jq -r '.tool_input.command // empty' 2>/dev/null)
[ -z "$COMMAND" ] && exit 0

HINT=""

# --- Test runners ---
if printf '%s' "$COMMAND" | grep -qE '(^|[[:space:]])(cargo[[:space:]]+(test|nextest)|pytest|mix[[:space:]]+test|go[[:space:]]+test|jest|vitest|npm[[:space:]]+test)'; then
  HINT="Tool failure — prefer mcp__forge__test_summary for test-runner output. It handles Rust, Python, JS, Go, and Elixir deterministically and saves ~80% of tokens vs raw output."

# --- Build / compile ---
elif printf '%s' "$COMMAND" | grep -qE '(^|[[:space:]])(cargo[[:space:]]+build|mix[[:space:]]+compile|tsc([[:space:]]|$)|go[[:space:]]+build|npm[[:space:]]+run[[:space:]]+build)'; then
  HINT="Tool failure — prefer mcp__forge__log_distill to extract actionable errors and warnings from the build output without the linker/boilerplate noise."

# --- Linters ---
elif printf '%s' "$COMMAND" | grep -qE '(^|[[:space:]])(cargo[[:space:]]+clippy|eslint|ruff[[:space:]]+check|golangci-lint|credo([[:space:]]|$))'; then
  HINT="Tool failure — prefer mcp__forge__lint_dedup to group identical lints by rule. Huge dedup win when a lint is triggered in many files."

# --- Vulnerability scans ---
elif printf '%s' "$COMMAND" | grep -qE '(^|[[:space:]])(osv-scanner|npm[[:space:]]+audit|cargo[[:space:]]+audit|safety[[:space:]]+check|bundler-audit|mix[[:space:]]+deps\.audit)'; then
  HINT="Tool failure — prefer mcp__forge__deps_audit. Parses Cargo.lock, package-lock.json, mix.lock, go.sum, requirements.txt with an embedded offline CVE/advisory database."

# --- Secret / credential grep patterns ---
elif printf '%s' "$COMMAND" | grep -qE '(grep|rg|ripgrep).*(AKIA|BEGIN[[:space:]]+(RSA|OPENSSH|PGP|DSA|EC)[[:space:]]+PRIVATE[[:space:]]+KEY|ghp_|xox[baprs]-|sk_(live|test)_)'; then
  HINT="Tool failure — prefer mcp__forge__secret_scan. Covers AWS/GCP/GitHub/Slack/Stripe tokens, JWTs, private keys, and password assignments in a single pass with masked snippets."

# --- TODO / FIXME / HACK inventory ---
elif printf '%s' "$COMMAND" | grep -qE '(grep|rg|ripgrep)[[:space:]].*(TODO|FIXME|HACK|XXX)'; then
  HINT="Tool failure — prefer mcp__forge__todo_extract. Structured inventory with git blame, staleness buckets, and per-kind summary."

# --- Log reading / tailing ---
elif printf '%s' "$COMMAND" | grep -qE '(^|[[:space:]])(tail[[:space:]]+-n[[:space:]]*[0-9]{3,}|cat[[:space:]].*\.log|less[[:space:]].*\.log|head[[:space:]]+-n[[:space:]]*[0-9]{3,})'; then
  HINT="Tool failure — prefer mcp__forge__log_distill (post-hoc) or mcp__forge__log_monitor (live) for log analysis instead of raw cat/tail."

# --- SQL injection / threat pattern scans ---
elif printf '%s' "$COMMAND" | grep -qE '(grep|rg|ripgrep).*(eval\(|shell=True|SELECT.*\+|pickle\.loads|yaml\.load[^_])'; then
  HINT="Tool failure — prefer mcp__forge__threat_scan. Runs the full STRIDE pattern catalog (spoofing/tampering/repudiation/info_disclosure/dos/elevation) in one pass."

# --- Mermaid render attempts ---
elif printf '%s' "$COMMAND" | grep -qE '(^|[[:space:]])(mmdc|mermaid-cli)'; then
  HINT="Tool failure — prefer mcp__forge__mermaid_validate to lint Mermaid syntax before attempting to render. Pure-Rust, no JS runtime required."

# --- SQL / schema diff via diff(1) ---
elif printf '%s' "$COMMAND" | grep -qE '(^|[[:space:]])diff[[:space:]].*(\.sql|\.cql|\.cypher)'; then
  HINT="Tool failure — prefer mcp__forge__schema_diff. Semantic diff with breaking/minor/patch severity and suggested semver bump."

# --- git diff for API review ---
elif printf '%s' "$COMMAND" | grep -qE 'git[[:space:]]+diff[[:space:]]+.*(src/|lib/|\.rs|\.ts|\.py|\.go)'; then
  HINT="Tool failure — consider mcp__forge__api_contract_diff if you are doing API review. It extracts public symbols and classifies changes as breaking/minor/patch."

# --- git log / shortlog ---
elif printf '%s' "$COMMAND" | grep -qE '(^|[[:space:]])git[[:space:]]+(log|shortlog)[[:space:]]+--format'; then
  HINT="Tool failure — prefer mcp__forge__git_summary for structured commit/contributor summaries."

# --- Concurrency pattern grep ---
elif printf '%s' "$COMMAND" | grep -qE '(grep|rg|ripgrep).*(goroutine|Mutex|Arc<|channel|async[[:space:]]+fn|tokio::spawn)'; then
  HINT="Tool failure — prefer mcp__forge__concurrency_scan for concurrency and distributed-systems pattern inventory."

# --- Project detection / directory tree ---
elif printf '%s' "$COMMAND" | grep -qE '(^|[[:space:]])(tree[[:space:]]+-L|find[[:space:]]+\.[[:space:]]+-name.*\.(rs|py|go|ts|ex)[[:space:]]+-exec)'; then
  HINT="Tool failure — prefer mcp__forge__project_summary or mcp__forge__module_outline for token-bounded structural overviews."

fi

# No match → exit silently.
[ -z "$HINT" ] && exit 0

# Emit the additionalContext injection. No error-handling: jq is a dependency.
jq -n --arg msg "$HINT" '{
  "hookSpecificOutput": {
    "hookEventName": "PostToolUseFailure",
    "additionalContext": $msg
  }
}'

exit 0
