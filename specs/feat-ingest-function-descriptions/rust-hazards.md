# feat-ingest-function-descriptions — Rust Correctness Hazards

> Focused on the new code paths: provider abstraction, LLM client, Pass 2 integration.
> `.unwrap()`-class hazards apply globally — this doc highlights the extras specific to async I/O, external processes, and LLM response handling.

## P0 — Must not merge without

| ID | Hazard | Location risk | Guard |
|----|--------|---------------|-------|
| R-P0-1 | `.unwrap()` / `.expect()` on HTTP response, JSON parse, or env var lookup | provider clients, config load | `clippy::unwrap_used = "deny"` in `ingest/descriptions.rs`; all fallible ops via `?` |
| R-P0-2 | `panic!` on malformed LLM response (F8) | JSON parser | Structured parser returns `Result<Description, ExtractError>`; skip + log on error |
| R-P0-3 | Secret in doc comment sent over network unredacted (F4) | request builder | Redaction pass applied in `prepare_snippet()` before any provider sees it; unit test asserts redaction |
| R-P0-4 | Endpoint validation bypass (F11) | config load | `validate_local_endpoint()` enforces loopback; bypass requires explicit flag; unit test asserts non-loopback rejected |
| R-P0-5 | Unbounded concurrency (F12) | Pass 2 dispatcher | `tokio::sync::Semaphore(cfg.concurrency)` around every LLM call; no `spawn` without permit |
| R-P0-6 | Silent fallback when provider unavailable (violates fail-loud) | availability probe | On failure in non-interactive mode, return nonzero exit; never default to `skip` without explicit user choice |

## P1 — Should not merge without

| ID | Hazard | Guard |
|----|--------|-------|
| R-P1-1 | `await` while holding a sync lock (std `Mutex`) across provider call | Use `tokio::sync::Mutex` or confine locks to sync sections |
| R-P1-2 | Cancellation safety (caller drops the ingest future) | All writes to graph via a single `finalize()` that handles partial state; test with `tokio_test::task::spawn` + drop |
| R-P1-3 | HTTP client reuse | Construct one `reqwest::Client` per run (not per call); otherwise connection pool thrash |
| R-P1-4 | Timeout must actually cancel the request | `Client::builder().timeout(Duration::from_millis(cfg.local_timeout_ms))`; unit test asserts request aborted, not just error-returned after completion |
| R-P1-5 | Token/word clamp applied post-response | Clamp in Rust regardless of model obedience (F13) |
| R-P1-6 | Retry loop without a cap | Max 3 retries with exp backoff + jitter; retry budget per run enforced |
| R-P1-7 | Progress / observability | Tracing span per entity; counters for `extracted`, `skipped_low_confidence`, `skipped_trivial`, `redactions`, `malformed`, `timeouts`; summary printed at end (fail-loud disclosure of any degradation) |
| R-P1-8 | Non-UTF-8 path in entity source location | Already a pipeline concern; audit that new code doesn't reintroduce `to_str().unwrap()` |

## P2 — Monitor

| ID | Hazard | Guard |
|----|--------|-------|
| R-P2-1 | Prompt template drift between providers | Single source of truth for the prompt; provider clients only format-wrap it |
| R-P2-2 | Token count estimation drift | Use provider-declared usage when available; approximate (chars / 4) fallback; log both |
| R-P2-3 | Repeated calls for unchanged entities across runs | Cache by `(entity_id, body_hash, model)` — deferred to followup; noted to avoid shipping without |
| R-P2-4 | Env var leaks in error messages | Never include API key substrings in error strings; redact by length heuristic |

## CI enforcement

```toml
[lints.clippy]
unwrap_used      = "deny"
expect_used      = "deny"
panic            = "deny"   # in descriptions module; #[allow] with justification otherwise
await_holding_lock = "deny"
```

Grep gates:

```bash
# No unwrap/expect/panic in the new module
rg -n '\.unwrap\(\)|\.expect\(|panic!\(' crates/ingest/src/descriptions/ && exit 1

# No literal "skip" fallback without config / prompt check
rg -n 'provider\s*=\s*"skip"' crates/ingest/src/descriptions/ | grep -v 'test\|doc' && exit 1 || exit 0

# Every provider client must go through the redactor (enforced structurally by type signature, but grep as backstop)
rg -n 'fn send_to_provider' crates/ingest/src/descriptions/ | xargs -I {} rg 'redact' {}
```

## Async-specific invariants

1. No `block_on` inside async contexts (use `tokio::task::spawn_blocking` only for CPU-bound work — LLM calls are I/O, not this).
2. All async tasks spawned from Pass 2 must be tracked in a `JoinSet` and awaited in `finalize()` to guarantee clean shutdown.
3. `Drop` impls on in-flight state must not block (no sync-on-drop network calls).
