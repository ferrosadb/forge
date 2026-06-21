# Bug: `mcp_forge_ingest_paper` on Two-Column PDF Generates Garbled Entities

**Severity:** High
**Component:** `mcp_forge_ingest_paper` / PDF extraction pipeline
**Version:** Current (v0.6.x)

## Issue

When `mcp_forge_ingest_paper` fetches and processes an arXiv PDF with a standard two-column LaTeX layout, the internal `pdftotext` extraction interleaves text from the left and right columns line-by-line. This produces word salad entities, phantom "authors," and completely misses section boundaries.

### Evidence

**ArXiv:2604.28087 (NEXUS 2026, 2-column ACM-style layout)**

`mcp_forge_ingest_paper` on the PDF produced:
- **Document entity:** title truncated mid-word (`"Towards Neuro-symbolic Causal Rule Synthesis, Verification, and"`)
- **Authors:** Only 2 extracted, one of them is the truncated title (`"Towards Neuro-symbolic Causal Rule Synthesis"` parsed as a person)
- **Sections:** Only 1 extracted (`"4 APPROACH"`), all other sections lost
- **Concepts:** 2 vague fragments (`"synthesis and verification layer"…`) instead of actual key terms
- **Total:** 5 entities, 4 edges — essentially useless for downstream graph queries

Meanwhile the paper has **7 sections + references**, **4 real authors**, **4 formal research problems**, **2 evaluation scenarios**, **3 evaluated LLM models**, and **42 citations**.

**Raw `pdftotext` output shows the root cause** — line 95 of the raw text:
```
2   STATE OF THE ART                                                      Humans often state goals imprecisely, while AI systems pursue
```
The left column header `"2 STATE OF THE ART"` and the right column body text appear on the SAME line. A naive line scan treats this as one string, so the regex `^\d+\s+[A-Z]+` matches the left portion but the right portion becomes part of the section title — or, depending on line order, gets concatenated with the next line's right column, producing garbage.

## Root Causes

1. **No de-columnization step.** The ingest pipeline runs `pdftotext -layout` (or equivalent) and treats the output as linear prose. Two-column PDFs require layout-aware reconstruction: splitting at the inter-column gutter, emitting the left-column paragraphs, then the right-column paragraphs, per page.

2. **No section boundary heuristics for column breaks.** Section headers like `1 INTRODUCTION` can appear in the LEFT column while the preceding section's body continues in the RIGHT column. A naive line scan will interleave them.

3. **No layout structure from the PDF.** We lose paragraph boundaries, column boundaries, and page boundaries. All downstream extraction (authors, sections, concepts, references) depends on knowing where the columns are.

## Impact

- Any paper ingested from a two-column PDF (majority of CS/ML preprints) produces a broken, low-fidelity knowledge graph.
- Users must manually re-parse and re-ingest, or fall back to abstract-only ingestion (which misses sections, concepts, and references).
- The `mcp_forge_ingest_paper` spec promises section parsing and concept extraction, but the actual implementation cannot deliver on any 2-column paper.

## Expected Behavior

After PDF extraction:
- **Text should be linearized** by column, not by line.
- **Section headers should be detected** independently of column position.
- **Author extraction** should not hallucinate from header text.
- **Entity count** for this paper should be ~30-40 (document, 4 authors, 8 sections, 10+ concepts, venue, scenarios, models) — not 5.

## Reproduction

```bash
# Any 2-column arXiv PDF
curl -L -o /tmp/2604.28087.pdf "https://arxiv.org/pdf/2604.28087.pdf"

# Ingest via forge
frg ingest-paper /tmp/2604.28087.pdf --cql localhost:19042

# Observe: only ~5 entities, half of them garbage.
# Compare to `frg ingest-paper https://arxiv.org/abs/2604.28087` (abstract only)
# which at least gets the title right but still lacks sections.
```

## Suggested Fixes

### Option A: De-columnize in the ingest pipeline
In the PDF extraction step (before calling any LLM or regex parser):
1. Detect the inter-column gutter (space-density analysis, typically ~70-80 spaces at a fixed column).
2. For each page:
   - Split lines at gutter position.
   - Join hyphenated words across lines within each column.
   - Emit all left-column paragraphs, then all right-column paragraphs.
3. Apply section detection to the resulting linear text.

### Option B: Skip PDF, use arXiv HTML5 instead
ArXiv provides an HTML5 version (`/html/2604.28087`) that is already linear and section-tagged. The ingest pipeline could prefer the HTML5 source over the PDF when available, avoiding the layout problem entirely.

### Option C: Use a layout-aware PDF library
Replace `pdftotext` with a library that returns structured text blocks (e.g., `pdfplumber` in Python, or `poppler` with bounding boxes). Sort text blocks by reading order (column-aware) before emitting plain text.

## Workaround

Until fixed, users can:
1. Download the arXiv HTML5 version manually.
2. Pipe `pdftotext` output through a de-columnizer script.
3. Accept abstract-only ingestion and manually add sections/concepts later.
