# Forge Ingest: Function Descriptions in Graph

## Problem

`frg ingest` extracts entities and edges into the ferrosa-memory knowledge graph, but it only stores **structural** facts:

- Entities: documents, modules, functions, structs, etc.
- Edges: `contains`, `depends_on`, `calls`, `references`

When an agent asks "what does `apply_command` do?" or "how does `poll_compactions` work?", the graph can tell you **where it is** and **what calls it**, but not **what it does**. The agent must read the full file or call `frg excerpt` — both wasteful when the agent just needs a one-sentence summary.

Current retrieval flow for a simple question:

```
Agent: "What does the CQL bridge do?"
hybrid_search → finds bridge.rs entity
→ No description, only structure
→ Must call Read or excerpt on the file
→ Wastes tokens reading 2800 lines to get a 15-word answer
```

## Solution

Enhance `frg ingest` to store **one-line descriptions** for public entities in the knowledge graph. These descriptions are extracted during ingestion by analyzing the entity's documentation comment + first few lines of the function body.

### 1. Entity enrichment

On ingest, for each entity with `entity_type="concept"` (and any other types with documentation):

- Extract a **1-2 sentence description** from `///` docs, doc comments, or the function's first statement + name
- Store it as a **temporal fact** on the entity: `write_temporal_fact(entity_id, description)`
- This makes it queryable via `hybrid_search` and `retrieve_entities` without reading any source files

Example graph after enhanced ingest of `ferrosa-cql/src/bridge.rs`:

```
(concept:term_to_cql_value) --HAS_DESCRIPTION--> (fact: "Converts a CQL parse tree Term node into a runtime CellValue")
(concept:parse_type)       --HAS_DESCRIPTION--> (fact: "Parses a CQL type reference into a Type enum, handling parameterized collections and frozen types")
(concept:cql_value_to_json) --HAS_DESCRIPTION--> (fact: "Serializes a CellValue to JSON, recursing into collections and maps")
```

### 2. Description quality criteria

Descriptions must follow these rules to avoid noise:

- **1-2 sentences, ≤ 60 words** — short enough to scan, long enough to be useful
- **No code snippets** — describe behavior, don't show it
- **Present tense, active voice** — "Converts X to Y" not "This function is used to convert"
- **Include what and why** — not just the name repeated: "Parses X" is bad; "Parses X into Y, handling Z edge cases" is good
- **Skip trivial delegations** — `fn parse_x() { parse_y() }` is not worth storing

### 3. Two-pass ingest for accuracy

```
Pass 1 (fast): Extract all entities + edges via existing logic
Pass 2 (slow): For each entity with documentation or a public name,
               extract a one-line description using an LLM call
               on the doc comment + first 10 lines of body
Pass 3: Write descriptions as temporal facts
```

Pass 1 is cached/idempotent (same entities/edges). Pass 2 is the new cost — one LLM call per public entity instead of per file. Expected overhead: ~200ms per 10 public functions.

### 4. Config options

```toml
[ingest.descriptions]
enabled = true

# Provider: "local" | "openai" | "anthropic" | "skip"
# "local" targets a locally-served OpenAI-compatible endpoint
# (Ollama, LM Studio, llama.cpp server). "skip" disables description
# extraction for this run (backward compatible with enabled = false).
provider = "local"

# Applies when provider = "local"
local_model     = "qwen2.5-coder:7b"
local_endpoint  = "http://localhost:11434"   # Ollama default; LM Studio uses :1234
local_timeout_ms = 5000

# Applies when provider = "openai" | "anthropic"
# API keys come from env (OPENAI_API_KEY, ANTHROPIC_API_KEY), never config.
remote_model = "claude-haiku-4-5"

# Extraction constraints
max_words       = 60
include_private = false
min_confidence  = 0.7
```

#### Provider availability check (runs before Pass 2)

When `provider = "local"`, forge probes the endpoint once at ingest start:

1. `GET {local_endpoint}/api/tags` (Ollama) or `/v1/models` (OpenAI-compatible) — 2s timeout
2. If endpoint unreachable → prompt (see below)
3. If endpoint reachable but `local_model` not in the returned list → prompt

When `provider ∈ {openai, anthropic}`, forge checks the corresponding env var; missing → prompt.

**Interactive prompt** (TTY attached):

```
Local model 'qwen2.5-coder:7b' not available at http://localhost:11434.
  [1] Choose a different local model (list from endpoint)
  [2] Switch provider (openai / anthropic)
  [3] Skip description extraction for this run
  [4] Abort ingest
>
```

The chosen value is used for this run only; persisting the choice requires editing config.

**Non-interactive mode** (no TTY, CI, `--non-interactive` flag): fail fast with the same message plus remediation hints — never silently fall back. This follows the fail-loud rule: degraded ingest must be an explicit user choice, not a default.

### 5. Usage impact

After this change, the retrieval flow becomes:

```
Agent: "What does the CQL bridge do?"
hybrid_search → finds bridge.rs entity with description
→ "Converts CQL parse tree values to and from JSON, handling type conversion between CQL binary and HTTP serialization layers"
→ Agent knows the answer in ONE tool call
→ Only calls Read/excerpt if deeper detail is needed
```

This is the same pattern the refactor skill already teaches for MCP-first exploration — structured data answers simple questions, raw files answer complex ones.

## Implementation Notes

**Files to modify:**
- `crates/forge/ingest.rs` (or equivalent) — add description extraction pass
- New: `crates/forge/ingest/descriptions.rs` — LLM-based description extraction from doc comments
- `crates/forge/ingest/schema.rs` — new fact type or extend existing entity schema

**LLM call pattern:**
```
Input: entity name + doc comment (if any) + first 10 lines of body (if any)
Prompt: "Write a one-sentence description of what this function/entity does.
         Use active voice, present tense. Max 60 words. No code snippets."
Output: description string
```

## Verification

- [ ] Ingesting a crate produces descriptive facts for public functions
- [ ] `hybrid_search` on a function name returns its description
- [ ] `retrieve_entities` includes description in results
- [ ] Private functions are skipped by default (`include_private = false`)
- [ ] Trivial delegates (single-expression functions) are skipped
- [ ] Descriptions are queryable: "how does X work?" → returns description match
- [ ] Ingest with `enabled = false` or `provider = "skip"` skips description extraction (backward compatible)
- [ ] Ingest time with descriptions is ≤ 3× baseline (reasonable overhead)
- [ ] Unreachable local endpoint triggers interactive prompt (TTY) or fail-loud error (non-interactive)
- [ ] Missing local model in endpoint's model list triggers the same prompt
- [ ] Missing API env var for remote provider triggers the same prompt
- [ ] User selection of "skip" in prompt completes Pass 1 only and exits 0

## References

- `feat-glob-stats.md` — sibling spec for file discovery
- `fmem-skill-ingest.md` — existing ingest pipeline that this extends
- `a local refactor skill catalog entry` — teaches MCP-first exploration; this spec enables that pattern for structural questions
