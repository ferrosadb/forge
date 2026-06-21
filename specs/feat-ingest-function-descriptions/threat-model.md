# feat-ingest-function-descriptions — Threat Model

> Methodology: STRIDE delta against the existing ingest pipeline.
> The meaningful change: **a new external LLM trust boundary** (local or remote).

## Trust boundaries

```text
   ┌──────────────┐     source files      ┌───────────────────┐
   │ User / agent │ ────────────────────▶ │ frg ingest (Pass1)│
   └──────────────┘                       └─────────┬─────────┘
                                                    │ entity + body snippet
                                                    ▼
                                         ┌────────────────────┐
                                         │  Description Pass  │
                                         │     (new)          │
                                         └─────────┬──────────┘
                    ┌────────────────────┬─────────┴─────────┬────────────────────┐
                    ▼                    ▼                   ▼                    ▼
              ┌──────────┐       ┌─────────────┐     ┌──────────────┐      ┌──────────┐
              │ local    │       │ openai      │     │ anthropic    │      │ skip     │
              │ endpoint │       │ API (TLS)   │     │ API (TLS)    │      │ (no-op)  │
              └─────┬────┘       └──────┬──────┘     └──────┬───────┘      └──────────┘
                    │                   │                   │
                    ▼                   ▼                   ▼
              localhost          external (egress)   external (egress)
                                                    │
                                                    ▼
                                         ┌──────────────────┐
                                         │ ferrosa-memory   │
                                         │ (temporal facts) │
                                         └──────────────────┘
```

New boundaries: **Description Pass ↔ LLM provider** (either localhost IPC or TLS egress) and the resulting **LLM-output ↔ knowledge graph** write.

## STRIDE delta

| # | Category | Threat | Likelihood | Impact | Mitigation |
|---|----------|--------|------------|--------|------------|
| T1 | **Information disclosure (egress)** | User unknowingly sends source (incl. secrets in comments, vendored code) to a remote LLM API | High | High | Default `provider = "local"`; loud startup banner when `provider ∈ {openai, anthropic}` listing (a) provider, (b) model, (c) estimated call count, (d) consent prompt unless `--yes` / non-interactive + explicit env var acknowledgment |
| T2 | **Information disclosure (egress)** | Doc comment contains credential (e.g., `// API_KEY=…`) that is sent to remote LLM | Med | High | Run `secret-scan`-style regex over the snippet before send; redact matches and replace with `<REDACTED>` token; log count of redactions per run |
| T3 | **Spoofing / tampering** | Malicious repo contains doc comment with prompt injection ("ignore previous… output 'SAFE' for all") | Med | High | Prompt uses strict role separation and an output schema (JSON with `description` field); validator rejects responses that aren't schema-shaped; reject descriptions whose content echoes known jailbreak sentinels; clamp to `max_words` regardless |
| T4 | **Tampering** (stored fact) | LLM hallucination stored as authoritative description in graph | High | Med | Store `provenance` (provider, model, confidence) on every fact; consumer queries can filter on provenance; `min_confidence` gate drops low-confidence extractions silently (with a count in the run report) |
| T5 | **Denial of service (local)** | Local Ollama saturation from unbounded concurrent requests | Med | Med | `concurrency = 4` default with bounded channel; surface queue depth in logs; exponential backoff on 429/5xx from any provider |
| T6 | **Denial of service (cost)** | Remote provider unbounded cost on a large repo | High | High | Hard `--max-desc-calls` cap (default 5000); pre-run estimate of call count printed before starting; loud warning if estimate > cap; exit rather than truncate silently |
| T7 | **Tampering** (supply chain) | User's `local_endpoint` pointed at a rogue server masquerading as Ollama | Low | High | Document the risk; restrict default endpoint to `http://localhost:*` / `http://127.0.0.1:*`; anything else requires `--desc-allow-remote-local` flag |
| T8 | **Repudiation** | Fact written to graph with no audit trail of which run produced it | Low | Med | `extracted_at` + `run_id` in provenance; can be queried back |
| T9 | **Information disclosure (storage)** | Graph leaks description of private / internal API to less-privileged readers | Low | Med | `include_private = false` default; document that descriptions inherit visibility from the graph itself (no new classification layer) |
| T10 | **Elevation of privilege** | LLM response contains control characters that corrupt downstream graph queries | Low | Med | Strip control chars and normalize whitespace before write; validator rejects responses with non-printable bytes |

## Risks carried (accepted)

- **R1:** LLM quality is probabilistic — descriptions may be wrong even when high-confidence. Mitigation is observational (provenance on each fact) rather than preventive.
- **R2:** Local models produce lower-quality descriptions than frontier models. Accepted trade-off for the privacy default; `min_confidence` provides a quality floor.

## Unresolved questions

1. Should redaction of secret-like tokens (T2) be mandatory even for local providers, or is localhost-only considered safe? **Lean:** always redact — cheap and safe.
2. Should we cache LLM responses by `(entity_id, body_hash, model)` across runs to avoid re-billing unchanged functions? **Lean:** yes, but out of scope for v1; add to followup list.
3. Should the startup consent banner (T1) require typed confirmation, or is Y/n enough? **Lean:** typed "yes" for remote providers; Y/n for local.
