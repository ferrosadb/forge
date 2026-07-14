---
name: sheet-sync
description: Use when driving development from a Google Sheet of bugs — pull open rows into the forge task board, work them, and push fix status/version/notes back to the sheet without clobbering the owner's cells.
---

# sheet-sync

Two-way sync between a client-owned Google Sheet of bugs and the forge task
board. The sheet owns bug content and triage (Title, Description, Severity,
Priority, MVP flag, early/terminal Status); the board owns dev progress
(middle Status values, our notes). Never write outside that split.

## One-time setup

1. Copy `crates/sheet-sync/examples/spoton-qa.toml` (or author your own) to
   `.forge/sheets/<alias>.toml` — this path is git-ignored, so it's local
   config, not a commit target.
2. Edit `spreadsheet_id`, `tab`, and the `[columns]` map (sheet header string
   → canonical field) to match the client's actual sheet. Keep `writable`,
   `dev_writable_status`, and `terminal_status` ordered *before*
   `[columns]`/`[status_map]` — TOML nests a bare array under whatever table
   most recently opened, so putting it after breaks the file silently.
3. Set `FORGE_GOOGLE_OAUTH_CLIENT` to the path of a Google **Desktop** OAuth
   `client_secret.json`.
4. Run `frg sheet auth <alias>` once. This opens a browser consent flow and
   caches a refresh token — one-time per alias/machine.

## Pull

```
frg sheet pull <alias> --dry-run   # preview first
frg sheet pull <alias>
```

- Upserts every **open** (non-`terminal_status`) row into the forge task
  board, keyed by the sheet's row id.
- Idempotent: re-running never duplicates rows and never moves a task
  **backward** — if the board task is already in a dev-owned state, pull
  refreshes content/priority but leaves status alone.
- **Duplicate ids in the sheet are refused, not merged.** If two rows share
  an id (e.g. two `QA-005`), pull reports both and skips them for import
  *and* future push. That needs a human to clean up the sheet — do not try
  to resolve it programmatically.

## Work a bug

Pick an open task (`frg task-board` / `mcp__forge__task_*`), fix it TDD-style
as usual.

## Push status back

```
frg sheet push <alias> <row_id> --status "<sheet status value>" \
    --fix-ver "<value>" --notes "<what was done>" --dry-run
```

Always `--dry-run` first — it prints the exact `A1:cell → old ⇒ new` diff and
writes nothing. Drop `--dry-run` once the diff looks right.

- **Status handoff**: you may only advance the sheet's Status to a
  dev-owned value — `In Progress`, `In Review`, `Fixed - Needs
  Verification`. The sync refuses to overwrite client-owned/terminal states
  (`New`, `Triaged`, `Verified/Closed`, `Won't Fix`, `Duplicate`); if the new
  value isn't dev-writable, or the sheet is already in a client-owned/
  terminal state, `status` is silently skipped but `fix_ver`/`notes` still
  write.
- **`fix_ver` value**: on a **demo** fix, set it to the git build hash of the
  PR that carries the fix. When that fix is promoted to **prod**, run push
  again to update `fix_ver` to the prod version tag.
- `fix_ver` and `resolution_notes` are always written regardless of status
  handoff.

## Safety

- Push only ever writes the three configured `writable` columns (status,
  fix_ver, resolution_notes), each located by **header name** at write time
  — everything else in the client's sheet is read-only from this tool.
- It never deletes rows.
- Missing header → hard error, no partial write.
