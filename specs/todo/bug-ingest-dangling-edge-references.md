# Bug: Ingest Creates Edges with Dangling Entity References

**Severity:** Critical
**Component:** frg ingest (crates/ingest/)
**Version:** v0.6.1

## Issue

`frg ingest --cql` creates typed edges where the majority of endpoints reference entities that don't exist in `entity_store`. This affects BOTH src_id and dst_id — not just destinations. The problem is worse on larger codebases.

## Evidence

### Multi-codebase ingest (v0.6.1, 2026-04-05)

3 codebases ingested sequentially into the same cluster:
- ferrosa-memory: 2,865 entities, 6,298 edges
- ferrosa: 11,029 entities, 60,247 edges
- ferrosa-dbaas: 2,015 entities, 3,572 edges

**Post-ingest verification:**
```
Entities in entity_store: 2,481
Valid edges in typed_edges: 19,130
Both endpoints in entity_store: 3,354 (17.5%)
Src only: 718
Dst only: 361
Neither endpoint exists: 14,697 (76.8%)
```

76.8% of edges reference entities that don't exist AT ALL. The entity_store has 2,481 entities but the ingests reported inserting 15,909 total. **13,428 entities were lost.**

### Single codebase ingest (earlier test, same session)
```
Entity IDs in store: 5,556
Valid edges: 6,183
Both in entities: 629 (10.2%)
```

## Root Cause Hypotheses

1. **Entity dedup collision**: When ingesting multiple codebases with `smart_ingest`, UPDATEs may overwrite entities from prior ingests with the same name, changing the entity_id. Edges created with the old entity_id become dangling.

2. **UUIDv5 namespace mismatch**: Entity IDs generated for edges use a different UUIDv5 namespace than the entities themselves, so the IDs never match.

3. **Batch write loss**: CQL writes for entities succeed in batches but some are lost due to ferrosa cluster replication issues (we've seen data loss bugs in this session).

4. **Entity count mismatch**: Ingests report 15,909 entities inserted but only 2,481 survive in entity_store. Either the INSERT reports success without durably writing, or subsequent ingests overwrite prior data.

## Impact

- Viz shows only 3,351 edges out of 19,130 (17.5%)
- Graph traversal (`explore_connections`) misses most relationships
- Knowledge graph is structurally incomplete — entities exist as islands
- The more codebases ingested, the worse the ratio gets

## Expected Behavior

Every edge endpoint must reference an entity that exists in entity_store. After ingesting N codebases:
- All entities from all ingests should persist
- All edges should have both endpoints in entity_store
- Match rate should be >99%

## Reproduction

```bash
frg ingest --cql localhost:19042 /path/to/codebase-a
frg ingest --cql localhost:19042 /path/to/codebase-b
# Check: SELECT count(*) FROM entity_store → much less than sum of reported inserts
# Check: cross-reference typed_edges endpoints → <20% match rate
```
