# feat-ingest-function-descriptions — Overview

> Status: Blueprint
> Source spec: `../todo/feat-ingest-function-descriptions.md`
> Parent pipeline: extends `fmem-skill-ingest` (in-process)

## Purpose

Add a description-extraction pass to `frg ingest` that stores 1–2 sentence descriptions of public code entities as temporal facts in ferrosa-memory. This lets `hybrid_search` answer "what does X do?" without reading source files.

## Value

- Cuts description-class queries from 3-call (`search → read → summarize`) to 1-call (`search`)
- Descriptions travel with the graph; benefit compounds across every future retrieval
- Enables downstream consumers (skills, agents, UI) to surface explanations without an LLM call at query time

## Scope

**In scope**

- New Pass 2 in the ingest pipeline: entity enrichment via LLM call on doc comment + first 10 lines of body
- Local-model-first provider abstraction (`local | openai | anthropic | skip`) with pre-flight availability probe and interactive prompt on failure
- Config surface in forge config TOML under `[ingest.descriptions]`
- Temporal-fact write via existing `write_temporal_fact` API
- Quality filter: skip trivial delegates, private entities (by default), descriptions below `min_confidence`
- Backward compat: `enabled = false` or `provider = "skip"` → behaves exactly as today

**Out of scope**

- Description regeneration on source change (stale-description handling is a followup)
- Multi-sentence / richer doc synthesis
- Description search UX changes (consumer of the fact, not producer)
- Cross-entity semantic linking (`explore_connections` already exists)

## Provider model

```text
                   ┌─────────────────────────────────────┐
                   │  ingest.descriptions.provider       │
                   └─────────────────────────────────────┘
                        │
        ┌───────────────┼──────────────────┬───────────────┐
        ▼               ▼                  ▼               ▼
    ┌───────┐       ┌────────┐        ┌─────────┐      ┌──────┐
    │ local │       │ openai │        │anthropic│      │ skip │
    └───┬───┘       └────┬───┘        └────┬────┘      └──┬───┘
        │ probe          │ env var         │ env var       │
        │ /api/tags      │ OPENAI_API_KEY  │ ANTHROPIC_*   │ no-op
        ▼                ▼                 ▼
      prompt-on-failure escalates to: switch provider / different model / skip / abort
```

Availability is checked **once at ingest start**, before Pass 2 begins. Failure surfaces loudly — never silent degradation (per global fail-loud rule).

## Interface contract

Config (forge's main config TOML, typically `forge.toml` or workspace-resolved):

```toml
[ingest.descriptions]
enabled         = true
provider        = "local"            # local|openai|anthropic|skip
local_model     = "qwen2.5-coder:7b"
local_endpoint  = "http://localhost:11434"
local_timeout_ms = 5000
remote_model    = "claude-haiku-4-5"
max_words       = 60
include_private = false
min_confidence  = 0.7
```

CLI flags (override config for one run):

```text
--desc-provider <local|openai|anthropic|skip>
--desc-model <name>
--desc-endpoint <url>
--non-interactive              # disables the prompt; missing model = error
```

Temporal-fact shape written per enriched entity:

```json
{
  "entity_id": "concept:ferrosa-cql::term_to_cql_value",
  "fact_type": "description",
  "value": "Converts a CQL parse tree Term node into a runtime CellValue.",
  "provenance": { "provider": "local", "model": "qwen2.5-coder:7b", "confidence": 0.82 },
  "extracted_at": "2026-04-17T11:22:00Z"
}
```

## Constraints

- Pass 2 must be **idempotent within a run** (same input → same output; do not retry on transient success)
- Pass 2 must be **skippable** without affecting Pass 1 correctness
- Must degrade loudly: non-interactive mode + unavailable provider = exit nonzero with remediation guidance
- Concurrency cap on LLM calls: default 4 in-flight, configurable via `ingest.descriptions.concurrency` — prevents local Ollama saturation
- Cost cap: hard limit of N calls per run (default 5000), surfaced as `--max-desc-calls`; exceeded → loud warning + partial result

## References

- Parent: `specs/fmem-skill-ingest/` — the base ingest pipeline this extends
- Sibling: `feat-glob-stats` — discovery layer that typically precedes ingest
- Upstream dep: ferrosa-memory `write_temporal_fact` (already exposed)
