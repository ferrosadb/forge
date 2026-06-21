# fmem-skill-ingest FMEA

> Phase 4 of blueprint. Failure Mode and Effects Analysis with RPN (Severity × Occurrence × Detection), each scored 1–5.
> Action threshold: RPN ≥ 50 → generate a test case. RPN ≥ 100 → add a work item with `source: fmea`.
> Updated 2026-04-16: added F23–F29 covering taxonomy pre-pass and per-skill tag resolution.

## Legend

- **Severity (S):** 1 cosmetic · 2 minor · 3 data issue recoverable · 4 data loss / wrong data persists · 5 unrecoverable
- **Occurrence (O):** 1 almost never · 2 rare · 3 occasional · 4 likely on normal use · 5 very frequent
- **Detection (D):** 1 obvious at runtime · 3 requires grepping logs · 5 silent / only found downstream
- **RPN:** S × O × D

## Failure modes

| ID | Function | Failure mode | Effect | S | O | D | RPN | Mitigation / test |
|---|---|---|---|---|---|---|---|---|
| F1 | walk | Root path does not exist | Zero skills ingested, exit 0 (misleading success) | 3 | 2 | 4 | 24 | Refuse to run if `--root` is missing or not a dir; exit 2 |
| F2 | walk | Permission denied on a subdirectory | Subset of skills ingested; operator thinks run was complete | 4 | 2 | 4 | **32** | Count skipped files; exit 1 if any are skipped due to I/O error; test: chmod a skill dir 000 |
| F3 | walk | Symlink loop | Walker hangs or blows stack | 4 | 1 | 3 | 12 | `follow_links(false)` + depth cap (already in threat-model T4/D2) |
| F4 | parse | Missing frontmatter | Skill skipped, warn | 2 | 4 | 2 | 16 | Warn, skip, exit 1 at end; test: SKILL.md with no `---` block |
| F5 | parse | Frontmatter missing `name` or `description` | Skill skipped, warn | 2 | 3 | 2 | 12 | Warn, skip, exit 1 at end; test: each required field individually missing |
| F6 | parse | YAML syntax error | Skill skipped, warn | 3 | 3 | 2 | 18 | Report file + line/col, skip, exit 1 at end |
| F7 | parse | Duplicate skill names across categories | One of the two silently overwrites the other in fmem | 5 | 2 | 5 | **50** | Pre-scan detects collisions; refuse to run with "skill name collision" listing both paths; test: two SKILL.md files with same `name` in different dirs |
| F8 | parse | Extremely large SKILL.md | OOM | 4 | 1 | 3 | 12 | Per-file size cap (2 MiB) (threat-model D1) |
| F9 | parse | Steps section not recognized (unusual heading) | Skill ingested with empty `steps[]`, retrieval quality drops | 2 | 4 | 5 | **40** | Document the accepted heading forms; warn when `steps[]` is empty; test: parser fixture with `## How to Use` instead of `## Instructions` |
| F10 | hash | Hash collision across runs | Modified skill treated as unchanged; fmem stays stale | 4 | 1 | 5 | 20 | SHA-256 (threat-model E2); test: flip one byte, assert hash changes |
| F11 | hash | Forgot to include supplementary files in hash | Edit to a supplementary file never triggers update | 3 | 3 | 5 | **45** | Include supplementary files in hash input in defined order (sorted by path); test: edit a supplementary, confirm hash changes |
| F12 | fmem-client transport | stdio subprocess fmem crashes mid-run | Partial ingest, later skills never attempted | 4 | 2 | 3 | 24 | Detect broken pipe, emit summary showing how many were attempted vs skipped; exit 2; test: kill fmem mid-run |
| F13 | fmem-client transport | JSON-RPC timeout on a single skill | Skill reported failed, run continues | 3 | 3 | 2 | 18 | Per-call timeout (default 10s, configurable); count into failed bucket |
| F14 | fmem-client transport | Response id mismatch | Wrong response applied to wrong request | 5 | 1 | 5 | 25 | Strict id matching (threat-model E1); test: mock transport that shuffles ids |
| F15 | fmem-client tools | fmem schema version newer than client expects | Payload rejected by fmem; every skill fails | 3 | 2 | 1 | 6 | Protocol version assert in `initialize`; fail loud with upgrade hint |
| F16 | fmem-client tools | Prerequisite skill referenced before it's ingested | Edge not created; silent gap in graph | 3 | 4 | 5 | **60** | Two-pass mode: first pass creates entities, second pass creates edges; OR dependency order via topological sort of prerequisites; test: skill A requires B but A is walked first |
| F17 | orchestrator | Non-deterministic walk order causes non-reproducible runs | Diff noise between runs, no behavior change | 1 | 4 | 3 | 12 | Sort skill paths lexicographically before iterating; test: run twice, assert identical summary |
| F18 | orchestrator | `--dry-run` accidentally calls fmem | Data written despite operator intent | 5 | 1 | 3 | 15 | Dry-run branch must be compile-time separable from ingest branch (no shared `send` path with a boolean); test: dry-run with a mock that panics if called |
| F19 | orchestrator | Mid-run SIGINT leaves pipeline half-applied | Partial catalog state | 3 | 2 | 3 | 18 | Ctrl-C handler emits current summary; fmem side is idempotent so re-run is safe |
| F20 | summary | `--verbose` truncation hides the bit that changed | Operator misses the meaningful edit | 2 | 3 | 4 | 24 | Diff-aware truncation: prioritize changed lines; test: edit only the last line of a long skill, confirm that line appears in `--verbose` output |
| F21 | CLI | User passes `--filter` glob that matches zero skills | Silent exit 0 | 2 | 3 | 4 | 24 | Warn "filter matched 0 skills" on stderr; test: `--filter "nonexistent-*"` |
| F22 | CLI | `--force` flag interacts with `--dry-run` | Ambiguous semantics | 2 | 2 | 3 | 12 | `--force` has no effect in dry-run; documented; test: `--dry-run --force` output identical to `--dry-run` |
| F23 | taxonomy | Taxonomy pre-pass fails partway (fmem error after N of M tags) | Partial taxonomy in fmem; skills referencing untreated tags silently lack `TAGGED_AS` edges | 4 | 2 | 4 | **32** | Emit per-tag summary; exit 2 immediately on first transport error; document that re-run is safe because of content_hash idempotency; test: mock fails on Nth tag |
| F24 | taxonomy | `tag-hierarchy.yaml` missing but spec file references PARENT_TAG | Taxonomy silently flat; operator expected hierarchy | 3 | 3 | 5 | **45** | Log at `info` when hierarchy file is absent — "taxonomy will be flat"; test: run without the file, assert log line |
| F25 | taxonomy | `tag-hierarchy.yaml` has cycle (`a PARENT b`, `b PARENT a`) | Fmem graph becomes undefined; retrieval results leak via cycles | 4 | 2 | 4 | **32** | Pre-ingest DFS cycle detection (threat T6); exit 3 with both node names; test: synthetic cyclic fixture |
| F26 | taxonomy | `tag-hierarchy.yaml` references a tag that is not in the top-level dir list | Orphan edge in fmem; confuses retrieval | 3 | 4 | 4 | **48** | Validate all node names in hierarchy against the walked top-level dirs before any ingest; exit 3 naming orphan; test: hierarchy file references `nonexistent` |
| F27 | per-skill tag | Frontmatter `tags:` entry not seen in pre-pass; lazy `smart_ingest` inside skill loop fails | Skill ingested with missing `TAGGED_AS` edge | 3 | 3 | 4 | **36** | Either (a) preflight collect all tags across all skills before phase B and seed them, or (b) fail loud when lazy `smart_ingest` errors and skip the dependent skill; choose (a) for v1; test: skill has tag not in any top-level dir |
| F28 | per-skill tag | Case-insensitive collision between frontmatter tag and existing tag (`TDD` vs `tdd`) | Two tag entities in fmem, split retrieval | 3 | 3 | 5 | **45** | Normalize tag names to lowercase at parse time; document the normalization; test: skill frontmatter uses mixed-case tag |
| F29 | per-skill tag | Skill `name` collides with a tag `name` in fmem's flat namespace | Ambiguous retrieval; `retrieve_skills_for_context` may hit the tag entity | 4 | 2 | 5 | **40** | Add a pre-ingest check: no top-level dir / `tags:` entry may match any parsed skill `name`; exit 3 naming the collision; test: skill named `quality` and a top-level dir named `quality` |

## Work items to create (RPN ≥ 100)

None at RPN ≥ 100. Sprint-priority work items (at or above RPN 40 with retrieval-quality or data-correctness stakes):

- **WI-FMEA-01** (F7, RPN 50) — skill name collision detection
- **WI-FMEA-02** (F9, RPN 40) — document and lint for accepted step-section heading forms
- **WI-FMEA-03** (F11, RPN 45) — supplementary files included in hash input
- **WI-FMEA-04** (F16, RPN 60) — two-pass ingest for prerequisite edge creation
- **WI-FMEA-05** (F24, RPN 45) — log when `tag-hierarchy.yaml` is absent so taxonomy flatness is not silent
- **WI-FMEA-06** (F26, RPN 48) — validate hierarchy nodes against walked top-level dirs before any ingest
- **WI-FMEA-07** (F27, RPN 36) — preflight tag collection: walk *all* skills first, seed all tags, only then ingest skills (elevated: makes F27 structurally impossible)
- **WI-FMEA-08** (F28, RPN 45) — lowercase-normalize tag names at parse time
- **WI-FMEA-09** (F29, RPN 40) — skill-name / tag-name collision detection

## Test cases to generate (RPN ≥ 50, plus elevated)

Each becomes a function in the `cargo test` suite under `crates/ingest/src/skill_ingest/tests.rs` or `crates/cli/tests/fmem_skill_ingest.rs`:

1. `collision_across_categories_aborts` (F7) — two fixtures with same `name`, different dirs; assert exit 1 and error names both paths.
2. `supplementary_file_edit_changes_hash` (F11) — fixture with supplementary; edit it; assert different hash.
3. `prerequisite_ordering_creates_edges` (F16) — fixture where A requires B; run both orderings; assert edge exists in both.
4. `large_file_rejected_with_message` (F8) — 3 MiB SKILL.md; assert skip + warning.
5. `symlink_skipped_with_warning` (F3) — symlinked SKILL.md; assert not followed, warning emitted.
6. `dry_run_does_not_call_fmem` (F18) — mock client panics on any call; dry-run must succeed.
7. `broken_pipe_mid_run_exits_nonzero` (F12) — mock client closes pipe after N skills; assert exit code 2 and summary reflects partial work.
8. `response_id_shuffle_rejected` (F14) — mock transport shuffles response ids; assert ingest fails loudly.
9. `unusual_step_heading_empty_steps_warning` (F9) — fixture uses `## How to Use`; assert warning about empty `steps[]`.
10. `filter_matches_zero_warns` (F21) — `--filter` no-match; assert stderr warning.
11. `deterministic_ordering` (F17) — two runs, identical summary hashes.
12. `force_dry_run_equivalent` (F22) — `--force --dry-run` output equals `--dry-run`.
13. `taxonomy_partial_failure_exits_2` (F23) — mock fails on 3rd tag; assert exit 2 and summary shows 2 tags created, skill phase not entered.
14. `missing_hierarchy_logs_flat_taxonomy` (F24) — run without `tag-hierarchy.yaml`; assert info log "taxonomy will be flat".
15. `cyclic_hierarchy_rejected` (F25) — fixture with `a→b→a`; assert exit 3 naming both nodes.
16. `orphan_hierarchy_node_rejected` (F26) — hierarchy references `nonexistent`; assert exit 3 naming the orphan.
17. `preflight_tag_collection_before_skill_phase` (F27) — skill has tag not in any top-level dir; assert tag is created in phase A via preflight, not lazily in phase B.
18. `tag_case_normalization` (F28) — skill frontmatter uses `TDD`; assert only one `tdd` entity exists.
19. `skill_tag_name_collision_rejected` (F29) — skill named `quality` + top-level dir `quality`; assert exit 3.

## Correlated failures

- **F2 ∧ F12** — if permission denied on a subdirectory happens alongside an fmem crash, the partial-run summary can misrepresent which skills were not attempted vs. not reachable. Mitigation: the summary distinguishes `io_skipped` and `transport_failed` buckets.
- **F7 ∧ F11** — a name collision discovered only after some skills are already ingested leaves fmem in an undefined state. Mitigation: collision detection runs in a pre-ingest pass before any fmem call.
- **F23 ∧ F27** — taxonomy pre-pass fails after some tags are created; phase B (skill ingest) then lazy-creates missing tags on demand, amplifying the partial state. Mitigation: WI-FMEA-07 elevates tag collection into the preflight so phase B *never* creates a tag — if a tag is missing at phase B, that's a bug, not a dynamic condition.
- **F25 ∧ F29** — a skill named `communication` plus a hierarchy cycle could produce a fmem graph where the skill entity accidentally becomes its own tag's descendant. Mitigation: F29's pre-check detects the name collision first, before F25's cycle detection even runs.

## Closed-loop review

After implementation, re-run this FMEA against the actual code and:
- drop any mode that the compiler made impossible;
- raise any mode whose Detection score dropped (likely D1, because stdout/stderr becomes the source of truth and tests assert on specific strings).
