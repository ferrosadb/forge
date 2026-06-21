# feat-glob-stats — Threat Model

> Methodology: STRIDE, scoped to this feature's delta against the current forge surface.
> Assumptions: forge is a local developer tool; threats are bounded by the privileges of the invoking user. Attack value is mostly **information disclosure** and **resource exhaustion**, not RCE.

## Trust boundaries

```text
    ┌──────────────────┐      pattern, flags      ┌───────────────────┐
    │ User / MCP caller│ ───────────────────────▶ │  frg glob         │
    └──────────────────┘                          │  (user-priv)      │
                                                  └─────────┬─────────┘
                                                            │ fs syscalls (read-only)
                                                            ▼
                                                  ┌───────────────────┐
                                                  │  Local filesystem │
                                                  └───────────────────┘
```

No new network boundary. No new persistent state. The sole new interaction is filesystem traversal bounded by process privileges.

## STRIDE delta

| # | Category | Threat | Likelihood | Impact | Mitigation |
|---|----------|--------|------------|--------|------------|
| T1 | **Information disclosure** | Pattern with `..` or absolute path escapes the workspace and lists files outside it (e.g., `/etc/**`) | Med | Med | Anchor patterns to `cwd` by default; reject absolute patterns unless `--allow-absolute` is passed; reject patterns containing `..` segments after normalization. |
| T2 | **Information disclosure** | Symlink traversal exposes files outside the workspace | Med | Med | `--follow` disabled by default; when enabled, abort on links pointing outside the traversal root (canonicalize + prefix check). |
| T3 | **Information disclosure** | Default excludes silently leak contents of `.env`, `*.pem`, `id_rsa` if user overrides with `--exclude ''` | Low | High | Maintain a hard-coded secret-pattern denylist that cannot be overridden from the CLI; match the list in `secret-scan` crate (DRY). Log a warning when `--exclude` is used. |
| T4 | **Denial of service (local)** | Pathological pattern (e.g., `**/**/**/**`) against a large tree exhausts CPU / file descriptors | Med | Low | Hard cap on traversal depth (configurable, default 20); hard cap on total match count before truncating (`--max-results`, default 10_000); stream output. |
| T5 | **Denial of service (local)** | Line-counting a 10GB file hangs the process | Low | Med | Skip files exceeding `--max-bytes` **before** opening; use `metadata()` only for the size check. |
| T6 | **Tampering** (output) | Filename with embedded newlines or ANSI escapes corrupts downstream CSV / table parsers | Low | Med | JSON escapes via serde; CSV quoting via `csv` crate; brief format must escape newlines (use `\n` literal) — emit a `path_needs_quoting: true` marker when a path contains control chars. |
| T7 | **Spoofing** (MCP) | Malicious MCP caller passes a pattern crafted to exhaust tokens by matching every file | Low | Low | Same caps as T4 apply in MCP mode; MCP tool definition must declare `readOnly: true` and `destructiveHint: false`. |
| T8 | **Repudiation** | N/A — read-only tool; no audit log needed beyond existing `shared::tracking` | — | — | — |
| T9 | **Elevation of privilege** | N/A — no privileged ops introduced | — | — | — |

## Risks carried (accepted)

- **R1:** A user with filesystem read access can already see these files via `ls`; this tool does not grant new capability. Risk is "agent reads more than the user intended" — mitigated by T1/T2 anchoring.
- **R2:** MCP mode inherits the host process's user privileges. Not a regression; documented in `hooks-integration.md`.

## Unresolved questions

1. Should `--follow` ever be supported, or is it permanent out-of-scope? (Current lean: permanent out-of-scope; revisit if a concrete user need appears.)
2. Do we want a `--respect-gitignore=false` escape hatch? If yes, treat as a privileged flag like `--allow-absolute`.
