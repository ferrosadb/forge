# feat: `frg fmem-skill-ingest` — seed ferrosa-memory skill catalog from `../research/skills`

**Status:** todo
**Consumer:** ferrosa-memory (see `../../../../ferrosa-memory/specs/skills-layer-design.md`)
**Created:** 2026-04-16

## Goal

Walk the `research/skills/**/SKILL.md` catalog and ingest each skill into ferrosa-memory via its `ingest_skill` MCP tool, with idempotent update semantics so the command can be run after every skill edit.

This is the bridge between the research repo's markdown skill catalog and fmem's richer entity model. It replaces manual `smart_ingest` calls for skill-shaped knowledge.

## Inputs

- **Source:** `$FORGE_SKILL_CATALOG_DIR` — 78 SKILL.md files organized as `{category}/{skill-name}/SKILL.md` plus supplementary files.
- **Target:** a running ferrosa-memory MCP server (stdio or HTTP).
- **Auth:** existing forge → fmem MCP client wiring (already used by `frg ingest`).

## Skill file format

Each `SKILL.md` has YAML frontmatter + markdown body:

```yaml
---
name: tdd
description: Guides red-green-refactor TDD cycles based on Kent Beck's methodology...
argument-hint: <feature-description>
supplementary-files:
  - tdd-strategies.md
---
# Skill body in markdown...
```

Category is the parent directory name: `task-level`, `tech`, `quality`, etc.

## Command interface

```
frg fmem-skill-ingest [OPTIONS]

Options:
  --root <PATH>        Root dir containing skill categories (default: ../research/skills)
  --filter <GLOB>      Only ingest matching skill names (default: all)
  --dry-run            Parse and validate, don't call fmem
  --session <UUID>     Override fmem session_id (default: configured default)
  --force              Re-ingest even if content_hash matches (default: skip unchanged)
  --server <URL|stdio> fmem endpoint (default: stdio from env)
  --verbose            Log per-skill diff
```

Exit codes:
- 0 — all skills ingested successfully (created/updated/skipped)
- 1 — one or more parse errors or ingest failures
- 2 — fmem server unreachable

## Taxonomy seed (one-time + idempotent)

Before ingesting any skills, forge builds the tag taxonomy in fmem so skill ingestion can link to it:

1. Walk the directory structure of `--root` (default `../research/skills/`). Each first-level directory is a top-level tag (`communication`, `leadership`, `management`, `quality`, `rules`, `task-level`, `tech`).
2. For each top-level directory, call fmem's `smart_ingest` with `entity_type="tag"`, name = directory name, scope=Global.
3. If a `tag-hierarchy.yaml` file is present at the root, use it to create `PARENT_TAG` edges between tags (e.g., `tdd PARENT_TAG testing`, `testing PARENT_TAG quality`). Absent that file, the taxonomy stays flat — just the top-level tags — and humans can add hierarchy edges later via manual MCP calls.
4. Tag ingestion is idempotent via `content_hash`.

The taxonomy seed runs first; skill ingestion uses the tag names for `TAGGED_AS` edges.

## Parsing rules

For each `SKILL.md`:

1. **Frontmatter required.** Must contain `name` and `description`. Missing either → skip with warning (exit 1 at end).
2. **Category.** Derived from path: `skills/{category}/{skill-name}/SKILL.md` → `category`.
3. **Trigger keywords.** Extract from description text plus any explicit `keywords:` in frontmatter. Heuristic: split description on whitespace, lowercase, drop stopwords, dedupe. If frontmatter has `keywords:`, use that directly.
4. **Steps.** Parse numbered/bulleted lists under `## Instructions`, `## Steps`, or `### Step N:` headings. Each step becomes `{phase: heading or "Step N", instruction: body text}`.
5. **Prerequisites.** Parse from frontmatter `prerequisites:` field if present (list of skill names). Otherwise empty.
6. **Related concepts.** Parse from frontmatter `related:` or detect mentions of other skill names in the body. Conservative — only include names that match an existing skill in the catalog.
7. **Output artifacts.** Parse from frontmatter `output_artifacts:` if present. Else infer from "## Output" / "## Artifacts" section.
8. **Supplementary files.** Listed in frontmatter. Concatenated into description context but not ingested as separate entities.

## Content hash

For each skill, compute `sha256(frontmatter_yaml || body_markdown || supplementary_files_content)`. Pass to fmem's `ingest_skill(content_hash=...)`. If fmem returns `action: Skipped` (hash unchanged), log and move on.

## Tag resolution per skill

For each skill:

1. The skill's `category` (derived from its directory) becomes its primary tag.
2. If the skill's frontmatter has a `tags:` list, those become additional tags.
3. For each tag name, call `smart_ingest(entity_type="tag", name=tag)` — idempotent, creates if missing.
4. Pass the list of tag names to `ingest_skill`; fmem creates the `TAGGED_AS` edges and materializes the denormalized `tags` column (including ancestors).

## fmem call shape

```json
{
  "name": "ingest_skill",
  "arguments": {
    "session_id": "<configured or --session override>",
    "name": "tdd",
    "category": "task-level",
    "description": "Guides red-green-refactor TDD cycles...",
    "trigger_keywords": ["tdd", "red-green-refactor", "kent", "beck"],
    "prerequisites": [],
    "related_concepts": ["refactor", "debug"],
    "steps": [
      {"phase": "Step 1: Build the Test List", "instruction": "..."},
      {"phase": "Step 2: Pick the Simplest Test", "instruction": "..."}
    ],
    "output_artifacts": ["checklist"],
    "content_hash": "sha256:deadbeef..."
  }
}
```

## Error handling

- **Malformed frontmatter.** Log, skip, continue. Exit 1 at end with count.
- **Unknown prerequisite.** If a prereq name doesn't exist in the catalog or in fmem, log a warning and ingest without it — user can re-run after ingesting the prerequisite.
- **fmem rejection.** Log the full error (including tool response text). Don't retry silently — fail loud.
- **Duplicate skill names across categories.** Disallowed. Exit 1 with "skill name collision" error listing all paths.

## Idempotency and dry-run

- Dry-run parses every file, validates the parsed structure against an in-memory copy of fmem's skill JSON schema (mirrored from fmem), and prints what would be created/updated/skipped.
- Real runs are safe to repeat; unchanged skills skip via content_hash.

## Observability

- Summary at end:
  ```
  Ingested 78 skills: 12 created, 5 updated, 61 skipped (unchanged), 0 failed.
  Duration: 4.3s.
  ```
- `--verbose` prints per-skill action with a truncated diff for updates.
- Structured logs at `info` level for each skill action; `warn` for parse failures; `error` for ingest failures.

## Relationship to fmem work

This command depends on fmem implementing `ingest_skill` (Sprint 2 of the skills-layer design). The forge side can be built in parallel against a mock fmem server, then wired up when fmem Sprint 2 lands.

## Out of scope

- Skill execution (`invoke_skill` is fmem-side).
- Skill editing UI.
- Pushing skill changes back to `../research` (one-way sync: research → fmem).
- Automatic `frg fmem-skill-ingest` on file change (that's a file watcher, separate work item).

## Acceptance criteria

- [ ] `frg fmem-skill-ingest --dry-run` parses all 78 SKILL.md files and prints a validation summary without calling fmem.
- [ ] `frg fmem-skill-ingest` against a running fmem server creates 78 skill entities on first run.
- [ ] Second run (no edits) reports 78 skipped via content_hash.
- [ ] Edit one skill file, re-run → reports 1 updated, 77 skipped.
- [ ] `frg fmem-skill-ingest --filter "tdd"` only ingests TDD skill.
- [ ] Malformed frontmatter in one file reports a warning, doesn't abort the batch, exits 1.
- [ ] fmem `retrieve_skills_for_context("how do I test this?")` returns the TDD skill as a top result.

## References

- `../../../../ferrosa-memory/specs/skills-layer-design.md` — consumer spec.
- `$FORGE_SKILL_CATALOG_DIR` — source catalog.
- `frg ingest` — existing pattern for forge → fmem MCP calls.
