# Feat: api_contract_diff — Detect breaking API changes between two revisions

**Priority:** High
**Component:** new crate `forge-api-diff`, CLI subcommand `api-diff`, MCP tool `api_contract_diff`

## Goal

Give the `semver` skill (and code reviewers) a deterministic answer to "does this diff contain breaking API changes?" Currently the skill reasons about signatures by hand. This tool extracts the public API surface from two git revisions and reports added/removed/changed symbols.

## Input

- `--base`: base git ref (default: `main`)
- `--head`: head git ref (default: `HEAD`)
- `--path`: scope to a directory
- `--lang`: force a language (default: auto-detect via `forge-project-detect`)

## Supported languages (v1)

- **Rust**: `pub fn`, `pub struct`, `pub enum`, `pub trait`, `pub type`. Field visibility inside structs counts.
- **TypeScript**: `export function`, `export class`, `export interface`, `export type`, `export const`.
- **Python**: top-level `def` / `class` without leading underscore, `__all__` contents if defined.
- **Go**: uppercase top-level identifiers (exported per Go convention).

## Classification

| Change | Severity | Example |
|---|---|---|
| Symbol removed | **Breaking** | `pub fn parse` deleted |
| Function signature changed (param types, return type, arity) | **Breaking** | `fn parse(s: &str)` → `fn parse(s: String)` |
| Trait/interface method added (non-default) | **Breaking** | `trait Store { fn read(); }` gains `fn write();` |
| Struct field removed or changed type | **Breaking** | — |
| Symbol added | **Minor** | — |
| Struct field added (with default) | **Minor** | — |
| Doc-comment only change | **Patch** | — |

## Output

```json
{
  "base": "main",
  "head": "feature/new-api",
  "changes": [
    {"kind": "removed", "symbol": "parse_v1", "file": "src/lib.rs", "severity": "breaking"},
    {"kind": "signature_changed", "symbol": "connect", "before": "fn connect(url: &str) -> Result<_>", "after": "fn connect(url: &str, timeout: Duration) -> Result<_>", "severity": "breaking"},
    {"kind": "added", "symbol": "parse_v2", "severity": "minor"}
  ],
  "suggested_bump": "major",
  "summary": {"breaking": 2, "minor": 1, "patch": 0}
}
```

`suggested_bump`: major if any breaking; minor if any added; patch otherwise.

## Implementation notes

- Use `forge-outline` to extract public symbols from both revs (it already parses Rust/TS/Python/Go).
- Git diff via `git show <ref>:<path>` — no git crate dependency required.
- For v1: signature diff is text-based (compare outlined arg strings). Does not understand type equivalence (`&str` vs `&'a str`).

## Dependencies

- `forge-outline`, `forge-project-detect`, `forge-shared`, `regex`, `anyhow`.

## Test plan

- Fixtures: pairs of `before.rs` / `after.rs` exercising each change kind.
- Git-ref path: temp repo with two commits, verify diff.
- Suggested-bump logic: unit-tested.

## Out of scope (v1)

- Type-equivalence-aware diff (requires full type resolution).
- Semver lint rules for cross-crate re-exports.
- Protobuf/JSON schema diff (those are `schema_diff`'s job).
