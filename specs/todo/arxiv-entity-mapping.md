# Arxiv Entity Mapping — Preserve External IDs

## Goal

When ingesting papers from arxiv, preserve the external identifiers so entities can be linked back to arxiv and deduplicated across papers using stable IDs rather than fuzzy name matching.

## Entity ID Strategy

### Papers
- **Primary ID**: arxiv ID (e.g., `2604.03110`)
- **UUID**: `uuid5(ARXIV_NS, "2604.03110")` — deterministic, same paper always gets same UUID
- **URI**: `https://arxiv.org/abs/2604.03110`
- **Store as**: `source_url` annotation on the entity

### Authors  
- **Primary ID**: Semantic Scholar author ID (numeric, e.g., `12345678`)
- **Fallback**: normalized name from arxiv author search URL
- **UUID**: `uuid5(AUTHOR_NS, semantic_scholar_id)` or `uuid5(AUTHOR_NS, normalized_name)`
- **URI**: `https://api.semanticscholar.org/graph/v1/author/{authorId}`

### Name Normalization Pipeline
```
arxiv HTML → extract author name → normalize to "First Last"
           → query Semantic Scholar by name + paper title
           → get stable authorId
           → use authorId as UUID seed
```

If Semantic Scholar lookup fails, fall back to normalized name.

## Extraction from Arxiv HTML

The arxiv abstract page includes:
```html
<div class="authors">
  <a href="/search/?searchtype=author&query=Kaiyu+Huang">Kaiyu Huang</a>,
  <a href="/search/?searchtype=author&query=Zihe+Liu">Zihe Liu</a>
</div>
```

The `query` parameter is the canonical name form. Use this as the dedup key:
- Extract `query=Kaiyu+Huang` → `Kaiyu Huang`
- This is consistent across all papers on arxiv for the same author

## Cross-Paper Linking

When ingesting paper B that references paper A:
1. Extract reference arxiv IDs from the HTML or Semantic Scholar references API
2. Compute `uuid5(ARXIV_NS, ref_arxiv_id)` for each reference
3. Check if that entity already exists in ferrosa-memory
4. If yes: create `references` edge from paper B → paper A
5. If no: create a placeholder document entity with just the title + arxiv ID

This builds the citation graph automatically.

## Author Dedup via Semantic Scholar

```
GET https://api.semanticscholar.org/graph/v1/paper/ArXiv:2604.03110?fields=authors
→ { authors: [{ authorId: "12345", name: "Kaiyu Huang" }, ...] }
```

The `authorId` is stable — same person always gets the same ID regardless of name variations. Use this as the UUID seed for person entities.

## Implementation

In `crates/ingest/src/paper.rs`:

1. **Parse arxiv HTML**: extract author names from `<a href="/search/?query=...">` links
2. **Normalize names**: use the URL query parameter (already normalized by arxiv)
3. **Query Semantic Scholar**: get stable `authorId` for each author
4. **Generate UUIDs**: `uuid5(ARXIV_NS, arxiv_id)` for papers, `uuid5(AUTHOR_NS, author_id)` for people
5. **Store URIs**: `source_url` annotation with arxiv/scholar URLs
6. **Cross-reference**: check existing entities by UUID before creating new ones

## Benefits

- Same author across 10 papers → 1 entity (not 10)
- Same paper referenced by 5 others → 1 entity with 5 incoming `references` edges
- Citation graph builds automatically
- URIs enable linking to external systems (arxiv, Semantic Scholar, ORCID)
- The `uri` field on EntityEntry (added in Batch 5) stores the canonical URL

## Author ID Priority

1. **ORCID** — `https://orcid.org/0000-0002-1234-5678` — globally unique, researcher-controlled. Available in arxiv HTML metadata (`<meta name="citation_author_orcid">`), Semantic Scholar, and CrossRef.
2. **Semantic Scholar authorId** — numeric, stable across name variants. Free API.
3. **Arxiv author search URL** — `query=Kaiyu+Huang` — normalized by arxiv, consistent per author.
4. **Normalized name** — `First Last` — last resort with phonetic matching.

UUID generation: `uuid5(ORCID_NS, orcid_id)` > `uuid5(SCHOLAR_NS, author_id)` > `uuid5(AUTHOR_NS, normalized_name)`

Store all available IDs as annotations on the person entity:
```
annotation(author, "orcid", "0000-0002-1234-5678")
annotation(author, "semantic_scholar_id", "12345678")
annotation(author, "arxiv_query", "Kaiyu+Huang")
```

This enables future cross-referencing and federation with external databases.
