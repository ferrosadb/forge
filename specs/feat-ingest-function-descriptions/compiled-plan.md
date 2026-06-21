# feat-ingest-function-descriptions — Compiled Execution Plan

> Agent-executable plan. Each task is self-contained; siblings in a batch may run in parallel.

## Dependency DAG

```text
          ┌──────────────────────┐
          │ T1 Config surface    │
          └──────────┬───────────┘
                     │
       ┌─────────────┼─────────────┐
       ▼             ▼             ▼
   ┌────────┐   ┌─────────┐   ┌──────────┐
   │T2 Trait│   │T3 Redact│   │T4 Schema │
   │Provider│   │         │   │Validator │
   └───┬────┘   └────┬────┘   └────┬─────┘
       │             │              │
       └─────────────┼──────────────┘
                     ▼
          ┌──────────────────────┐
          │ T5 Provider clients  │
          │ (local|openai|anth)  │
          └──────────┬───────────┘
                     │
          ┌──────────────────────┐
          │ T6 Availability probe│
          │ + interactive prompt │
          └──────────┬───────────┘
                     │
          ┌──────────────────────┐
          │ T7 Pass 2 orchestr.  │
          │ concurrency + retry  │
          └──────────┬───────────┘
                     │
          ┌──────────────────────┐
          │ T8 Graph write       │
          │ (temporal facts)     │
          └──────────┬───────────┘
                     │
          ┌──────────────────────┐
          │ T9 Run report + obs. │
          └──────────┬───────────┘
                     │
          ┌──────────────────────┐
          │ T10 Test suite + CI  │
          └──────────────────────┘
```

## Task packets

### T1 — Config surface

- **Deliverable:** `[ingest.descriptions]` section in forge config loader with all fields documented; CLI flags for per-run override.
- **Files:**
  - `crates/ingest/src/config.rs` — add `DescriptionsConfig` struct with serde + defaults
  - `crates/cli/src/main.rs` — CLI flags `--desc-provider`, `--desc-model`, `--desc-endpoint`, `--max-desc-calls`, `--non-interactive`
- **Verification:** `cargo test -p ingest config::desc_defaults`; CLI-level snapshot of `--help` for `ingest` subcommand

### T2 — `DescriptionProvider` trait + `SkipProvider`

- **Deliverable:** Trait from `test-plan.md §Layer 7`; `SkipProvider` as no-op impl enabling `provider="skip"` with zero network I/O.
- **Files:**
  - `crates/ingest/src/descriptions/mod.rs`
  - `crates/ingest/src/descriptions/provider.rs`
  - `crates/ingest/src/descriptions/providers/skip.rs`
- **Verification:** `disabled_mode_writes_zero_description_facts` test passes

### T3 — Redactor

- **Deliverable:** Pure-function redactor matching secret-scan patterns; returns `(redacted, count)`.
- **Shared source:** Reuse pattern list from `crates/secret-scan` (do not duplicate).
- **Guards:** R-P0-3 (applied before any provider sees snippet).
- **Verification:** Layer-1 tests `redactor_masks_api_key_patterns`, `redactor_counts_redactions`; property `prop_redactor_idempotent`

### T4 — Response schema + validator

- **Deliverable:** Strict JSON schema for LLM response; parser + validator covering word clamp (F13), control-char strip (T10), prompt-echo (F14), jailbreak-sentinel (F3) rejection.
- **Files:** `crates/ingest/src/descriptions/schema.rs`
- **Verification:** Layer-1 schema tests all pass

### T5 — Provider clients

- **Deliverable:** `OllamaProvider`, `OpenAIProvider`, `AnthropicProvider`; all implement `DescriptionProvider`.
- **Guards:**
  - Single shared `reqwest::Client` per provider instance (R-P1-3)
  - Timeout on client builder (R-P1-4)
  - Endpoint validation for local (R-P0-4 / F11)
  - All requests flow through redactor (R-P0-3)
- **Verification:** `validate_local_endpoint_loopback_only`, `parse_ollama_tags_response`, `parse_openai_models_response`; mocked provider tests

### T6 — Availability probe + interactive prompt

- **Deliverable:** Pre-flight probe that validates selected provider, prompts on failure (TTY) or fails loudly (non-TTY).
- **Critical invariant:** Never silently degrade to `skip`. Fail-loud rule (R-P0-6).
- **Files:** `crates/ingest/src/descriptions/probe.rs`; prompt helpers in `crates/cli/src/interactive.rs`
- **Verification:** `missing_model_triggers_prompt_tty`, `missing_model_non_interactive_exits_nonzero`, `endpoint_unreachable_aborts_pass2_keeps_pass1`

### T7 — Pass 2 orchestrator

- **Deliverable:** Concurrent extraction loop with bounded concurrency (`Semaphore`), exponential-backoff retry (max 3), per-call timeout, per-run call cap.
- **Guards:**
  - R-P0-5 (semaphore around every spawn)
  - R-P1-1 (no sync mutex across await)
  - R-P1-2 (cancellation-safe finalize)
  - R-P1-6 (retry cap honored)
- **Files:** `crates/ingest/src/descriptions/orchestrator.rs`
- **Verification:** `concurrency_bound_respected`, `rate_limit_backoff_survives`, `rate_limit_exhaustion_skips_entity`, `timeout_per_call_honored`

### T8 — Graph write (temporal facts)

- **Deliverable:** Serialize `Description` + provenance to `write_temporal_fact`; handle dangling-entity case.
- **Guards:** F9 (dangling entity → drop + log, never crash); F15 (provenance fields enforced at type level).
- **Files:** `crates/ingest/src/descriptions/writer.rs`
- **Verification:** `dangling_entity_drops_gracefully`, `fact_written_and_read_back`, `provenance_schema_required_fields`

### T9 — Run report + observability

- **Deliverable:** Structured end-of-run report with counts: extracted, skipped_trivial, skipped_low_confidence, redactions, malformed, timeouts, retries, cost-capped.
- **Guards:** R-P1-7 (fail-loud disclosure — any nonzero in "skipped" categories must be visible).
- **Files:** `crates/ingest/src/descriptions/report.rs`
- **Verification:** Snapshot test on report rendering for mixed-outcome fixture

### T10 — Test suite + CI wiring

- **Deliverable:** All Layer 1–7 tests implemented; adversarial corpora committed; RPN-coverage audit script; CI jobs for hermetic (mock-only) and optional-local-model integration.
- **Files:**
  - `crates/ingest/tests/descriptions_*.rs`
  - `crates/ingest/tests/adversarial/` corpora
  - `ci/rpn-coverage-audit.sh`
- **Verification:**
  - `cargo test -p ingest` green
  - `cargo clippy -- -D warnings` green
  - RPN-coverage audit 100%
  - Coverage ≥ 85% for `crates/ingest/src/descriptions/`

## Parallel execution batches

- **Batch 1:** T1
- **Batch 2:** T2, T3, T4 (parallel — disjoint files)
- **Batch 3:** T5
- **Batch 4:** T6
- **Batch 5:** T7
- **Batch 6:** T8
- **Batch 7:** T9
- **Batch 8:** T10

## Three-tier verification

| Tier | Gate | Command |
|------|------|---------|
| Per-task | Task tests green | `cargo test -p ingest <pattern>` |
| Feature | Hermetic suite green | `cargo test -p ingest --test descriptions_*` |
| System | Workspace lint + coverage + RPN audit + adversarial corpora | `make test-all` |

## Resolved ambiguities

| Question (from overview / threat-model) | Resolution |
|----------------------------------------|------------|
| Always redact for local too (T2)? | **Yes.** Cheap, safe; closes F4 deterministically |
| Cache across runs? | **Deferred.** Followup spec; v1 ships without |
| Typed confirmation for remote? | **Yes for remote**, simple Y/n for local |

## Exit criteria

- All verification checkboxes in `overview.md §Verification` (inherited from source spec) hold
- All P0 hazards in `rust-hazards.md` guarded by tests
- RPN-coverage audit green
- Adversarial corpora produce stable (recorded) counts
- Feature flag `enabled = false` produces byte-identical graph output vs baseline (regression-proof)
- Spec moves from `todo/` → `implemented/` when T10 lands; moves to `verified/` after two release cycles with no regression
