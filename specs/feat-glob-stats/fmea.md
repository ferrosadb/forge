# feat-glob-stats — FMEA

> Scoring: Severity / Occurrence / Detection, each 1–10. RPN = S × O × D.
> Thresholds: RPN ≥ 200 → P1 work item; RPN 50–199 → test cases required; RPN < 50 → monitor only.

## Failure modes

| # | Failure mode | Cause | Effect | S | O | D | RPN | Mitigation | Test |
|---|--------------|-------|--------|---|---|---|-----|------------|------|
| F1 | Pattern escapes workspace root | User passes `../../**` or `/etc/**` | Lists files outside intended scope; agents ingest leaked data | 8 | 5 | 7 | **280** | Normalize + anchor patterns; reject `..` after normalization; gate absolute patterns behind `--allow-absolute` (see T1) | Unit: pattern normalization. Property: no normalized pattern contains `..` or absolute root unless flag set |
| F2 | Symlink loop / external link followed | `--follow` enabled, symlink points to `/` or forms a cycle | Infinite loop or data exfiltration | 7 | 3 | 6 | 126 | `--follow` default off; when on, canonicalize and prefix-check against root; cap traversal depth | Fixture: symlink loop → bounded exit. Fixture: external symlink → rejected |
| F3 | Huge file line-counting hangs | `--max-bytes` not checked before `read()` | Process hangs on a binary/generated file | 6 | 4 | 5 | 120 | Short-circuit on `metadata().len() > max_bytes` before any open | Fixture: 200MB file, verify skipped without opening |
| F4 | Non-UTF-8 path panics | `path.to_str().unwrap()` on macOS/Linux paths with invalid UTF-8 | Process crash mid-run | 8 | 2 | 4 | 64 | Use `to_string_lossy()` + `path_encoding: "lossy"` marker in output | Fixture: create path with `OsStr::from_bytes(&[0xFF])` (Unix), verify no panic |
| F5 | Memory blow-up on huge match count | JSON format buffers all results before write | OOM on repos with 100k+ files | 5 | 3 | 6 | 90 | Enforce `--max-results` (default 10k) with `truncated: true` marker; stream `brief`/`csv` | Load test: 50k synthetic files → memory bounded |
| F6 | TOCTOU: file deleted mid-traversal | Race between `metadata()` and `read()` for line count | `Err(NotFound)` bubbles up as crash | 4 | 6 | 5 | 120 | Treat `NotFound` / `PermissionDenied` as `skipped_reasons` bucket; never fail the whole run | Fixture: concurrent `rm` during walk → partial results + skip tally |
| F7 | Default excludes miss new secret pattern | User drops `*.env.local` or similar | Secrets surface in results | 9 | 4 | 8 | **288** | Hard-coded secret denylist sourced from `secret-scan` crate (single source of truth); cannot be overridden by `--exclude`; warning log when any `--exclude` passed | Unit: secret denylist cannot be removed via CLI. Integration: `.env` present → never listed |
| F8 | Glob crate mismatch with shell expectations | User expects bash `**` semantics but crate differs | Wrong files returned silently | 4 | 7 | 9 | 252 | Document exact semantics in `--help`; include `pattern` echo in JSON output; smoke-test parity suite against common shell patterns | Property tests over pattern corpus vs documented semantics |
| F9 | `is_generated` heuristic false-positive | Marks real source as generated (e.g., files with `@generated` in docstring) | Agent skips relevant files | 3 | 5 | 7 | 105 | Be conservative: only mark as generated on exact first-line `@generated` sentinel or `.generated.` in filename; document rule | Fixture: mixed files with sentinels → classification matches rule table |
| F10 | Platform-specific path separator leaks | Windows paths returned with `\`, downstream expects `/` | Broken composition with `digest` | 4 | 4 | 5 | 80 | Always emit POSIX-style separators in output; document | CI: Windows runner asserts `/` in all paths |
| F11 | CSV format breaks on commas / newlines in filenames | Manual CSV emission | Downstream parse error | 5 | 3 | 4 | 60 | Use `csv` crate quoting; never hand-roll | Fixture: filename with `,` and `"` → RFC 4180 compliant |
| F12 | ANSI escape in filename corrupts table output | Raw bytes in `table` format | Terminal corruption / command injection into next prompt | 6 | 2 | 6 | 72 | Strip / escape control chars in `table` renderer; document | Fixture: filename with `\x1b[31m` → sanitized |

## Priority summary

- **P1 (RPN ≥ 200):** F1 (pattern escape), F7 (secret denylist), F8 (glob semantics mismatch)
- **P2 (RPN 50–199):** F2, F3, F4, F5, F6, F9, F10, F11, F12
- **P3 (RPN < 50):** none

## Test-case generation rule

Every row with RPN ≥ 50 MUST have at least one corresponding test in `test-plan.md`. Verified by `rpn_coverage` check in CI (grep-based; see `test-plan.md`).
