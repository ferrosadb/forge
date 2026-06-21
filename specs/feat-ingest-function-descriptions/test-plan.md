# feat-ingest-function-descriptions — Test Plan

> 7-layer test spec. Every FMEA row with RPN ≥ 50 has at least one test referencing its ID.

## Layer 1 — Unit tests (pure functions)

| Test | FMEA / Hazard | Notes |
|------|---------------|-------|
| `redactor_masks_api_key_patterns` | F4 / R-P0-3 | Snippet containing `API_KEY=sk-abc` / `AWS_SECRET_*` / `-----BEGIN PRIVATE KEY-----` → all redacted |
| `redactor_counts_redactions` | F4 | Returns `(redacted_text, count)`; count surfaces in run report |
| `validate_local_endpoint_loopback_only` | F11 / R-P0-4 | `http://127.0.0.1:11434` ok; `http://10.0.0.5:11434` rejected without flag |
| `validate_local_endpoint_scheme` | F11 | `file://`, `ssh://`, `tcp://` rejected |
| `parse_ollama_tags_response` | F2 | Fixture JSON → list of models |
| `parse_openai_models_response` | F2 | OpenAI-compatible `/v1/models` fixture |
| `schema_validator_rejects_oversized` | F13 | 500-word response → clamped to `max_words` |
| `schema_validator_rejects_non_json` | F8 | Prose, markdown, empty → `Err(Malformed)` |
| `schema_validator_rejects_prompt_echo` | F14 | Response containing known prompt phrase → `Err(PromptLeak)` |
| `schema_validator_strips_control_chars` | T10 | Response with `\x1b`, `\0` → stripped or rejected |
| `jailbreak_sentinel_detector` | F3 / T3 | Inputs like "ignore previous", "SYSTEM:" → description rejected |
| `estimate_call_count` | F6 | Given entity list → accurate eligible count |
| `provenance_schema_required_fields` | F15 | Fact missing `provider` / `model` / `confidence` → build error |

## Layer 2 — Property tests

| Test | Covers |
|------|--------|
| `prop_redactor_idempotent` | Redact twice = redact once |
| `prop_word_clamp_bound` | For any response, output word count ≤ `max_words` |
| `prop_skip_trivial_delegates` | Any single-expression body → skipped |
| `prop_provenance_roundtrip` | Serialize / deserialize preserves all provenance fields |

## Layer 3 — Fixture / integration tests

| Test | FMEA |
|------|------|
| `endpoint_unreachable_aborts_pass2_keeps_pass1` | F1 — mid-run endpoint kill → Pass 1 facts committed, Pass 2 aborts loudly |
| `missing_model_triggers_prompt_tty` | F2 — with mocked TTY, prompt appears and Pass 2 honors selection |
| `missing_model_non_interactive_exits_nonzero` | F2 — no TTY + missing model → exit 2 with guidance on stderr |
| `rate_limit_backoff_survives` | F7 — mock 429 server with eventual 200 → run completes |
| `rate_limit_exhaustion_skips_entity` | F7 — persistent 429 → skip + log, run finishes |
| `timeout_per_call_honored` | F10 — slow mock server → individual call times out, loop continues |
| `cost_cap_exits_when_estimate_exceeds` | F6 — synthetic 10k-entity repo, `--max-desc-calls 100` → preflight warns and exits |
| `dangling_entity_drops_gracefully` | F9 — delete entity between Pass 1 and Pass 2 write → drop + log, no crash |
| `concurrency_bound_respected` | F12 / R-P0-5 — instrumented mock counts in-flight ≤ configured concurrency |
| `provider_banner_shown_for_remote` | T1 — `provider = openai` → banner printed before any call; `--yes` or non-interactive+env-var required |
| `remote_requires_typed_consent` | T1 — interactive mode, remote provider, no `--yes` → typed "yes" prompt |

## Layer 4 — Snapshot tests (`insta`)

- Run report on a deterministic fixture corpus (no network): extracted/skipped/redacted/clamped counts
- Error messages for each failure path (unreachable, missing model, missing env var, cost cap) — stable human-readable output

## Layer 5 — Contract / round-trip tests (ferrosa-memory)

| Test | Covers |
|------|--------|
| `fact_written_and_read_back` | End-to-end write → `retrieve_entities` → assert description + full provenance |
| `hybrid_search_returns_description` | Write facts → `hybrid_search("what does X do")` → description surfaces |
| `disabled_mode_writes_zero_description_facts` | `enabled=false` and `provider="skip"` both produce identical graph output to current baseline |

## Layer 6 — Adversarial corpus (security regression)

| Corpus | Purpose |
|--------|---------|
| `tests/adversarial/doc-prompt-injection/` | 30 crafted doc comments attempting prompt injection (F3 / T3) |
| `tests/adversarial/secret-in-doc/` | 20 doc comments with secret-shaped strings (F4) |
| `tests/adversarial/hallucination-bait/` | Functions whose name/body are misleading — assert descriptions either match body or are rejected by confidence floor |

Each corpus run must: (a) never leak a secret in the outgoing payload, (b) never store an attacker-controlled description unfiltered, (c) produce stable counts across runs (record baseline, alert on drift).

## Layer 7 — Performance / cost ceiling

| Bench | Target |
|-------|--------|
| `ingest_baseline_vs_descriptions_local` | Descriptions-on wall time ≤ 3× baseline on 200-entity fixture (spec §overview constraint) |
| `memory_bounded_at_20k_entities` | Peak RSS < 500MB during a 20k-entity run (F12) |

Perf is non-gating but tracked; regressions > 2× flagged in CI comment.

## Coverage gate

- ≥ 85% line coverage for `crates/ingest/src/descriptions/`
- Functions with CC ≥ 15 require docstrings + 90% local coverage

## RPN coverage audit

CI grep: for every `Fn` ID in `fmea.md` with RPN ≥ 50, assert presence in at least one test name or `// FMEA: F3` comment in `crates/ingest/src/descriptions/**/tests/`.

## Mock provider for hermetic tests

A trait-based mock is required for Layers 3–6 to avoid real network calls:

```rust
trait DescriptionProvider {
    async fn extract(&self, snippet: &Snippet) -> Result<Description, ProviderError>;
    async fn probe(&self) -> Result<ProbeInfo, ProviderError>;
}
```

Production impls: `OllamaProvider`, `OpenAIProvider`, `AnthropicProvider`, `SkipProvider`.
Test impls: `MockProvider` (scripted responses), `FlakyProvider` (controlled 429/timeout injection).
