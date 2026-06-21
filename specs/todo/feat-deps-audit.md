# Feat: deps_audit — Dependency lockfile audit for CVEs and suspicious versions

**Priority:** Medium
**Component:** new crate `forge-deps-audit`, CLI subcommand `deps-audit`, MCP tool `deps_audit`

## Goal

`pipeline-defense` currently hand-composes OSV queries. A lockfile parser + vulnerability lookup gives a deterministic first-pass.

## Input

- `path`: project directory (default: cwd)
- `--offline`: skip network lookups, only parse lockfiles and flag locally-known patterns (default: true in v1)
- `--min-severity`: `low`, `medium`, `high`, `critical` (default: medium)

## Supported lockfiles (v1)

| Ecosystem | File | Parser |
|---|---|---|
| Rust | `Cargo.lock` | TOML |
| Node | `package-lock.json` | JSON |
| Node | `pnpm-lock.yaml` | YAML (v2, use simple regex in v1) |
| Python | `requirements.txt`, `Pipfile.lock`, `poetry.lock` | regex / TOML |
| Elixir | `mix.lock` | regex (Erlang term syntax) |
| Go | `go.sum` | text |
| Ruby | `Gemfile.lock` | regex |

## Offline mode behavior (v1 default)

No network. Parses every lockfile it finds, produces a `PackageInventory` and applies **local heuristics only**:

- **Flagged version patterns** (embedded allowlist of known-bad versions):
  - `log4j-core < 2.17.1` → CRITICAL (CVE-2021-44228 Log4Shell)
  - `openssl < 1.1.1k` → HIGH
  - `node-ipc == 10.1.1` / `10.1.2` → CRITICAL (supply chain attack 2022)
  - `event-stream == 3.3.6` → CRITICAL
  - `colors == 1.4.44-liberty-2` → HIGH (2022 sabotage)
  - `ua-parser-js 0.7.29 / 0.8.0 / 1.0.0` → HIGH (supply chain)
- **Yanked / deprecated markers** where available from lockfile metadata.
- **Pinned to `*` / `latest` / a floating range inside a lockfile** — WARN (lockfile should pin).

The embedded list is compiled into the binary — small (~30 entries), reviewed quarterly.

## Online mode (`--online`)

Query OSV (`api.osv.dev`) for each package@version. Not in v1. Gated behind `--online` flag with network timeout.

## Output

```json
{
  "ecosystems": ["rust", "node", "python"],
  "files_parsed": ["Cargo.lock", "package-lock.json", "requirements.txt"],
  "package_count": 847,
  "findings": [
    {
      "ecosystem": "node",
      "package": "log4js",
      "version": "6.3.0",
      "severity": "high",
      "source": "embedded",
      "advisory": "Known vulnerable to GHSA-xxx-yyy",
      "recommendation": "upgrade to >= 6.4.0"
    }
  ],
  "summary": {"critical": 0, "high": 1, "medium": 0, "low": 0, "total_packages": 847}
}
```

## Dependencies

- `toml` (workspace) for Cargo.lock, poetry.lock, Pipfile.lock.
- `serde_json` for package-lock.json.
- `regex` for the rest.
- `forge-shared`.

## Test plan

- Fixture lockfiles for each ecosystem (small, 5-10 packages each).
- Planted vulnerable version in each ecosystem → expect finding.
- Clean lockfile → empty findings, nonzero package count.

## Out of scope (v1)

- Network queries (OSV, GitHub advisories) — gated behind `--online` in v2.
- License auditing — separate concern.
- Transitive path explanation — v2.

## Skills that benefit

- `pipeline-defense` — first-pass supply-chain check before CI audit.
- `secure-review` — dependency awareness during review.
