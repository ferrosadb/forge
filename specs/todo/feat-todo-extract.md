# Feat: todo_extract — Structured TODO/FIXME/HACK inventory

**Priority:** Medium
**Component:** new crate `forge-todo-extract`, CLI subcommand `todo-extract`, MCP tool `todo_extract`

## Goal

`code-audit`, `complexity-audit`, and `refactor` skills all need an inventory of debt comments in the codebase, currently gathered via ad-hoc `grep -n TODO`. Produce a structured, deduplicated, blame-attributed catalog.

## Input

- `path`: directory to scan (default: cwd)
- `--blame`: attach `git blame` author + commit SHA per finding (default: true)
- `--kinds`: comma-separated subset of `TODO,FIXME,HACK,XXX,BUG,NOTE,OPTIMIZE,DEPRECATED` (default: all)

## Patterns

Match tokens that appear as their own word inside a comment:

- Line comments: `// TODO:`, `# FIXME`, `-- HACK`, `; XXX`, `<!-- BUG -->`.
- Block comment openers: `/* TODO:`, `(* TODO`.

Case-insensitive, but the token itself must be uppercase (`todo` in prose doesn't match).

## Output

```json
{
  "path": ".",
  "files_scanned": 342,
  "findings": [
    {
      "kind": "FIXME",
      "file": "src/parse.rs",
      "line": 128,
      "text": "handle escape characters in raw strings",
      "full_line": "    // FIXME: handle escape characters in raw strings",
      "author": "ben@example.com",
      "commit": "a1b2c3d",
      "age_days": 412
    }
  ],
  "summary": {"TODO": 47, "FIXME": 12, "HACK": 5, "XXX": 2},
  "oldest_days": 812,
  "staleness": {
    "0-30": 8,
    "31-90": 14,
    "91-365": 32,
    "365+": 12
  }
}
```

## Prioritization hints

- `FIXME` and `HACK` older than 180 days → P1.
- `TODO` older than 365 days → P2.
- `XXX` and `BUG` always P1.
- `NOTE`/`DEPRECATED` informational only (not in default output unless `--include-notes`).

## Dependencies

- `ignore` (walk + gitignore)
- `regex`
- `std::process::Command` for `git blame` (shell out, no git2 crate)
- `chrono` (age computation)
- `forge-shared`

## Test plan

- Fixture directory with planted markers in Rust, Python, JS, Elixir, SQL.
- Blame test on a temp git repo with two commits.
- Language coverage: verify comment-style detection for each supported lang.
- Summary counts and staleness buckets.

## Out of scope (v1)

- JIRA/GitHub issue correlation (requires network).
- Author notification/assignment.
- Owner resolution beyond git blame.
