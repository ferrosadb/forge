# Paper Ingestion — Academic Papers → Knowledge Graph

## Problem

LLMs frequently reference academic papers but the knowledge is ephemeral — read once, forgotten next session. Research insights, author networks, citation graphs, and key concepts should persist in the knowledge graph so future sessions can query "what papers did we read about X?" or "who are the key authors in memory augmented LLMs?"

## Solution

Add `frg ingest-paper` command that extracts structured knowledge from academic papers (arxiv, Semantic Scholar, ACL Anthology, local PDFs) and creates entities + edges in ferrosa-memory.

## Usage

```bash
# From arxiv URL
frg ingest-paper https://arxiv.org/abs/2401.12345

# From Semantic Scholar
frg ingest-paper https://api.semanticscholar.org/paper/abc123

# From local PDF
frg ingest-paper ./papers/memory-augmented-agents.pdf

# From DOI
frg ingest-paper doi:10.1234/example.2024

# Batch: ingest all papers in a BibTeX file
frg ingest-paper --bibtex refs.bib

# Crawl co-author papers (depth=1)
frg ingest-paper https://arxiv.org/abs/2401.12345 --follow-authors

# Direct CQL loading
frg ingest-paper https://arxiv.org/abs/2401.12345 --cql localhost:19042
```

## Entity Types Created

| Entity Type | Example | Fields |
|---|---|---|
| `document` | "MemWalker: Memory-Augmented..." | title, abstract, year, venue, doi, arxiv_id, source_url |
| `person` | "Alice Researcher" | name, affiliation, orcid (if available) |
| `org` | "Stanford University" | name, type (university/company/lab) |
| `concept` | "retrieval-augmented generation" | name, definition (from abstract context) |
| `section` | "3.2 Memory Architecture" | heading, summary, parent_document |

## Edge Types Created

| Edge | Meaning |
|---|---|
| `person` → `wrote` → `document` | Author relationship |
| `document` → `references` → `document` | Citation link |
| `document` → `discusses` → `concept` | Key topic extraction |
| `person` → `affiliated_with` → `org` | Author affiliation |
| `document` → `contains` → `section` | Document structure |
| `concept` → `related_to` → `concept` | Co-occurring concepts |
| `person` → `related_to` → `person` | Co-authorship (derived from shared papers) |

## Extraction Pipeline

### Phase 1: Fetch & Parse

```
URL/DOI/PDF → resolve source → fetch content → parse into structured form
```

**Source handlers:**

| Source | Method |
|---|---|
| arxiv.org | Fetch `/abs/` page HTML, extract metadata. For full text: fetch `/html/` (arxiv HTML5) or `/pdf/` |
| Semantic Scholar API | `GET /paper/{id}?fields=title,authors,abstract,references,citations,venue,year` |
| ACL Anthology | Fetch page HTML, extract metadata + PDF link |
| Local PDF | Extract text via `pdf-extract` or `lopdf` crate. Parse sections by heading patterns. |
| DOI | Resolve via `https://doi.org/{doi}` → follow redirect to publisher → extract metadata |
| BibTeX | Parse `.bib` file, resolve each entry's DOI/URL |

**Structured output:**
```rust
struct PaperMetadata {
    title: String,
    authors: Vec<Author>,
    abstract_text: String,
    year: Option<u16>,
    venue: Option<String>,         // "NeurIPS 2024", "ACL 2023"
    doi: Option<String>,
    arxiv_id: Option<String>,
    source_url: String,
    references: Vec<Reference>,    // cited papers
    sections: Vec<Section>,        // parsed document structure
    keywords: Vec<String>,         // author keywords or extracted
}

struct Author {
    name: String,
    affiliation: Option<String>,
    orcid: Option<String>,
}

struct Reference {
    title: String,
    authors: Vec<String>,
    year: Option<u16>,
    doi: Option<String>,
    arxiv_id: Option<String>,
}

struct Section {
    heading: String,
    level: u8,           // 1=H1, 2=H2, etc.
    text: String,
    concepts: Vec<String>, // extracted key concepts
}
```

### Phase 2: Entity Creation

For each extracted element, call `smart_ingest` via ferrosa-memory:

1. **Paper entity** (document):
   ```
   smart_ingest(content="{title}. {abstract}", entity_type="document", entity_name="{title}")
   ```

2. **Author entities** (person):
   ```
   smart_ingest(content="{name}, {affiliation}", entity_type="person", entity_name="{name}")
   ```
   - `smart_ingest` handles dedup: if "Alice Researcher" already exists from a prior paper, it UPDATEs instead of creating a duplicate
   - Author name variants: "A. Researcher" vs "Alice Researcher" — store both forms, smart_ingest's phonetic matching catches common variants

3. **Organization entities** (org):
   ```
   smart_ingest(content="{affiliation}", entity_type="org", entity_name="{affiliation}")
   ```

4. **Concept entities** (concept):
   - Extract from abstract + section headings + keywords
   - Use NER or simple noun phrase extraction
   ```
   smart_ingest(content="{concept} — discussed in {paper_title}", entity_type="concept", entity_name="{concept}")
   ```

5. **Section entities** (section) — optional, for deep indexing:
   ```
   smart_ingest(content="{heading}: {summary}", entity_type="section", entity_name="{heading}")
   ```

### Phase 3: Edge Creation

```
batch_create_edges([
    // Authorship
    { src: author_id, dst: paper_id, edge_type: "wrote" },
    
    // Citations (for each reference that matches an existing paper)
    { src: paper_id, dst: cited_paper_id, edge_type: "references" },
    
    // Topics
    { src: paper_id, dst: concept_id, edge_type: "discusses" },
    
    // Affiliations
    { src: author_id, dst: org_id, edge_type: "affiliated_with" },
    
    // Document structure
    { src: paper_id, dst: section_id, edge_type: "contains" },
])
```

### Phase 4: Provenance

All entities get `source_url` annotation via edge_annotations:
```
annotation(paper_entity, "source_url", "https://arxiv.org/abs/2401.12345")
annotation(paper_entity, "source_type", "academic_paper")
annotation(paper_entity, "ingested_at", "2026-04-05T22:00:00Z")
```

## Author Network Discovery

With `--follow-authors`:

1. After ingesting paper P, get all authors A1..An
2. For each author, query Semantic Scholar for their other papers
3. Ingest top-K papers (by citation count) for each author
4. This builds the co-author network and related work graph automatically

```
Paper A (ingested) 
  → Author X → Paper B, Paper C (auto-discovered)
  → Author Y → Paper D, Paper E (auto-discovered)
  → Paper B references Paper F (citation chain)
```

Future queries:
- "What has Author X published?" → `explore_connections(author_x, edge_type=wrote)`
- "What papers cite this work?" → `explore_connections(paper, edge_type=references, direction=incoming)`
- "Related work on memory augmentation?" → `hybrid_search("memory augmentation")` → finds papers + concepts

## Integration with Existing ingest_url

The `url.rs` module (1190 lines) already handles web page → entity extraction. Paper ingestion extends this with:
- Academic metadata parsing (authors, references, venue, DOI)
- Source-specific handlers (arxiv HTML, Semantic Scholar API, PDF extraction)
- Citation graph edge creation
- Author dedup across papers

Implementation options:
- **Option A:** New file `crates/ingest/src/paper.rs` alongside `url.rs`
- **Option B:** Extend `url.rs` with paper-specific detection (if URL matches arxiv/scholar/doi pattern, use paper pipeline)

Recommend Option A — paper extraction is specialized enough to warrant its own module.

## API Dependencies

| API | Usage | Auth Required |
|---|---|---|
| arxiv.org | Paper metadata + HTML | No (rate limited, be polite) |
| Semantic Scholar | Author search, citations, metadata | No (API key optional, increases rate limit) |
| CrossRef | DOI resolution, metadata | No |
| ORCID | Author disambiguation | No (public API) |

## Implementation Plan

| Task | Effort | Priority |
|---|---|---|
| `paper.rs` module with arxiv HTML parser | 2 days | P0 |
| Semantic Scholar API client | 1 day | P0 |
| PDF text extraction (local files) | 2 days | P1 |
| BibTeX parser | 1 day | P1 |
| `--follow-authors` co-author crawling | 1 day | P2 |
| DOI/CrossRef resolution | 1 day | P2 |
| Concept extraction from full text | 2 days | P2 |
| CLI integration (`frg ingest-paper`) | 0.5 day | P0 |
| Tests + sample papers | 1 day | P0 |

## Files to Create/Modify

```
crates/ingest/src/paper.rs        — new: paper extraction pipeline
crates/ingest/src/arxiv.rs        — new: arxiv-specific parser
crates/ingest/src/scholar.rs      — new: Semantic Scholar API client
crates/ingest/src/pdf.rs          — new: PDF text extraction
crates/ingest/src/bibtex.rs       — new: BibTeX parser
crates/ingest/src/lib.rs          — add paper module exports
crates/cli/src/main.rs            — add ingest-paper subcommand
```

## Example: Full Pipeline

```
$ frg ingest-paper https://arxiv.org/abs/2401.06104 --cql localhost:19042

Fetching https://arxiv.org/abs/2401.06104...
Parsed: "MemWalker: Interactive Memory Consolidation for LLM Agents"
Authors: 4 (Howard Chen, Ramakanth Pasunuru, Jason Weston, Asli Celikyilmaz)
Sections: 8
References: 42
Concepts: 12 (memory consolidation, LLM agents, retrieval augmented generation, ...)

Creating entities:
  document: "MemWalker: Interactive Memory..." (Created)
  person: "Howard Chen" (Created)
  person: "Ramakanth Pasunuru" (Created)
  person: "Jason Weston" (Updated — already known from prior paper)
  person: "Asli Celikyilmaz" (Created)
  org: "Meta AI" (Updated)
  concept: "memory consolidation" (Updated)
  concept: "retrieval augmented generation" (Updated — already known)
  ...

Creating edges:
  Howard Chen → wrote → MemWalker (4 authorship edges)
  MemWalker → discusses → memory consolidation (12 topic edges)
  MemWalker → references → "Generative Agents..." (8 citation edges matched)
  Howard Chen → affiliated_with → Meta AI (4 affiliation edges)

Summary: 18 entities (12 created, 6 updated), 28 edges
```
