# Feat: mermaid_validate — Lint Mermaid diagrams before writing

**Priority:** Medium
**Component:** new crate `forge-mermaid-validate`, CLI subcommand `mermaid-validate`, MCP tool `mermaid_validate`

## Goal

`architect`, `threat-model`, `blueprint`, and `dsm-analysis` skills generate Mermaid diagrams and occasionally emit syntactically broken ones (unmatched brackets, unknown diagram types, bad edge syntax). A validator gives them a deterministic "is this renderable?" check before writing.

## Input

Text input via stdin (`register_stdin_tool!`) or via `input` MCP argument.

## Checks (v1)

1. **Diagram type recognized** — first non-empty line must be one of: `graph`, `flowchart`, `sequenceDiagram`, `stateDiagram`, `stateDiagram-v2`, `classDiagram`, `erDiagram`, `gantt`, `pie`, `journey`, `gitGraph`, `mindmap`, `timeline`, `quadrantChart`, `requirementDiagram`, `C4Context`, `C4Container`, `C4Component`, `C4Dynamic`.
2. **Balanced brackets** — `()`, `[]`, `{}`, `<<>>`, `[[]]` must balance. Report line of first unbalanced bracket.
3. **Edge syntax** (flowchart/graph) — `A --> B`, `A -- label --> B`, `A -.-> B`, `A === B`, `A ==> B`. Flag `A->B` without space or `A ---> B` with invalid arrow length.
4. **Node ID charset** — must match `[A-Za-z_][A-Za-z0-9_]*` (plus quoted form `"node label"`).
5. **Sequence diagram participants** — `participant A`, `actor B`; messages must reference declared participants.
6. **Unterminated lines** — a line ending with `-->` or `---` without a target.
7. **Code fence not included** — warn if input starts with ` ```mermaid ` (should pass raw content, not the fenced block).

## Output

```json
{
  "valid": false,
  "diagram_type": "flowchart",
  "errors": [
    {"line": 3, "column": 12, "code": "UNBALANCED_BRACKET", "message": "unmatched '['"},
    {"line": 7, "code": "UNKNOWN_PARTICIPANT", "message": "message references undeclared 'C'"}
  ],
  "warnings": [
    {"line": 1, "code": "FENCE_INCLUDED", "message": "input appears to include ```mermaid fence"}
  ]
}
```

`valid: true` iff `errors` is empty.

## Implementation notes

- Pure state machine + regex. No mermaid library dependency (the reference implementation is JavaScript; embedding V8 is overkill).
- First line parse → diagram type → dispatch to per-type checker.
- Shared bracket balance routine.

## Dependencies

- `regex`, `serde`, `serde_json`, `forge-shared`.

## Test plan

- Positive fixtures: one minimal valid diagram per supported type.
- Negative fixtures: each error code exercised.
- Regression: broken diagrams harvested from past skill outputs.

## Skills that benefit

- `architect` — validate Mermaid before writing spec files.
- `threat-model` — STRIDE trust-boundary diagrams.
- `blueprint` — all phase diagrams.
- `dsm-analysis` — module DSM rendering.
