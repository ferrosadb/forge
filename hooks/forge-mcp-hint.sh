#!/bin/bash
# forge-mcp-hint.sh - SessionStart hook
# Prints MCP tool preference when forge is available.
# Output is injected into session context.

if ! command -v frg &>/dev/null; then
  exit 0
fi

cat <<'EOF'
The forge MCP server is available. Its tools return structured JSON and save tokens vs raw Bash output. Use them for build/test/lint when applicable — Claude Code will resolve the tool names automatically.
EOF
