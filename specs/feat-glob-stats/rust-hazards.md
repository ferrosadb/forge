# feat-glob-stats — Rust Correctness Hazards

> Derived from `corpus/correctness-hazards/correctness-hazard-reference.md`.
> Scan before merge; CI must enforce P0/P1.

## P0 — Must not merge without

| ID | Hazard | Location risk | Guard |
|----|--------|---------------|-------|
| R-P0-1 | `.unwrap()` / `.expect()` on fallible fs ops | `glob.rs` traversal, line-counting | Forbid via `clippy::unwrap_used` at crate level; propagate via `?` into `skipped_reasons` tally |
| R-P0-2 | `panic!` on non-UTF-8 paths | any `path.to_str().unwrap()` | Replace with `to_string_lossy()` + `path_encoding` marker; grep gate in CI |
| R-P0-3 | Path traversal (F1 / T1) | pattern normalization | Centralize normalization; unit-tested; no ad-hoc string concat with user input |
| R-P0-4 | Secret denylist bypass (F7) | `--exclude` handling | Denylist applied **after** user excludes; test that `--exclude '*'` still hides `.env` |

## P1 — Should not merge without

| ID | Hazard | Guard |
|----|--------|-------|
| R-P1-1 | Unbounded memory on buffered output (F5) | Stream in `brief`/`csv`; enforce `--max-results` cap; add `truncated: true` marker |
| R-P1-2 | Integer overflow on line counts | Use `u64` throughout; saturating arithmetic where relevant; test with a synthetic file of `u32::MAX` newlines not required, but u64 math is |
| R-P1-3 | TOCTOU between `metadata()` and `open()` (F6) | Treat `NotFound` / `PermissionDenied` as skip, not fatal |
| R-P1-4 | Symlink following when disabled (F2) | `WalkBuilder::follow_links(false)` explicit; test that symlink is not descended |
| R-P1-5 | Clippy strict mode | `#![warn(clippy::pedantic)]` at crate root; CI `cargo clippy -- -D warnings` |
| R-P1-6 | Depth-limit regression | `WalkBuilder::max_depth(cfg.max_depth)`; unit test asserts deep tree halts at configured depth |

## P2 — Monitor

| ID | Hazard | Guard |
|----|--------|-------|
| R-P2-1 | Line-counting allocation per file | Use `bytecount::count(buf, b'\n')` over chunked reads; avoid collecting `Vec<String>` lines |
| R-P2-2 | Blocking I/O inside any async context | `glob` is sync; if MCP server is async, wrap in `spawn_blocking` |
| R-P2-3 | Locale-dependent sort | Explicit byte-wise sort for deterministic output; document |

## CI enforcement

```toml
# crates/cli/Cargo.toml  (or workspace-level)
[lints.clippy]
unwrap_used   = "deny"
expect_used   = "deny"
panic         = "deny"      # in glob.rs scope; escape-hatch via #[allow] with comment
indexing_slicing = "warn"
```

Grep gate in CI (covers the intent, not only the lint):

```bash
# must return 0 matches inside crates/cli/src/glob*.rs
rg -n '\.unwrap\(\)|\.expect\(|panic!\(' crates/cli/src/glob*.rs && exit 1 || exit 0
```
