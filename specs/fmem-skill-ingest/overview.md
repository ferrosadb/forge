# fmem-skill-ingest Blueprint — Overview

> Status: in-process (blueprint locked, ready for dispatch)
> Created: 2026-04-16
> Updated: 2026-04-16 (spec added taxonomy seed + per-skill tag resolution)
> Locked: 2026-04-16 (post-fmem-Sprint-2 — open questions resolved, scope finalized)
> Feature spec: `../todo/fmem-skill-ingest.md`
> Consumer: `ferrosa-memory/specs/skills-layer-design.md`

## Feature summary

Add `frg fmem-skill-ingest` — a new forge subcommand that walks `research/skills/**/SKILL.md` (≈78 files across `task-level/`, `tech/`, `quality/`, etc.) and ingests each skill into ferrosa-memory via its `ingest_skill` MCP tool. Idempotent via SHA-256 `content_hash`; re-runnable after every skill edit.

## Why it matters

The research repo is the source of truth for the skill catalog (markdown, version-controlled, human-editable). ferrosa-memory needs those same skills as first-class typed entities so that `retrieve_skills_for_context` and the `Skill` intention trigger can surface them to the LLM at the right moment. Today there is no bridge; skill ingestion is manual via generic `smart_ingest` calls.

## Scope

In:
- New subcommand `frg fmem-skill-ingest` (CLI + MCP tool registration)
- Markdown/YAML parser for `SKILL.md` frontmatter and body
- **Taxonomy seed pass** — walk `--root` first-level dirs and ingest each as a `tag` entity via fmem `smart_ingest`; optional `tag-hierarchy.yaml` builds `PARENT_TAG` edges
- **Per-skill tag resolution** — category dir → primary tag; frontmatter `tags:` list → additional tags; passed to `ingest_skill` which creates `TAGGED_AS` edges
- `content_hash` computation + idempotent upsert path (for both skills and tags)
- CLI flags: `--root`, `--filter`, `--dry-run`, `--session`, `--force`, `--server`, `--verbose`

Out (per spec):
- Skill execution (`invoke_skill` lives in fmem)
- Skill editing UI
- Pushing skill changes back to research (one-way: research → fmem)
- File-watcher / auto-invocation (separate work item)

## Critical blueprint findings

### Finding 1 — The MCP client wiring claimed in the spec does not exist

The feature spec says:

> **Auth:** existing forge → fmem MCP client wiring (already used by `frg ingest`).

This is incorrect. `frg ingest`, `frg ingest-url`, and `frg ingest-paper` all talk to ferrosa-memory via **direct CQL** through a Python `cassandra-driver` subprocess (see `crates/ingest/src/loader.rs`). There is no MCP client in the forge codebase today.

**Implication.** Building `fmem-skill-ingest` against MCP — as the spec requires — means introducing forge's first MCP client. That is a meaningful infrastructure addition (transport selection stdio-vs-HTTP, JSON-RPC framing, session/auth, error propagation) that was hidden behind a false premise in the spec.

**Decision needed (addressed in Phase 6 project plan):** either
- (A) Build the MCP client as part of this feature (adds Sprint 0 to the plan), or
- (B) Follow the existing CQL pattern and teach forge to speak the `ingest_skill` semantics directly against the `entity_store` table.

**Recommendation:** (A). Rationale: fmem's `ingest_skill` encapsulates skill-schema validation, edge creation for `REQUIRES` / `RELATED_TO`, and content-hash skip logic. Re-implementing that in forge duplicates domain logic and couples forge to the fmem CQL schema. The MCP client is a one-time cost that other future admin commands (`invoke_skill`, `retrieve_skills_for_context`) will reuse.

### Finding 2 — Hard dependency on fmem Sprint 2

`ingest_skill` is scheduled for fmem Sprint 2 of the skills-layer migration (see `ferrosa-memory/specs/skills-layer-design.md`). The feature spec already acknowledges this and proposes building against a mock server. The project plan (Phase 6) makes this explicit with a mock-first milestone that unblocks forge implementation in parallel.

### Finding 3 — Greenfield, well-specified

No prior implementation exists. The feature spec is thorough (interface, error handling, idempotency, acceptance criteria). Correspondingly, the highest-value blueprint artifacts are FMEA (Phase 4) and the compiled executable plan (Phase 10), not architecture discovery.

### Finding 4 — Taxonomy pre-pass widens the client surface (added 2026-04-16)

The updated spec requires a taxonomy seed step that calls `smart_ingest` with `entity_type="tag"` before any skill ingestion. This means the fmem client must expose *two* typed tool wrappers — `ingest_skill` and `smart_ingest` — not just one. The pre-pass is also state-bearing: if it fails partway, the catalog ends up with a subset of tags created, and a retry must be safe. The blueprint addresses this by making `smart_ingest(entity_type="tag", ...)` idempotent via content_hash and running the pre-pass inside the same two-pass orchestrator (entities first, edges second) already planned for `REQUIRES`/`RELATED_TO` edges. See the updated `architecture.md`, `dsm.md`, `fmea.md`, and `compiled-plan.md` for the changes this fan-out triggered.

### Finding 5 — fmem already does most of the heavy lifting (added 2026-04-16, post-fmem-Sprint-2)

After fmem's Sprint 2 shipped (`ingest_skill`, `retrieve_skills_for_context`, `invoke_skill`) plus Sprint 2d (DAG cycle prevention for REQUIRES + PARENT_TAG), most of the blueprint's defensive client-side work is server-side now:

- `ingest_skill` auto-creates tag entities + TAGGED_AS edges from `category` and `tags:` — forge does **not** need a separate `smart_ingest`-for-tags wrapper for skill ingestion. P5a is dropped.
- `ingest_skill` skips REQUIRES edges with missing prereqs silently — re-running fills them. The client-side three-phase orchestration is no longer required for correctness; a single-pass + re-pass for edges is sufficient.
- DAG cycle prevention is server-side for both REQUIRES and PARENT_TAG. The blueprint's client-side cycle detection (P31) is kept as **fail-fast** (better error UX before the round-trip), not as the source of truth.
- `normalize_tag` is server-side. forge applies the **same** normalization (lowercase + non-alphanum → `-`) at parse time so client-side preflight collision/uniqueness checks compute the same buckets fmem will. Per-spec rule: `_` → `-`, lowercase, on the way in.

### Finding 6 — fmem still needs two new admin tools (added 2026-04-16)

For tag-hierarchy seeding and post-ingest verification, two gaps remain in fmem:

1. `ensure_parent_tag(child, parent)` — name-keyed PARENT_TAG edge creation. Without this, forge has to do `retrieve_entities` (phonetic) + client-side exact filter + `create_edge` per edge (3 round-trips, awkward).
2. `verify_skill(name)` — return tags + prerequisites + required_by + missing_prerequisites for one skill so forge can confirm ingest landed every edge.

A spec for both is at `../../../../ferrosa-memory/specs/todo/skill-ingest-support.md`. Estimated ~1 day on the fmem side. forge's verification phase (D) blocks on these.

### Locked design choices (post-Q&A 2026-04-16)

1. **`tag-hierarchy.yaml` stays in scope.** Sprint 1a remains. fmem support tracked in the new fmem spec above.
2. **Verification is a hard exit gate.** Single-pass ingest + re-pass for edges + verification phase D. Ingest is not "complete" until `verify_skill` reports zero `missing_prerequisites` for every parsed skill and every declared tag relationship is present. Failed verification → exit 4.
3. **Tag normalization at parse time:** `_` → `-`, lowercase, collapse runs of `-` (matches fmem's `normalize_tag`).

## Artifact index

| File | Phase | Purpose |
|---|---|---|
| `overview.md` | 0 | This file |
| `architecture.md` | 1 | Component placement, data flow, MCP-client decision |
| `dsm.md` | 2 | Module structure + dependency direction |
| `threat-model.md` | 3 | STRIDE over parsing, path, MCP, hashes |
| `fmea.md` | 4 | Failure modes, RPN scoring, test cases |
| `rust-hazards.md` | 5 | Rust-specific correctness scan |
| `project-plan.md` | 6 | Sprint breakdown |
| `compiled-plan.md` | 10 | Agent-executable task packets + DAG |
