---
executive_summary: >
  Keep operational workflow state in Forge checklists, normalized meaningful
  attempt history and folds in Ferrosa, raw output in terminal scrollback, and
  a local append-only journal only during a verified Ferrosa outage.
---

# ADR-001: Separate Operational Attempt State from Semantic Memory

## Status

Proposed

## Date

2026-07-12

## Context

Agents can repeat identical or semantically similar work without advancing the
larger goal. Preventing this requires durable operational counters, similarity
history, review and override records, and enough command evidence to explain a
loop. Storing every raw command and output in semantic memory would be noisy,
expensive, and unsafe; keeping all history only in project-local files would
lose cross-session and learned anti-fixation behavior.

## Decision

Forge checklists will own operational state: item status, leases, dependencies,
waiting gates, current review, bounded attempt fingerprints, repeat counters,
priority components, and Ferrosa event references.

Ferrosa will own normalized meaningful attempt events, semantic similarity,
loop incidents, durable reviews and overrides, and folded goal-, project-,
agent-, and model-level anti-fixation memories. Ordinary events are retained for
90 days before folding; loop incidents and human decisions remain indefinitely.

Raw stdout, stderr, unredacted commands, pane captures, and polling are not
persisted by default. A project-local append-only JSONL journal is used only
after a bounded Ferrosa handshake/write failure. It is reconciled idempotently
and deleted after acknowledgment.

## Consequences

### Positive

- Forge can enforce exact repeat limits even when Ferrosa is down.
- Ferrosa can recognize near loops across turns, items, and models.
- The system retains human reasons and successful pivots without accumulating
  raw terminal noise.
- Checklist files remain bounded, inspectable, and project-local.
- The outage journal is a fallback rather than a competing source of truth.

### Negative

- The attempt controller must coordinate two stores and reconcile failures.
- Semantic enforcement is unavailable during a Ferrosa outage and therefore
  fails open.
- Retention and folding require lifecycle jobs and idempotent event identities.

### Neutral

- Existing checklist schema versions remain readable through optional fields.
- Human terminal commands remain outside hook enforcement.

## Alternatives Considered

### Store every command in Ferrosa

Rejected because raw command streams are noisy, may expose secrets, and blur
the distinction between operational state and reusable knowledge.

### Store all attempt history in checklist JSON

Rejected because checklist files would grow without bound and could not support
cross-session semantic similarity or memory folding efficiently.

### Always keep a parallel local journal

Rejected because two continuously active histories create reconciliation and
source-of-truth ambiguity. The local journal exists only during a verified
Ferrosa outage.

### Exact fingerprints only

Rejected because agents can repeat the same hypothesis through different
commands, files, prompts, or models while making negligible progress.

## Related Decisions

- [Forge anti-loop attempt control](../anti-loop-attempt-control.md)
- [Checklist DAG project plan](../checklist-dag-project-plan.md)
- [Hooks integration](../hooks-integration.md)
