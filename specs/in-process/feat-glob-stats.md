# Forge Glob with Stats

## Problem

`frg digest` and `frg module_outline` both require a file path or directory, but they return **code structure**, not **what files exist**. Agents need to know which files are in a directory before they can call `excerpt`, `module_outline`, or `Read` on them.

Currently there are two paths:

1. **Bash `find`/`ls`** — raw output, requires post-processing to get sizes, ages, etc. Wastes tokens on parsing.
2. **`frg digest <dir>`** — returns structure but only for files it can enumerate from source-language conventions. It doesn't support arbitrary globs or filter by file type/size.

Agents end up doing two calls for a simple question:

```
Bash: find src/ -name "*.rs" -type f | head  # what files exist?
Read: src/file_a.rs                            # now read one to decide if it's relevant
```

This is token-inefficient and slow.

## Solution

A new `frg glob` subcommand that finds files matching patterns and returns structured stats.

### 1. CLI

```bash
# Simple glob
frg glob "src/**/*.rs"

# Filter by size range (skip tiny tests files and massive generated files)
frg glob "src/**/*.rs" --min-lines 20 --max-lines 5000

# Filter by age (recently modified only)
frg glob "src/**/*.rs" --modified-after 7d

# Output formats
frg glob "src/**/*.rs" --format json
frg glob "src/**/*.rs" --format csv
frg glob "src/**/*.rs" --format table
frg glob "src/**/*.rs" --format brief  # just paths, one per line (default)
```

### 2. JSON output

```json
{
  "pattern": "src/**/*.rs",
  "results": [
    {
      "path": "src/ferrosa-cql/src/router.rs",
      "lines": 11677,
      "bytes": 412048,
      "modified": "2026-04-15T10:23:00Z",
      "is_generated": false
    },
    ...
  ],
  "total_matched": 124,
  "total_skipped": 3
}
```

### 3. Filter options

| Flag | Purpose | Default |
|------|---------|---------|
| `--min-lines N` | Skip files with fewer than N lines | 0 |
| `--max-lines N` | Skip files with more than N lines (skip generated/large binaries) | 100000 |
| `--min-bytes N` | Skip files smaller than N bytes | 0 |
| `--max-bytes N` | Skip files larger than N bytes | 1GB |
| `--modified-after <N>d` | Only files modified in last N days | 0 (all) |
| `--modified-before <N>d` | Only files older than N days | 0 (all) |
| `--exclude <pattern>` | Glob pattern to exclude (repeatable) | `.git/`, `target/`, `vendor/`, `node_modules/`, `*.lock` |
| `--format <fmt>` | Output format | `brief` |

### 4. Use cases

- **Before `frg digest`**: `frg glob "src/**/*.rs" --max-lines 5000 --format brief` tells the agent which files are reasonable to digest.
- **Before `frg smell_detect`**: `frg glob "*.rs" --min-lines 100 --max-lines 5000` narrows to files worth analyzing (skips tiny helpers and massive generated code).
- **Before `frg dsm`**: `frg glob "src/{ferrosa-cql,ferrosa-storage}/**/*.rs" --format csv` feeds into dependency analysis.
- **Before refactoring**: `frg glob "src/**/*.rs" --format table --modified-after 30d` shows recently-changed files to prioritize which module to audit first.

### 5. Implementation

- Use Rust's `glob` crate with the same ignore rules as existing forge tools (`.gitignore`, `.ignore`).
- Stats collected via `std::fs::metadata()` + `read_line()` count (fast, no need to parse content).
- `--lines` count is approximate (counts newline-terminated lines) — sufficient for filtering.
- Respect `--max-lines` before reading — don't count lines of a 200KB file if we'll skip it anyway.

## Implementation Notes

**Files to create:**
- `crates/cli/src/glob.rs` — glob logic, stat collection, filter application
- `crates/cli/src/glob_builtin.toml` — default exclude patterns

**Files to modify:**
- `crates/cli/src/main.rs` — add `GlobStats` subcommand
- `crates/mcp-server/src/lib.rs` — register `glob` MCP tool (readOnly: true, returns JSON)

## Verification

- [ ] `frg glob "src/**/*.rs"` returns paths with line counts
- [ ] `--min-lines` / `--max-lines` correctly filter results
- [ ] `--format json` returns valid JSON with full metadata
- [ ] `--format table` produces human-readable output
- [ ] `--format brief` returns just paths (default)
- [ ] `.gitignore` and `target/` are excluded by default
- [ ] MCP `glob` tool returns same data as CLI
- [ ] Large files (>10MB) are skipped without counting lines

## References

- Existing `frg digest` — already computes line counts during digestion; `glob` should be lightweight enough to run standalone
- `find(1)` — the Unix tool this generalizes with structured output
