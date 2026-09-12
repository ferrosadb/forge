# Agent Rules

## STOP: memory-public is NOT TO BE USED

**`ferrosadb/ferrosa-memory` (memory-public) is old and must never be used.** <!-- memory-public-ok: the rule itself -->
Use **`ferrosadb/ferrosa-memory-private`** for every build, clone, CI checkout,
version pin, submodule, release download and doc link.

The public repo is a filtered mirror of private. They share tag names but not
code: on 2026-09-11 the mirror was at schema v64 and private at v66. Anything
built, cloned, pinned or downloaded from the mirror silently downgrades schemas
and breaks installs. Its release workflows are disabled, so its releases are
stale forever.

Check this repo with `scripts/guard-no-memory-public.sh` (CI runs it). A line
that must name the public repo, such as this rule's own text, carries the
marker `memory-public-ok: <reason>`.

## Project Purpose

Forge is a Rust workspace that builds `frg`, a token-efficient CLI and MCP server
for AI coding agents. It wraps noisy developer workflows and emits compact,
structured JSON that agents can act on without reading full raw logs.

## Development Rules

- Keep outputs deterministic and machine-readable by default. Human prose belongs
  in hints, docs, or explicit text modes.
- Fail loud when a wrapper cannot produce trustworthy structured output. Do not
  return fake success or empty summaries for failed underlying commands.
- Preserve bounded output. New commands must cap or summarize unbounded logs,
  diffs, and file scans.
- Sanitize secrets in diagnostics. Never print raw tokens, private keys, or
  credentials in errors or test snapshots.
- Add regression tests for parser changes and bug fixes. Golden/sample outputs
  are encouraged when they make behavior clear.

## Verification

Before opening a PR or release:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo build --workspace --all-targets
```

If a check is skipped because an external tool is missing, state that explicitly
and run the narrowest available fallback.
