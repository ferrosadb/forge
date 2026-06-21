# Bug: UUID Collision for Common Function Names Across Crates

**Severity:** Medium
**Component:** frg ingest

## Issue

`frg ingest` generates entity IDs using UUIDv5 from entity names. Common names like `new`, `from`, `default`, `fmt` appear in every crate. Without the crate prefix in the UUID seed, different crates' `new()` functions get different UUIDs (because the content differs) but the entity NAME is identical, causing false dedup matches and visual confusion.

## Evidence

```
new: 22 duplicates across crates
default: 7 duplicates
fmt: 5 duplicates
into_response: 4 duplicates
```

## Expected Behavior

Entity names should be fully qualified: `ferrosa-cluster::coordinator::new` not just `new`. The UUID should be seeded from the fully qualified name so entities are unique per crate.

## Impact

- Viz shows false connections between unrelated crates via shared function names
- Search returns wrong `new()` function
- Entity count inflated by duplicates
