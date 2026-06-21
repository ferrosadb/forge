# Feat: checklist_state — Persistent workflow checklist store

**Priority:** Medium
**Component:** new crate `forge-checklist-state`, CLI subcommand `checklist`, MCP tool `checklist_state`

## Goal

`blueprint`, `compile-project`, and `performance-tuning` skills use multi-step checklists that reset each session. A small state store lets Claude pick up mid-workflow after a `/clear`, compaction, or fresh session.

## Storage

- Directory: `<project-root>/.forge/checklists/`
- Format: one JSON file per named checklist (e.g. `.forge/checklists/blueprint-init.json`)
- Auto-commit off (Claude can stage intentionally; state is intentionally cheap to inspect and edit)

## Schema

```json
{
  "name": "blueprint-init",
  "created": "2026-04-10T14:00:00Z",
  "updated": "2026-04-10T15:32:00Z",
  "source_skill": "blueprint",
  "items": [
    {
      "id": "phase-1-architect",
      "title": "Phase 1: Architect",
      "status": "completed",
      "completed_at": "2026-04-10T14:20:00Z",
      "notes": "specs/architecture/overview.md written"
    },
    {
      "id": "phase-2-dsm",
      "title": "Phase 2: DSM analysis",
      "status": "in_progress",
      "notes": "dependency graph extracted, writing report"
    },
    {
      "id": "phase-3-threat-model",
      "title": "Phase 3: Threat model",
      "status": "pending"
    }
  ]
}
```

## CLI subcommands

- `frg checklist create <name> --items item1,item2,item3` — initialize a new checklist
- `frg checklist list` — list all checklists in `.forge/checklists/`
- `frg checklist show <name>` — pretty-print a checklist
- `frg checklist set <name> <item-id> <status>` — update item status (`pending|in_progress|completed|blocked`)
- `frg checklist note <name> <item-id> "free text"` — attach a note
- `frg checklist delete <name>` — remove a checklist

## MCP tool

Single MCP tool `checklist_state` with a mode parameter: `create | list | show | set | note | delete`. Claude calls it to save or resume workflow state between turns.

## Output

All operations return the current state of the checklist as JSON (or the list of checklists for `list`).

## Dependencies

- `serde`, `serde_json`, `chrono`, `anyhow`, `forge-shared`.
- No database — flat JSON files, atomic writes via temp+rename.

## Test plan

- Round-trip: create → set → show → verify state.
- Concurrent-write safety: two writes to same checklist produce a consistent file (last-write-wins is fine).
- Missing checklist: `show` returns a clear error, `set` returns error.

## Out of scope (v1)

- Cross-project checklists (all state is project-local).
- Time-based reminders / deadlines.
- Hierarchy / sub-items.

## Skills that benefit

- `blueprint` — track 10-phase pipeline progress across sessions.
- `compile-project` — track RALPH task packet status.
- `performance-tuning` — track 7-phase methodology.
- Any multi-session refactor or migration.
