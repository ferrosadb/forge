# feat-ingest-function-descriptions — FMEA

> RPN = S × O × D (1–10 each). RPN ≥ 200 → P1; 50–199 → test required; < 50 → monitor.

## Failure modes

| # | Failure mode | Cause | Effect | S | O | D | RPN | Mitigation | Test |
|---|--------------|-------|--------|---|---|---|-----|------------|------|
| F1 | Local endpoint unreachable mid-run | Ollama crash / network flap | Pass 2 stalls, Pass 1 output possibly lost if not committed | 7 | 5 | 5 | **175** | Probe at start (T1); retry 3× with exponential backoff per call; on repeated failure, abort Pass 2 loudly and finalize Pass 1 writes | Integration: kill endpoint mid-run → graceful abort, Pass 1 facts intact |
| F2 | Local model missing from endpoint | User changed config, didn't pull model | Run aborts (expected) or silently degrades (bug) | 6 | 7 | 4 | **168** | Startup probe lists available models; interactive prompt on mismatch; non-interactive = nonzero exit with guidance | Unit: probe parses `/api/tags` response; integration: mismatched model triggers prompt |
| F3 | Prompt injection via hostile doc comment | Malicious repo | Graph poisoned with attacker-chosen description | 8 | 4 | 8 | **256** | Structured output schema, validator, clamp to `max_words`, strip control chars, reject jailbreak sentinels | Fixture: doc comments from adversarial corpus (see `test-plan.md`) → validator rejects or clamps |
| F4 | Secret leaked to remote provider | Credential in doc comment | Credential exfil to third party | 10 | 3 | 9 | **270** | Redact secret-scan patterns before any send (even local); log redaction count; consent banner lists remote provider prominently | Unit: snippet with `API_KEY=...` → redacted. Integration: remote call sees no raw secret |
| F5 | LLM hallucination stored as truth | Model confidently wrong | Agents act on false summary | 7 | 8 | 7 | **392** | Provenance on every fact; `min_confidence` floor; document that descriptions are advisory, not canonical; consumer-side filter possible | Fixture: hallucination test corpus; stored facts carry confidence; assert downstream consumers can filter |
| F6 | Remote cost runaway | Huge repo, remote provider, no cap | Surprise API bill | 9 | 4 | 6 | **216** | Pre-run estimate (count of eligible entities) printed; hard `--max-desc-calls` cap; loud exit when estimate > cap | Unit: estimator counts correctly; integration: cap honored, partial result marked |
| F7 | Rate limit / 429 cascade | Burst of concurrent requests | Partial graph writes, retries thrash | 5 | 6 | 5 | 150 | Concurrency bounded (default 4); exponential backoff with jitter; max-retries per call; on exhaustion, skip entity + log | Integration: mock 429 server → bounded retry, no crash |
| F8 | Non-JSON / malformed response | Model ignores output schema | Parse failure cascades | 6 | 5 | 4 | 120 | Strict parser with fallback to "extract first code-fence" then give up; skip + log rather than crash; count malformed in report | Fixture: model returns prose, markdown, empty string → all handled |
| F9 | Dangling fact: entity deleted between Pass 1 and Pass 2 | Race with external graph write | Fact references nonexistent entity | 5 | 3 | 7 | 105 | Write within a transaction per-entity; on "entity not found" at write time, drop and log; run report tallies drops | Fixture: delete entity between passes → drop without crash |
| F10 | Timeout per call exhausts queue | Slow model + many entities | Run takes hours | 4 | 6 | 5 | 120 | Per-call timeout (default 5s local, 30s remote); on timeout, skip + log; progress indicator shows rate | Integration: slow-mock server → timeouts honored, progress continues |
| F11 | Rogue "localhost" endpoint pointing off-box | Misconfigured `local_endpoint` | Data exfil under "local" provider label | 9 | 2 | 8 | 144 | Validate endpoint scheme+host before use; reject non-loopback unless `--desc-allow-remote-local` | Unit: endpoint validator rejects non-loopback; integration: flag required |
| F12 | Concurrency explodes caller process | Unbounded channel, 20k entities | OOM / thrash | 7 | 2 | 4 | 56 | Bounded channel with backpressure; respect `concurrency` config | Load test: 20k entities, memory bounded |
| F13 | Description length blows schema | Model ignores `max_words` | Oversized facts bloat graph | 3 | 6 | 3 | 54 | Post-process clamp to `max_words` regardless of model obedience; track clamp rate | Unit: 500-word response → truncated to limit |
| F14 | Prompt instruction leak | Model echoes system prompt as description | Useless description, privacy | 4 | 4 | 5 | 80 | Validator rejects responses containing prompt fragments (substring match against known prompt phrases) | Fixture: model echoes prompt → rejected |
| F15 | Provenance missing on stored fact | Writer bug | Can't distinguish provider quality | 4 | 3 | 8 | 96 | Schema enforces provenance fields; compile-time if possible, runtime assert otherwise; integration test reads back and asserts | Integration: write → read → assert provenance round-trips |

## Priority summary

- **P1 (RPN ≥ 200):** F3 (prompt injection), F4 (secret leak), F5 (hallucination), F6 (cost runaway)
- **P2 (50–199):** F1, F2, F7, F8, F9, F10, F11, F12, F13, F14, F15
- **P3 (<50):** none

## Test-case generation rule

Every RPN ≥ 50 row MUST have a corresponding test referenced in `test-plan.md`. CI audits by grep of `Fn` IDs in test names/docstrings.
