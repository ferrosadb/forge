# fmem-skill-ingest — Rust Correctness Hazards

> Phase 5 of blueprint. Forward-looking hazard review — the code doesn't exist yet, so this is a *guardrail checklist* the implementation must satisfy, not a diagnostic scan.
> Updated 2026-04-16: added P1-6 for tag-hierarchy cycle detection and P2-6 for tag-name normalization.

Stack: Rust (stable) inside the forge workspace. Repo convention (Power-of-10 adapted to Rust, plus the fail-loud philosophy in `skills/rules/safety.md`) applies.

## P0 — data loss / security

| ID | Rule | Where it applies in this feature | Required pattern |
|---|---|---|---|
| P0-1 | No `.unwrap()` / `.expect()` in library code paths that can see untrusted input | `skill_ingest::parse`, `fmem_client::transport` | Return `Result<_, E>` with a typed error; reserve `expect` for invariants the compiler can prove |
| P0-2 | No silent `let _ =` on fallible calls | Everywhere | Use `?` or explicit handling; if truly ignoring, add `// intentionally ignored because <reason>` |
| P0-3 | Canonicalize all paths before use | `skill_ingest::walk` (supplementary file resolution) | `Path::canonicalize` then assert prefix equals the skill-dir canonical path (threat-model T3) |
| P0-4 | No following symlinks when walking untrusted trees | `skill_ingest::walk` | `walkdir::WalkDir::new(root).follow_links(false)` (threat-model T4) |
| P0-5 | Don't log raw frontmatter/body content on error paths | all error sites | Only path + short reason (threat-model I2) |
| P0-6 | Strict id matching on JSON-RPC responses | `fmem_client::transport` | Map from request-id → pending oneshot; reject mismatches (FMEA F14) |

## P1 — concurrency / resource

| ID | Rule | Where it applies | Required pattern |
|---|---|---|---|
| P1-1 | Bound all dynamic collections | `skill_ingest::summary` (per-skill diff entries, per-category counts) | Cap at total number of SKILL.md walked; there is no streaming unbounded input |
| P1-2 | Bounded loops | walker depth cap, per-file read cap | Depth 10; per-file 2 MiB (threat-model D1/D2, FMEA F8) |
| P1-3 | No allocation in hot loops | parse inner loops | Reuse buffers where it matters; 78 skills is small, so this is a soft guideline — don't over-engineer |
| P1-4 | Per-call timeout on MCP | `fmem_client::transport` | `recv_timeout` on the response channel; default 10s, configurable |
| P1-5 | Single in-flight request for v1 | `fmem_client` | Serialize requests; concurrency is a follow-up |
| P1-6 | Bounded tag-hierarchy graph traversal | `skill_ingest::taxonomy` | DFS with visited set; cap edge count at 1000 (threat-model D4); FMEA F25 cycle detection |

## P2 — latent bugs

| ID | Rule | Where it applies | Required pattern |
|---|---|---|---|
| P2-1 | Short functions (≤60 lines logic, target 25) | everywhere | Per repo `skills/rules/safety.md` Rule 4 |
| P2-2 | Assertion density — validate invariants | `skill_ingest::parse`, `hash` | Debug-only `debug_assert!` is fine for invariants; return errors for anything touching untrusted input |
| P2-3 | Deterministic ordering | walker, hash computation over supplementary files | Sort paths before hashing / iterating (FMEA F11, F17) |
| P2-4 | No `goto`/labeled break equivalents | walker | Use early returns + guard clauses |
| P2-5 | No unbounded recursion | nowhere obvious here; watch the walker | `walkdir` is iterative — OK |
| P2-6 | Normalize tag names at parse time | `skill_ingest::parse`, `skill_ingest::taxonomy` | Lowercase + trim; reject non-matching (threat T7, T8); FMEA F28 |

## Fail-loud compliance (`skills/rules/safety.md`)

Every failure mode in the FMEA must have an observable surface. Implementation-level audit hooks:

- `skill_ingest::summary` keeps buckets: `created`, `updated`, `skipped_unchanged`, `skipped_error`, `io_error`, `transport_error`, `schema_error`. Each bucket has a count; the summary emits all of them even when zero, so a future operator can tell the difference between "no transport errors" and "the transport-error bucket was removed."
- `fmem_client::error` has one variant per failure class (transport/protocol/tool/schema/timeout). No catch-all `Other(String)` — every new failure class gets a new variant and a reviewer. (Revisit if this proves impractical during implementation; document the exception.)
- Never return `Ok(empty_summary)` on partial failure. Exit codes:
    - 0 — zero failed, zero skipped-by-error
    - 1 — parse errors / schema errors (user-fixable)
    - 2 — transport / server errors (environment)
    - 3 — precondition errors (`--root` missing, collision, etc.)

## Rust-specific footguns to watch

| Pattern | Risk | Guardrail |
|---|---|---|
| `.to_string_lossy()` on paths | silent loss of non-UTF-8 filenames → hash instability | Fail on non-UTF-8 skill paths; emit warning naming the OsStr hex |
| `String::from_utf8_lossy` on file bodies | silently rewrites bytes → hash drift | Read as bytes, reject files that aren't valid UTF-8 |
| `HashMap` iteration for hash inputs | nondeterministic ordering | Use `BTreeMap` or sort keys before hashing |
| `serde_yaml` default behavior | (historical) anchor expansion — bomb risk | Pin the version; review against advisories; cap frontmatter size pre-parse |
| `tokio` vs sync | introduces async surface for a one-shot CLI | Use blocking I/O end-to-end; single-threaded by design for v1 |
| Subprocess via `Command` | shell escaping vulnerabilities | For fmem stdio, use `Command::new(path).arg(...)`; never pass through a shell |
| `Arc<Mutex<_>>` as convenience | hides ownership intent | Not needed for single-threaded v1; rebuff it in review |

## CI guardrails to add

- `cargo clippy --workspace -- -D warnings` — already enforced by existing CI (verified during previous commit)
- `cargo fmt --all -- --check` — already enforced
- Add per-PR: `cargo deny check advisories` for `serde_yaml` / `serde_json` / `sha2` / `walkdir` / transport crates (captured as a work item in Phase 6)

## What this does *not* cover

- Functional behavior tests — those live in FMEA (Phase 4) and the compiled plan (Phase 10) as verification tier 2/3 tasks.
- Threat-model mitigations — those live in `threat-model.md` and are cross-linked from work items.
- Performance — 78 skills on a local filesystem is tiny; no hot-path tuning for v1.
