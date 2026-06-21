# Feat: threat_scan — Pattern scan for STRIDE attack vectors

**Priority:** High
**Component:** new crate `forge-threat-scan`, CLI subcommand `threat-scan`, MCP tool `threat_scan`

## Goal

`threat-model` and `secure-review` skills currently reason about STRIDE categories from first principles. A pattern-based scan gives them a first-pass target list — the same way `concurrency_scan` does for distributed-systems issues.

## Input

- `path`: directory to scan (default: cwd)
- `--categories`: comma-separated subset of `spoofing,tampering,repudiation,info_disclosure,dos,elevation` (default: all)
- `--min-confidence`: `low`, `medium`, `high` (default: medium)

## Detection catalog (v1)

### Spoofing
| ID | Pattern | Languages |
|---|---|---|
| SPOOF-001 | HTTP route handler without auth middleware within 10 lines | Python (Flask/FastAPI), Node (Express), Rust (axum/actix) |
| SPOOF-002 | JWT decode without signature verification (`jwt.decode(...)` without `verify=True`) | Python, Node |
| SPOOF-003 | Password compare via `==` or `.eq()` instead of constant-time | Rust, Python, Go |

### Tampering
| ID | Pattern | Languages |
|---|---|---|
| TAMPER-001 | SQL string concatenation (`"SELECT ... " + user_input`) | All |
| TAMPER-002 | `eval(` with user input | Python, JS |
| TAMPER-003 | `shell=True` or `os.system(` with user input | Python |
| TAMPER-004 | Mass assignment — deserialize into DB model without allowlist | Rails, Django, ActiveRecord patterns |

### Repudiation
| ID | Pattern | Languages |
|---|---|---|
| REPUD-001 | Mutation endpoint (POST/PUT/DELETE handler) without log/audit statement within 20 lines | All |

### Information Disclosure
| ID | Pattern | Languages |
|---|---|---|
| INFO-001 | Stack trace rendered to HTTP response (`.to_string()` of error in response body) | All |
| INFO-002 | `.env` file committed to git (check `.gitignore`) | All |
| INFO-003 | `println!` / `console.log` / `print` of variables named `password`, `token`, `secret`, `key` | All |
| INFO-004 | CORS `Access-Control-Allow-Origin: *` with credentials | All |

### Denial of Service
| ID | Pattern | Languages |
|---|---|---|
| DOS-001 | Unbounded loop reading user input | All |
| DOS-002 | Regex with catastrophic backtracking (`(a+)+`) | All |
| DOS-003 | HTTP handler without timeout config | Rust axum, Python |
| DOS-004 | HTTP handler without rate-limit middleware | Node, Rust, Python |

### Elevation of Privilege
| ID | Pattern | Languages |
|---|---|---|
| ELEV-001 | Role check via string equality (`role == 'admin'`) — case-sensitive, no enum | All |
| ELEV-002 | `setuid` / `chmod 777` / `sudo` in scripts | Shell, Dockerfile |
| ELEV-003 | Deserialization of untrusted data (`pickle.loads`, `yaml.load` without safe loader) | Python |

## Output

Matches `concurrency_scan` format:

```json
{
  "files_scanned": 342,
  "findings": [
    {
      "id": "TAMPER-001",
      "category": "tampering",
      "severity": "high",
      "confidence": "high",
      "file": "src/db.py",
      "line": 45,
      "snippet": "    query = 'SELECT * FROM users WHERE id = ' + user_id",
      "recommendation": "Use parameterized query."
    }
  ],
  "summary_by_category": {"tampering": 1},
  "summary_by_severity": {"high": 1}
}
```

## Implementation notes

- Mirror `forge-concurrency-scan/src/scanner.rs` structure — per-category `Rule { id, pattern, languages, severity, confidence, recommendation }`.
- Language detection via file extension.
- Confidence calibration: patterns with many false positives (REPUD-001, DOS-003) default to `medium`; high-signal patterns (TAMPER-002, ELEV-003) default to `high`.

## Dependencies

- `regex`, `ignore`, `serde`, `forge-shared`.
- Can share walk logic with `forge-concurrency-scan`.

## Test plan

- Per-rule positive/negative fixtures.
- False-positive suite: real-world snippets that look like patterns but shouldn't match (`role_id == admin_id` is not ELEV-001).
- Confidence calibration test: high-confidence rules must have 0% FP rate on fixture corpus.
