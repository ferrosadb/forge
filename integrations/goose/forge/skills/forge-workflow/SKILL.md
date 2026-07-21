---
name: forge-workflow
description: Apply Forge workflow discipline with small checklists, bounded attempts, acceptance criteria, and verification gates.
---

# Forge Workflow

Use this skill when a goose session should apply Forge workflow discipline: small checklist-driven work packets, explicit state, bounded attempts, and verification gates. The goal is steady progress without loops or scope creep.

## Operating principles

- Prefer compact, structured context over raw logs or broad exploration.
- Work in small packets with a clear acceptance criterion before editing.
- Keep task state explicit: pending, in progress, blocked/waiting, review-ready, or done.
- Treat checklists as execution state, not prose notes.
- Fail loud when evidence is missing; do not claim success from ambiguous output.
- Defer out-of-scope work instead of expanding the current packet.

## Start-of-work checklist

Before making changes:

1. Restate the requested outcome in one sentence.
2. Identify the smallest useful work packet that can be completed now.
3. Define acceptance criteria for that packet.
4. Identify likely verification gates, but do not invent project-specific commands unless they are already known from the repo or user.
5. Note explicit exclusions and defer them.

If the task is larger than one packet, create a short checklist with dependency order. Keep each item independently reviewable.

## Packet shape

Each packet should have:

- **Goal:** the user-visible outcome.
- **Scope:** files or behavior expected to change.
- **Acceptance:** observable criteria for completion.
- **Verification:** the narrowest trustworthy check available.
- **Out of scope:** related work intentionally deferred.

When dependencies exist, do not start blocked work. Move to a ready item or ask for the missing decision/input.

## Attempt control and anti-loop behavior

Avoid repeated low-progress attempts on the same item.

- Before retrying, state what changed about the next attempt.
- Do not repeat an identical command/edit/search sequence unless the environment changed or the retry is deliberate verification.
- After two failed or low-progress attempts, pivot: narrow the packet, inspect different evidence, ask for input, or mark the item blocked/waiting.
- Keep legitimate verification separate from implementation attempts.
- If a loop is detected, record the blocker and propose one structurally different next action.

Useful retry question: “What new evidence or changed condition makes this attempt different?” If the answer is “nothing,” do not retry.

## Checklist discipline

Use checklists to drive execution:

- Keep items small enough to finish in one focused pass.
- Mark only completed work as done.
- Record blockers as blockers, not as vague notes.
- Preserve dependency order: complete prerequisites before dependent items.
- Prefer serial execution for mutating code unless the repo explicitly supports isolated parallel worktrees.
- Surface waiting decisions to the user with a precise question.

A good checklist item is concrete: “Create goose plugin manifest” is better than “Integrate Forge.”

## Acceptance criteria

For every packet, define completion before doing the work. Good criteria are:

- File/path exists with expected content.
- Behavior is documented or implemented as requested.
- A bounded verification check passed, or a caveat explains why it was not run.
- Out-of-scope work is explicitly deferred.

Do not use broad success language unless the acceptance criteria were actually met.

## Verification gates

Prefer the narrowest reliable verification available from project instructions or existing docs. Examples of gate types:

- formatting check for touched source code,
- targeted unit or parser tests for behavior changes,
- schema or JSON validation for manifest/config files,
- file existence/content inspection for documentation-only changes,
- final diff review for scope control.

If no verified command is available, say so and use a bounded non-destructive inspection instead. Avoid inventing exact CLI usage.

## Scope control

When adjacent work appears:

1. Decide whether it is required for the current acceptance criteria.
2. If not required, write it down as deferred follow-up.
3. Continue the current packet or stop for review.

Do not turn a small integration task into tool implementation, repo refactoring, or broad documentation unless the user explicitly asks.

## Completion response

End with a concise report:

- files changed or created,
- acceptance criteria met,
- verification performed,
- caveats or deferred follow-ups.

Keep the report factual and avoid claiming commands or integrations were tested unless they were actually run.
