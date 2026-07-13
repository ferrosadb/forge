# Forge Specs

Architecture and implementation specs for the Forge Rust CLI workspace.

## Architecture

- [overview.md](overview.md) — workspace purpose, crate topology, runtime modes
- [components.md](components.md) — component responsibilities and boundaries by crate
- [data-flow.md](data-flow.md) — CLI, MCP, hook, analytics, and ingestion flows

## Feature Specs

- [anti-loop-attempt-control.md](anti-loop-attempt-control.md) — bounded attempt control, waiting gates, scoring, and loop detection
- [token-conservation.md](token-conservation.md) — progressive disclosure and MCP tool tiering
- [paper-ingestion.md](paper-ingestion.md) — academic paper ingestion design

## Architecture Decisions

- [ADR-001: Operational attempt state and memory ownership](decisions/001-operational-attempt-state-and-memory.md) — separates checklist state, Ferrosa memory, raw output, and outage fallback

## Work Item Pipeline

- [todo/](todo/) — planned work
- [in-process/](in-process/) — active work
- [implemented/](implemented/) — implemented, awaiting verification
- [verified/](verified/) — verified work
