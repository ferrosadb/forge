# Bug: Author Name Format Inconsistency Prevents Dedup

**Severity:** Medium
**Component:** frg ingest-paper (crates/ingest/src/paper.rs)

## Issue

`ingest-paper` produces different author name formats for different papers:
- Paper 1 (MaKD): `Kaiyu Huang` (first-last)
- Paper 2 (JoyAI-LLM Flash): `Huang, Kaiyu` (last-first comma)

When two papers share an author, `smart_ingest` can't match them because the names are different strings. The phonetic search also fails because the token order differs.

## Evidence

Kaiyu Huang appears in both papers:
- MaKD entity: name = `Kaiyu Huang` (lost to data loss, but was ingested)
- JoyAI entity: name = `Huang, Kaiyu` (entity_id = d1aec5e2)

Same for Jinan Xu:
- MaKD: `Jinan Xu`
- JoyAI: `Xu, Jinan` (entity_id = afbd4191)

These should merge into one person entity per author.

## Expected Behavior

All author names should be normalized to a consistent format (preferably `First Last`) before ingestion. The name normalization should handle:
- `Last, First` → `First Last`
- `First Last` → `First Last` (no change)
- `F. Last` → `F. Last` (preserve initials)

## Fix

In the paper extraction pipeline, normalize author names before calling `smart_ingest`:
```rust
fn normalize_author_name(name: &str) -> String {
    if let Some((last, first)) = name.split_once(", ") {
        format!("{} {}", first.trim(), last.trim())
    } else {
        name.to_string()
    }
}
```
