# Feat: secret_scan — Scan for API keys, credentials, private keys

**Priority:** High
**Component:** new crate `forge-secret-scan`, CLI subcommand `secret-scan`, MCP tool `secret_scan`

## Goal

Replace the hand-rolled `grep -E 'AKIA|BEGIN RSA'` patterns used today by `secure-review`, `pipeline-defense`, and `cloud-audit` skills with a single deterministic tool that produces structured findings.

## Input

- `path`: file or directory to scan (default: cwd)
- `--min-entropy`: Shannon entropy threshold for generic high-entropy strings (default: 4.5)
- `--include-entropy`: enable generic high-entropy detection (default: off, too noisy)

## Detection patterns (v1)

| ID | Pattern | Label |
|---|---|---|
| AWS_ACCESS_KEY | `AKIA[0-9A-Z]{16}` | AWS access key ID |
| AWS_SECRET | `(?i)aws.{0,20}['\"][0-9a-zA-Z/+]{40}['\"]` | AWS secret access key |
| GCP_KEY | `AIza[0-9A-Za-z_\-]{35}` | Google API key |
| GCP_OAUTH | `[0-9]+-[0-9A-Za-z_]{32}\.apps\.googleusercontent\.com` | GCP OAuth client |
| GITHUB_PAT | `ghp_[0-9A-Za-z]{36}` | GitHub personal access token |
| GITHUB_OAUTH | `gho_[0-9A-Za-z]{36}` | GitHub OAuth token |
| SLACK_TOKEN | `xox[baprs]-[0-9A-Za-z\-]{10,48}` | Slack token |
| STRIPE_KEY | `sk_(live\|test)_[0-9A-Za-z]{24,}` | Stripe secret key |
| PRIVATE_KEY | `-----BEGIN (RSA\|DSA\|EC\|OPENSSH\|PGP) PRIVATE KEY-----` | Private key header |
| JWT | `eyJ[A-Za-z0-9_\-]+\.eyJ[A-Za-z0-9_\-]+\.[A-Za-z0-9_\-]+` | JWT (context-dependent) |
| GENERIC_PASSWORD | `(?i)(password\|passwd\|secret)\s*[:=]\s*['\"][^'\"]{6,}['\"]` | Password assignment |

## Output

```json
{
  "files_scanned": 342,
  "findings": [
    {
      "id": "AWS_ACCESS_KEY",
      "severity": "critical",
      "file": "config/prod.env",
      "line": 14,
      "snippet": "AWS_ACCESS_KEY_ID=AKIA****************",
      "label": "AWS access key ID"
    }
  ],
  "summary": {
    "critical": 1,
    "high": 0,
    "medium": 0
  }
}
```

Snippets redact the matched secret. Severity = critical for private keys and AWS/GCP root credentials; high for GitHub/Slack/Stripe/JWT; medium for generic password assignments.

## Dependencies

- `regex` (workspace)
- `ignore` (respect `.gitignore`, skip binary files)
- Uses `forge_shared` patterns for JSON emission and file walking.

## Test plan

- Unit tests for each pattern (positive + negative cases).
- Integration test scanning a temp dir with 5 planted secrets of different types; assert count and IDs.
- False-positive tests: UUIDs, git SHAs, Base64-encoded images should NOT match.

## Out of scope (future)

- Entropy-based generic secret detection (v2, gated by `--include-entropy`).
- Custom pattern files.
- Git history scan (`--since HEAD~100`).

## Skills that benefit

- `secure-review` — first-pass secret inventory before OWASP review.
- `pipeline-defense` — CI config + build artifact scan.
- `cloud-audit` — Terraform/CloudFormation file scan before deploy review.
