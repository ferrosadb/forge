# ferrosa-memory dependency note — current mutation surface for Forge

> **Target repo:** `ferrosa-memory`
> **Consumer:** `forge` code-graph ingest
> **Status:** current-state dependency note, not a promise of future CRUD tools
> **Updated:** 2026-04-22

## What exists today

The live `ferrosa-memory` MCP surface available to Forge is:

| Tool | Status | Use for code-graph ingest |
|---|---|---|
| `ingest_entities` | available | Primary bulk upsert path for entities + typed edges |
| `create_edge` | available | Single edge insert; useful for diagnostics or small repairs |
| `batch_create_edges` | available | Multi-edge insert; secondary path only |
| generic `update_entities` | **not available** | Do not assume PATCH support |
| generic `delete_entities` | **not available** | Do not assume hard delete support |
| generic `update_edges` | **not available** | Do not assume edge PATCH support |
| generic `delete_edges` | **not available** | Do not assume edge delete support |

The important boundary is:

- Forge should treat `ingest_entities` as the only supported bulk write contract.
- Forge should not write CQL directly.
- Forge should not assume a CRUD family that the server does not expose yet.

## `ingest_entities` contract Forge can rely on

`ingest_entities` is the server-owned batch write tool for semantic entities and typed edges.

### Request shape

```json
{
  "tenant_id": "UUID",
  "session_id": "UUID",
  "entities": [
    {
      "id": "UUID",
      "name": "string",
      "entity_type": "file|function|method|type|trait|parameter|...",
      "context": "string",
      "confidence": 0.9,
      "state": "active",
      "embedding": [0.123, ...],
      "attrs": { "...": "..." }
    }
  ],
  "edges": [
    {
      "src_id": "UUID",
      "dst_id": "UUID",
      "edge_type": "contains|defined_in|calls|references|has_type|implements|...",
      "weight": 1.0,
      "metadata": { "...": "..." }
    }
  ],
  "options": {
    "embed_missing": false,
    "embedding_model": "nomic-embed-text-v2-moe",
    "on_conflict": "update",
    "strict_edges": true,
    "dry_run": false
  }
}
```

### Response shape

```json
{
  "entities": {
    "inserted": 12,
    "updated": 3,
    "skipped": 0,
    "failed": []
  },
  "edges": {
    "inserted": 40,
    "skipped_duplicate": 0,
    "failed": []
  },
  "embeddings": {
    "computed": 0,
    "received": 12,
    "failed": []
  },
  "schema_version": "2026-03-01",
  "duration_ms": 1234
}
```

### Invariants Forge should assert

1. No silent drop:
   - `failed[]` is authoritative.
   - If Forge sends `N` entities and the response does not reconcile to `inserted + updated + skipped + len(failed) == N`, that is a server/client bug and the run should fail loud.
2. `strict_edges: true` means every edge endpoint must either:
   - be present in the same batch, or
   - already exist in the same `(tenant_id, session_id)`.
3. `on_conflict: "update"` is idempotent bulk upsert, not partial patch:
   - Forge must send the desired full semantic row for changed entities.
4. Tenant isolation is enforced server-side:
   - `tenant_id` mismatch should fail the call, not quietly cross-write.

## What Forge should do now

### Full ingest

Use `ingest_entities` only.

- Batch all file and symbol entities first.
- Submit edges only after the corresponding entity ids have already been acknowledged.
- Split by payload size, but preserve topological ordering.

### Incremental refresh for changed files

Still use `ingest_entities`.

- For changed files and changed symbols, Forge should re-emit the full desired current row using the same stable ids.
- For changed edges, Forge should re-emit the desired edge set using `ingest_entities`.
- If a small repair path is useful, `create_edge` / `batch_create_edges` can be used as operational helpers, but they are not the primary ingest contract.

### Deleted files / deleted symbols

This is the current gap.

Because generic delete tools are not available, Forge should **not** model refresh around hard delete yet.

Current safe position:

- treat add/update as supported
- treat delete as a follow-on dependency
- if the feature needs soft-deletion before delete tools exist, model that as a separate explicit design:
  - Forge reads the resident row,
  - rewrites it through `ingest_entities` with a terminal `state`,
  - and excludes terminal states from normal graph traversal

That soft-delete flow is **not** standardized yet, so this feature should not silently invent it.

## Consequences for the code-graph feature

### Safe assumptions

- `ingest_entities` exists and works.
- Bulk upsert is the supported server boundary.
- Typed edge creation is supported through that bulk path.

### Unsafe assumptions

- `update_entities` exists.
- `delete_entities` exists.
- `update_edges` exists.
- `delete_edges` exists.

Any plan item that depends on those generic CRUD tools should be marked as:

- deferred
- soft-delete fallback
- or upstream dependency

but not treated as already available.

## Recommended upstream backlog

If Forge later needs a true mutation family, the next server-side additions should be:

1. `update_entities`
   - batch PATCH for attrs/state/metadata without requiring Forge to resend the full row
2. `delete_entities`
   - batch delete with explicit edge cascade policy
3. `delete_edges`
   - delete by `(src_id, edge_type, dst_id)` composite key

`update_edges` is lower priority than `delete_edges`; most refresh flows can model edge change as delete + reinsert.

## Guidance for the current project plan

For `feat-code-graph-ingest`, this dependency note means:

- T2 remains valid: move code ingest to `ingest_entities`
- T10 remains valid: topological batching for `ingest_entities`
- refresh add/update remains valid
- delete-oriented refresh work must not assume a generic CRUD surface that does not exist

In short:

- **bulk upsert is real**
- **generic CRUD is not**
- Forge should ship the code-graph ingest around that reality instead of planning against a future API
