# Forge Specs

Architecture, decision, and historical implementation specs for the Forge Rust
CLI workspace. The checked-in code and CLI help are the source of truth for
shipped behavior; historical feature plans remain here for rationale and risk
traceability.

## Architecture

- [overview.md](overview.md) — workspace purpose, crate topology, runtime modes
- [components.md](components.md) — component responsibilities and boundaries by crate
- [data-flow.md](data-flow.md) — CLI, MCP, hook, analytics, and ingestion flows
- [token-conservation.md](token-conservation.md) — current MCP visibility and progressive-disclosure model

## Feature Specs

- [anti-loop-attempt-control.md](anti-loop-attempt-control.md) — bounded attempt control, waiting gates, scoring, and loop detection
- [paper-ingestion.md](paper-ingestion.md) — academic paper ingestion design

## Architecture Decisions

- [ADR-001: Operational attempt state and memory ownership](decisions/001-operational-attempt-state-and-memory.md) — separates checklist state, Ferrosa memory, raw output, and outage fallback

## Work Item Pipeline

- [todo/](todo/) — planned work; some older entries have shipped and are marked in-file until their historical records are reorganized
- [in-process/](in-process/) — active or externally gated work
- [implemented/](implemented/) — implemented feature records
- [verified/](verified/) — independently verified work (created when needed)
