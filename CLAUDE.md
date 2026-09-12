# CLAUDE.md — Forge

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

## What this repo is

Forge is the standalone home of `frg`: a Rust CLI and MCP server that helps AI
coding agents conserve context by converting verbose tool output into compact
structured JSON.

## Common commands

```bash
cargo build --workspace
cargo test --workspace
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo run -p forge -- --help
cargo run -p forge -- project-detect .
```

The binary is named `frg`:

```bash
cargo build --release -p forge
./target/release/frg version
```

## Conventions

- CLI/MCP commands should emit structured JSON unless documented otherwise.
- Prefer small crates with focused parser/adapter responsibilities.
- Keep public docs honest: Forge is a developer-preview tool, not a polished
  production platform.
- Use fmem/Ferrosa integrations as optional integrations; core build and unit
  tests should remain useful without private infrastructure.
