# Forge Tool Aliases

## Problem

LLM agents frequently invoke tools by wrong names — using names from other frameworks they were fine-tuned on, common shorthand, or slight misspellings. Without alias resolution, every mismatch is a hard failure that wastes a turn and forces a retry loop.

Observed examples:
- `Edit` instead of `foundry.edit_file` (Claude Code convention)
- `Bash` instead of `foundry.execute_command`
- `grep` instead of `forge.digest`
- `smell` instead of `forge.smell_detect`

Foundry's BrokerActor needs a canonical alias map that Forge provides, so tool name mismatches resolve before failing.

## Solution

### 1. `frg tool-aliases` subcommand

New CLI subcommand that outputs the alias map as structured JSON:

```bash
frg tool-aliases --format json
```

Returns:
```json
{
  "version": 1,
  "canonical_tools": [
    "forge.digest",
    "forge.outline",
    "forge.smell_detect",
    ...
  ],
  "aliases": {
    "Edit": "foundry.edit_file",
    "Write": "foundry.write_file",
    "Read": "foundry.read_file",
    "Glob": "foundry.search_files",
    "Grep": "foundry.search_files",
    "Bash": "foundry.execute_command",
    "LS": "foundry.list_directory",
    "list_dir": "foundry.list_directory",
    "search": "foundry.search_files",
    "edit": "foundry.edit_file",
    "write": "foundry.write_file",
    "read": "foundry.read_file",
    "run": "foundry.execute_command",
    "execute": "foundry.execute_command",
    "digest": "forge.digest",
    "outline": "forge.outline",
    "smell": "forge.smell_detect",
    "deps": "forge.dep_tree",
    "dsm": "forge.dsm_analyze",
    "todo": "forge.todo_extract",
    "audit": "forge.deps_audit",
    "secrets": "forge.secret_scan",
    "threats": "forge.threat_scan"
  },
  "fuzzy_suggestions": {
    "edt": "foundry.edit_file",
    "wrte": "foundry.write_file",
    "globb": "foundry.search_files"
  }
}
```

The `fuzzy_suggestions` map contains entries where Levenshtein distance to the canonical tool name is ≤ 3. These are **suggestions only** — Foundry never auto-resolves fuzzy matches.

### 2. `forge-aliases.toml` config file

Project-specific alias overrides. Found at:
- `<project-root>/forge-aliases.toml`
- `~/.config/forge/aliases.toml` (global)

Format:
```toml
# Project-specific tool aliases
# Merged on top of built-in aliases

[[alias]]
from = "my_edit"
to = "foundry.edit_file"

[[alias]]
from = "run_test"
to = "foundry.execute_command"
```

`frg tool-aliases` merges built-in + global + project-local aliases, with later entries overriding earlier ones.

### 3. Resolution rules (Foundry-side)

Foundry's BrokerActor applies these rules in order:

1. **Exact canonical match**: `foundry.edit_file` → use directly, skip alias lookup
2. **Exact alias match**: `Edit` → resolves to `foundry.edit_file`
3. **Case-insensitive alias match**: `edit` → resolves to `foundry.edit_file`
4. **Prefix-stripped match**: `foundry_edit_file` → strip `foundry_` prefix → `edit_file` → match alias
5. **Fuzzy suggestion**: `edt` → not auto-resolved, but suggestion included in error response
6. **No match**: return `ToolValidationError` with `NotInAllowedValues` violation, list available tools in `expected` field

Every resolved alias is logged to the journal with `original_name`, `resolved_name`, `match_type`.

### 4. MCP integration

The alias map is also accessible via MCP tool:

```json
{
  "name": "tool_aliases",
  "description": "Returns the tool alias map for resolving common tool name mismatches",
  "inputSchema": { "type": "object", "properties": {} },
  "annotations": { "readOnly": true }
}
```

This allows agents to query the alias map at runtime without running a CLI command.

### 5. Hot reload

On `SIGHUP`, Forge re-reads `forge-aliases.toml` files and rebuilds the alias map. Foundry's BrokerActor re-calls `frg tool-aliases --format json` on `SIGHUP` to pick up changes.

## Implementation Notes

**Completed:**
- `frg tool-aliases` CLI subcommand with `--format json` (default) and `--format table` options
- Built-in alias map in `crates/cli/src/aliases_builtin.toml` with 24 aliases
- Config file merging: built-in + `~/.config/forge/aliases.toml` + `<project>/forge-aliases.toml`
- Fuzzy suggestions generation (Levenshtein-based typo detection)
- MCP `tool_aliases` tool (readOnly: true) returning same data as CLI
- Human-readable table format output
- 8 integration tests covering JSON output, table format, config merging, and MCP registration

**Files created:**
- `crates/cli/src/aliases.rs` — alias map logic, config loading, fuzzy suggestions
- `crates/cli/src/aliases_builtin.toml` — compiled-in default aliases
- `crates/cli/tests/tool_aliases.rs` — integration tests
- `crates/mcp-server/src/lib.rs` — added `ToolAnnotations` struct for MCP metadata

**Files modified:**
- `crates/cli/src/main.rs` — added `ToolAliases` command and MCP tool registration
- `crates/mcp-server/src/lib.rs` — added `ToolAnnotations` for readOnly flag

## Verification

- [x] `frg tool-aliases --format json` returns valid JSON with all 24 built-in aliases
- [x] `Edit` resolves to `foundry.edit_file` (exact alias)
- [x] `edit` resolves to `foundry.edit_file` (case-insensitive)
- [x] Project-local `forge-aliases.toml` overrides are merged and visible in output
- [x] `frg tool-aliases` without `--format json` prints human-readable table
- [x] MCP `tool_aliases` tool returns same data as CLI
- [ ] SIGHUP reloads alias map from disk (deferred — requires signal handler)

## References

### Files to modify in Forge

- `crates/cli/src/main.rs` — add `ToolAliases` subcommand
- `crates/mcp-server/src/lib.rs` — register `tool_aliases` MCP tool
- New: `crates/cli/src/aliases.rs` — alias map definition, Levenshtein fuzzy matching, config file loading
- New: `crates/cli/src/aliases_builtin.toml` — compiled-in default aliases (included via `include_str!`)

### Alias map source of truth

The built-in alias map lives in `crates/cli/src/aliases_builtin.toml` and is compiled into the binary via `include_str!`. At runtime, it's merged with:
1. `~/.config/forge/aliases.toml` (global user overrides)
2. `<project-root>/forge-aliases.toml` (project-local overrides)

## Verification

1. `frg tool-aliases --format json` returns valid JSON with all 24 built-in aliases
2. `Edit` resolves to `foundry.edit_file` (exact alias)
3. `edit` resolves to `foundry.edit_file` (case-insensitive)
4. Project-local `forge-aliases.toml` overrides are merged and visible in output
5. `frg tool-aliases` without `--format json` prints human-readable table
6. MCP `tool_aliases` tool returns same data as CLI
7. SIGHUP reloads alias map from disk

## References

- `../foundry/specs/tools-integration.md` §7 — Foundry's AliasResolver that consumes this data
- `../foundry/specs/todo/feat-forge-tool-aliases.md` — Foundry-side work item
- `specs/token-conservation.md` — Forge MCP tiering (aliases cross all tiers)