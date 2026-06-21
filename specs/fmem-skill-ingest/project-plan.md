# fmem-skill-ingest Project Plan

> Phase 6 of blueprint. Timeboxed sprint plan, prioritized by risk (threat-model + FMEA) and sequenced around the fmem Sprint 2 dependency.
> Updated 2026-04-16: added Sprint 1a (taxonomy) and expanded Sprint 0 with the `smart_ingest` + `create_edge` wrappers.
> **Locked 2026-04-16:** removed `smart_ingest` / `create_edge` wrappers (fmem auto-creates tags); added `ensure_parent_tag` + `verify_skill` wrappers (depend on new fmem spec); added Phase D verification as a hard exit gate.

## Dependencies and assumptions

- **External blocker:** fmem must ship `ingest_skill` MCP tool (fmem Sprint 2). The forge side can be built in parallel against a mock.
- **Internal prerequisite:** forge has no MCP *client* today (blueprint Finding 1). Sprint 0 builds it.
- **Transport choice:** stdio first, HTTP follow-up (architecture.md).
- **Single engineer.** Timebox estimates assume one primary contributor; parallelizable spots are called out.

## Sprint 0 — MCP client foundation (1–2 days)

Goal: a reusable `forge-fmem-client` crate with stdio transport, a typed `ingest_skill` wrapper, and a mock for testing.

| # | Work item | Links | Estimate |
|---|---|---|---|
| 0.1 | Scaffold `crates/fmem-client` with `transport`, `tools`, `error` modules | architecture.md, dsm.md | 2h |
| 0.2 | stdio transport: spawn `fmem --mcp`, JSON-RPC framing, request/response matching by id (FMEA F14) | rust-hazards P0-6 | 4h |
| 0.3 | `initialize` handshake with protocol version assert (FMEA F15) | dsm.md | 1h |
| 0.4 | Typed error enum (no `Other(String)` catch-all) | rust-hazards fail-loud | 1h |
| 0.5 | `tools::ingest_skill` typed wrapper matching fmem schema | skills-layer-design.md | 2h |
| 0.5a | `tools::ensure_parent_tag` typed wrapper (depends on new fmem tool) | fmem spec `skill-ingest-support.md` | 1h |
| 0.5b | `tools::verify_skill` typed wrapper (depends on new fmem tool) | fmem spec `skill-ingest-support.md` | 1h |
| 0.6 | In-memory mock transport for unit tests | FMEA F6, F12, F14, F18, F23 | 2h |
| 0.7 | Unit tests: transport id matching, timeout, broken pipe, schema rejection, all three tool round-trips | FMEA F12–F15, F23 | 3h |

**Exit criteria.** `cargo test -p forge-fmem-client` green; clippy/fmt clean; mock-based round-trip test passes.

## Sprint 1 — Skill parser + walker + hasher (2 days)

Goal: pure parsing + hashing logic, no network, fully unit-tested. Parallelizable with Sprint 0.

| # | Work item | Links | Estimate |
|---|---|---|---|
| 1.1 | `skill_ingest::walk` — `walkdir` with `follow_links(false)`, depth cap, deterministic sort (threat T4, FMEA F3, F17) | threat-model.md, rust-hazards P0-4, P1-2 | 2h |
| 1.2 | `skill_ingest::parse` — frontmatter (serde_yaml) + body; size cap; reject non-UTF-8 (FMEA F4, F5, F6, F8) | rust-hazards P0-2 | 4h |
| 1.3 | `skill_ingest::parse` — step-section heading forms; warn on empty `steps[]` (WI-FMEA-02, FMEA F9) | fmea.md | 2h |
| 1.4 | Supplementary-file resolution with path canonicalization (threat T3) | threat-model.md, rust-hazards P0-3 | 2h |
| 1.5 | `skill_ingest::hash` — SHA-256 over sorted (frontmatter ‖ body ‖ supplementary) (WI-FMEA-03, FMEA F11) | fmea.md | 2h |
| 1.6 | Pre-ingest collision detection: two paths, same `name` → exit 3 (WI-FMEA-01, FMEA F7) | fmea.md | 2h |
| 1.7 | Secret-scan gate: reject skills whose body matches `forge-secret-scan` (threat I1) | threat-model.md | 2h |
| 1.8 | Unit tests covering FMEA F4, F5, F6, F7, F8, F9, F11, F17 | fmea.md | 4h |

**Exit criteria.** Parser + walker + hasher deterministic and unit-tested; `cargo test -p forge-ingest skill_ingest::` green.

## Sprint 1a — Taxonomy pre-pass (0.75 day, slimmed)

Goal: the `skill_ingest::taxonomy` module walks top-level dirs, parses optional `tag-hierarchy.yaml`, and emits an ordered list of `(child, parent)` PARENT_TAG edges. Tag *creation* is delegated to fmem (via `ingest_skill` side-effect or `ensure_parent_tag`).

Depends on: Sprint 1 walker + parser (paths + YAML reuse).

| # | Work item | Links | Estimate |
|---|---|---|---|
| 1a.1 | `skill_ingest::taxonomy::walk_top_level` — enumerate first-level dirs; apply `normalize_tag` (lowercase + non-alphanum → `-`, collapse `-`) matching fmem | threat T7, T8; rust-hazards P2-6 | 1.5h |
| 1a.2 | Parse `tag-hierarchy.yaml` (safe loader, 64 KiB cap, ≤1000 edges) | threat T5, D4; rust-hazards P1-2 | 2h |
| 1a.3 | Cycle detection via DFS (fail-fast UX before round-trip; fmem also rejects cycles server-side) | threat T6; FMEA F25; rust-hazards P1-6 | 1.5h |
| 1a.4 | Orphan detection: every hierarchy node must appear in `walk_top_level` output or in a parsed skill's `tags:` | FMEA F26 | 1h |
| 1a.5 | Preflight tag collection: union top-level + every skill's `tags:` to compute the full tag set used in collision checks | FMEA F27 | 1h |
| 1a.6 | Skill-name / tag-name collision check | FMEA F29 | 1h |
| 1a.7 | "Hierarchy absent" info log | FMEA F24 | 30m |
| 1a.8 | Unit tests covering FMEA F24–F29 | fmea.md | 2.5h |

**Exit criteria.** `skill_ingest::taxonomy::plan(root) -> Result<TaxonomyPlan, TaxonomyError>` returns an ordered, validated plan of (child, parent) PARENT_TAG edges; all unit tests green.

## Sprint 2 — Orchestrator + CLI + MCP tool (1.5 days)

Goal: wire parser + hasher + client into a runnable command with the flag surface from the feature spec.

| # | Work item | Links | Estimate |
|---|---|---|---|
| 2.1 | `skill_ingest::mod` four-phase orchestrator (A taxonomy → B skill ingest → C re-pass for missing edges → D verify); per-phase bucketing in summary | dsm.md, architecture.md data flow, rust-hazards fail-loud | 4h |
| 2.2 | Phase A wiring: call `ensure_parent_tag(child, parent)` per planned PARENT_TAG edge; exit 2 on first transport error | fmea.md, project-plan 1a | 1.5h |
| 2.2b | Phase B wiring: per skill, `ingest_skill` with category + tags + prerequisites — fmem creates tags/TAGGED_AS automatically and skips REQUIRES with missing prereqs | architecture.md data flow | 2h |
| 2.2c | Phase C wiring: re-issue `ingest_skill` for any skill whose response indicated skipped REQUIRES; bound retries at 2 (FMEA F16, WI-FMEA-04) | fmea.md | 2h |
| 2.2d | Phase D wiring: per parsed skill, `verify_skill(name)` — assert zero `missing_prerequisites`, expected tags present; exit 4 on any verification failure with named skill + missing edges | architecture.md Phase D, overview Finding 6 | 3h |
| 2.3 | `Commands::FmemSkillIngest` clap variant + registration in `crates/cli/src/main.rs` | existing paper ingest pattern | 2h |
| 2.4 | MCP tool registration (tier 1) | forge mcp-server | 1h |
| 2.5 | Flag wiring: `--root`, `--filter`, `--dry-run`, `--session`, `--force`, `--server`, `--verbose` | feature spec | 2h |
| 2.6 | Exit-code map: 0/1/2/3 per rust-hazards | rust-hazards.md | 1h |
| 2.7 | `--dry-run` compile-time-separable from real send (FMEA F18) | fmea.md | 1h |
| 2.8 | Integration test against mock transport: 3-skill fixture, end-to-end summary asserts | FMEA F7, F16, F18, F21 | 3h |

**Exit criteria.** `frg fmem-skill-ingest --dry-run --root tests/fixtures/skills-small` prints expected summary; all integration tests green.

## Sprint 3 — Live fmem integration + docs (1 day)

Goal: verify against a real fmem Sprint 2 build; update architecture docs; close the loop on acceptance criteria.

| # | Work item | Links | Estimate |
|---|---|---|---|
| 3.1 | E2E test against running `fmem --mcp`; run against `research/skills/` | feature-spec acceptance criteria | 2h |
| 3.2 | Verify all 7 acceptance criteria | feature-spec | 2h |
| 3.3 | Update `../components.md` with `crates/fmem-client` + ingest responsibility bullet | architecture.md | 30m |
| 3.4 | Update `../data-flow.md` with "Skill Ingestion Flow" sequence diagram | architecture.md | 30m |
| 3.5 | Add `cargo deny check advisories` to CI | rust-hazards CI guardrails | 30m |
| 3.6 | `--verbose` diff-aware truncation (FMEA F20) | fmea.md | 1h |
| 3.7 | Move this spec from `todo/` to `implemented/`; create a `verified/` entry after E2E passes | work-item pipeline | 15m |

**Exit criteria.** E2E with real fmem passes all acceptance criteria; docs updated; spec moved to `implemented/`.

## Critical path

```
Sprint 0 (MCP client) ──┐
                        ├──> Sprint 1a (taxonomy) ──> Sprint 2 (orchestrator) ──> Sprint 3 (live)
Sprint 1 (parser)  ─────┘                            ^
                                                     │
                                                     └──── (Sprint 1a also feeds here)
```

Sprints 0 and 1 are independent and can run in parallel if a second contributor is available. Sprint 1a depends on Sprint 1 (reuses walker + parser). Sprint 2 needs Sprint 0, Sprint 1, and Sprint 1a. Sprint 3 needs fmem Sprint 2 shipped.

## Risk-ordered work item list (aggregated)

From threat model (≥15) and FMEA (≥50, plus elevated):

1. **WI-THREAT-01** (T4, risk 15) — walker skips symlinks → Sprint 1 (1.1)
2. **WI-THREAT-02** (I1, risk 16) — secret-scan gate → Sprint 1 (1.7)
3. **WI-THREAT-03** (T5, risk 12) — tag-hierarchy YAML bomb → Sprint 1a (1a.2)
4. **WI-THREAT-04** (T6, risk 9) — hierarchy cycle detection → Sprint 1a (1a.3)
5. **WI-FMEA-01** (F7) — skill name collision → Sprint 1 (1.6)
6. **WI-FMEA-04** (F16) — two-pass skill edges → Sprint 2 (2.2b)
7. **WI-FMEA-03** (F11) — supplementary in hash → Sprint 1 (1.5)
8. **WI-FMEA-02** (F9) — step-heading handling → Sprint 1 (1.3)
9. **WI-FMEA-05** (F24) — hierarchy absent info log → Sprint 1a (1a.7)
10. **WI-FMEA-06** (F26) — orphan node detection → Sprint 1a (1a.4)
11. **WI-FMEA-07** (F27) — preflight tag collection → Sprint 1a (1a.5)
12. **WI-FMEA-08** (F28) — tag case normalization → Sprint 1a (1a.1)
13. **WI-FMEA-09** (F29) — skill/tag name collision → Sprint 1a (1a.6)

Remaining FMEA/threat items of lower priority are covered as test cases within the owning sprint (see `fmea.md` test-case list).

## Total timebox (locked)

- Sprint 0: 1.5 days (15h — three slim wrappers: ingest_skill, ensure_parent_tag, verify_skill)
- Sprint 1: 2.5 days (20h)
- Sprint 1a: 0.75 day (11h — slimmed; no client-side tag creation)
- Sprint 2: 2 days (16.5h — four-phase orchestrator with verify gate)
- Sprint 3: 1 day (8h)
- **Total: ~7.75 engineering days serial, ~6 days parallel**

External dependency: fmem `skill-ingest-support.md` (~1 day on the fmem side) blocks Sprint 0.5a/0.5b and Sprint 2 Phase A/D wiring. forge can build Sprints 1, 1a, and the rest of Sprint 0 against a mock that pretends the new fmem tools exist.

## Out-of-plan

- HTTP transport (stdio ships first; HTTP is a follow-up issue)
- Concurrent requests to fmem (single in-flight for v1 — see rust-hazards P1-5)
- Auto-invocation on skill file change (explicit non-goal per feature spec)
- Skill-version history (deferred per fmem design doc)
