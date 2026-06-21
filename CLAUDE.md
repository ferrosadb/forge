# CLAUDE.md — Forge

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
