# PDF Structure Extraction — Implementation Plan

> **For Hermes:** Use subagent-driven-development skill to implement this plan task-by-task.

**Goal:** Port 5 structural PDF parsing ideas from OpenDataLoader into the forge `crates/ingest` pipeline. Fix the broken two-column PDF ingestion (documented in `specs/todo/bug-ingest-paper-two-column-pdf-layout.md`) and enable rich KG entities from academic papers.

**Architecture:** Add a new `crates/ingest/src/pdf/` module tree with a pipeline of typed processors. The fast path uses `pdftotext -bbox` (bounding-box-aware extraction) followed by XY-Cut++ reading-order reconstruction. A new `PdfElement` typed stream replaces the raw text blob that `paper.rs` currently passes to regex-based extractors.

**Tech Stack:** Rust (forge crate), `pdftotext` (poppler, already a dependency), `serde_json`, `regex`. No JVM, no Python servers.

---

## Background: Current State

`crates/ingest/src/paper.rs:617-663` — `extract_from_pdf()` spawns `pdftotext -layout` and treats output as a linear prose blob. Lines from left and right columns are interleaved. Section extraction relies on regex `^(\d+(?:\.\d+)?)\s+([A-Z][^\n]{3,80})$` which fails when column text bleeds into section headers. The bug report `specs/todo/bug-ingest-paper-two-column-pdf-layout.md` documents the failure on arXiv:2604.28087.

`crates/ingest/src/sanitize.rs` — Already has web-content sanitization (prompt injection, hidden HTML, suspicious Unicode). PDF-specific hidden-text filtering is missing.

---

## Task 1: Add `pdftotext -bbox` wrapper and `PdfElement` typed stream

**Objective:** Extract text with bounding boxes, fonts, and page numbers — the foundation for all downstream processors.

**Files:**
- Create: `crates/ingest/src/pdf/mod.rs`
- Create: `crates/ingest/src/pdf/extract.rs`
- Create: `crates/ingest/src/pdf/element.rs`
- Modify: `crates/ingest/src/lib.rs` (add `pub mod pdf;`)

**Step 1: Define the element taxonomy**

In `crates/ingest/src/pdf/element.rs`:

```rust
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BoundingBox {
    pub left: f64,
    pub bottom: f64,
    pub right: f64,
    pub top: f64,
}

impl BoundingBox {
    pub fn width(&self) -> f64 { self.right - self.left }
    pub fn height(&self) -> f64 { self.top - self.bottom }
    pub fn center_x(&self) -> f64 { (self.left + self.right) / 2.0 }
    pub fn center_y(&self) -> f64 { (self.bottom + self.top) / 2.0 }
    /// Horizontal overlap ratio with another box [0.0, 1.0]
    pub fn overlap_ratio_x(&self, other: &BoundingBox) -> f64 {
        let overlap = (self.right.min(other.right) - self.left.max(other.left)).max(0.0);
        overlap / self.width().max(other.width())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum ElementType {
    Heading,
    Paragraph,
    Table,
    List,
    Image,
    Caption,
    Formula,
    HeaderFooter,
    Watermark,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PdfElement {
    pub id: u64,
    pub element_type: ElementType,
    pub page_number: u32,
    pub bounding_box: BoundingBox,
    pub text: String,
    pub font: Option<String>,
    pub font_size: Option<f64>,
    /// For headings: depth (1 = title, 2 = section, etc.)
    pub level: Option<u8>,
    /// For tables: number of rows/cols detected
    pub table_shape: Option<(usize, usize)>,
}

/// A page is a list of elements in reading order.
pub type PageElements = Vec<PdfElement>;
pub type DocumentElements = Vec<PageElements>;
```

**Step 2: Implement bbox extraction via `pdftotext`**

In `crates/ingest/src/pdf/extract.rs`:

```rust
use std::path::Path;
use std::process::Command;
use anyhow::{Context, Result};
use regex::Regex;
use crate::pdf::element::{BoundingBox, PdfElement, ElementType, PageElements, DocumentElements};

/// Run `pdftotext -bbox-layout` and parse the XML output into typed elements.
pub fn extract_with_bbox(path: &Path) -> Result<DocumentElements> {
    let output = Command::new("pdftotext")
        .arg("-bbox-layout")
        .arg(path)
        .arg("-")
        .output()
        .context("pdftotext not found — install poppler")?;

    if !output.status.success() {
        anyhow::bail!("pdftotext -bbox-layout failed: {}", String::from_utf8_lossy(&output.stderr));
    }

    let xml = String::from_utf8_lossy(&output.stdout);
    parse_bbox_xml(&xml)
}

fn parse_bbox_xml(xml: &str) -> Result<DocumentElements> {
    // pdftotext -bbox-layout emits simple XML:
    // <page number="1" ...>
    //   <word xMin="72.0" yMin="700.0" xMax="120.0" yMax="730.0">Hello</word>
    // </page>
    // We cluster words into lines, lines into paragraphs.
    let mut pages: DocumentElements = Vec::new();
    // TODO: implement XML parsing via quick-xml or regex
    Ok(pages)
}
```

**Step 3: Wire `lib.rs` and add `quick-xml` dependency**

In `crates/ingest/Cargo.toml`, add:
```toml
quick-xml = { version = "0.36", features = ["serialize"] }
```

In `crates/ingest/src/lib.rs`, add `pub mod pdf;`.

**Step 4: Run tests**

Create `crates/ingest/src/pdf/tests/` with a sample PDF fixture. For now, test with a hand-crafted XML string.

Command: `cargo test -p forge-ingest pdf::`
Expected: compilation passes, tests exist (may be `#[ignore]` until fixture is added).

**Step 5: Commit**

```bash
git add crates/ingest/src/pdf/ crates/ingest/Cargo.toml crates/ingest/src/lib.rs
git commit -m "feat(pdf): add PdfElement taxonomy and bbox extraction skeleton"
```

---

## Task 2: Implement XY-Cut++ Reading-Order Reconstruction

**Objective:** Re-sort text elements from PDF object order (line-by-line, column-interleaved) into actual reading order: left column top-to-bottom, then right column top-to-bottom.

**Files:**
- Create: `crates/ingest/src/pdf/reading_order.rs`
- Create: `crates/ingest/src/pdf/tests/reading_order_test.rs`
- Modify: `crates/ingest/src/pdf/mod.rs` (re-export)

**Step 1: Implement the XY-Cut++ algorithm**

In `crates/ingest/src/pdf/reading_order.rs`:

```rust
use crate::pdf::element::{BoundingBox, PdfElement};

/// XY-Cut++ reading order sorter.
/// Based on OpenDataLoader's XYCutPlusPlusSorter — simplified for pdftotext bbox output.
///
/// Algorithm:
/// 1. Pre-mask: identify cross-layout elements (width > beta * max_width, overlaps >= 2 columns).
/// 2. Compute density ratio to prefer X-cut (column split) vs Y-cut (row split).
/// 3. Recursively segment with adaptive axis selection.
/// 4. Merge cross-layout elements at top or bottom of page.
///
/// Returns elements sorted in reading order.
pub fn sort_reading_order(elements: &mut [PdfElement]) -> Vec<PdfElement> {
    const DEFAULT_BETA: f64 = 2.0;
    const DEFAULT_DENSITY_THRESHOLD: f64 = 0.9;
    const MIN_GAP_THRESHOLD: f64 = 5.0; // PDF points

    // Filter out elements with degenerate bounding boxes
    let valid: Vec<_> = elements
        .iter()
        .filter(|e| e.bounding_box.width() > 0.0 && e.bounding_box.height() > 0.0)
        .cloned()
        .collect();

    if valid.len() <= 1 {
        return valid;
    }

    // Phase 1: identify cross-layout elements (full-width headers, footers)
    let max_width = valid.iter().map(|e| e.bounding_box.width()).fold(0.0, f64::max);
    let (cross_layout, mut remaining): (Vec<_>, Vec<_>) = valid.into_iter().partition(|e| {
        e.bounding_box.width() > DEFAULT_BETA * max_width
    });

    // Count horizontal overlaps for each element
    let overlap_counts: Vec<usize> = remaining.iter().map(|a| {
        remaining.iter().filter(|b| a.id != b.id && a.bounding_box.overlap_ratio_x(&b.bounding_box) > 0.1).count()
    }).collect();

    // Re-classify: if an element overlaps >= 2 others horizontally, it's cross-layout
    let (mut true_cross, mut normal): (Vec<_>, Vec<_>) = remaining.into_iter().zip(overlap_counts.into_iter())
        .partition(|(_, count)| *count >= 2);

    true_cross.extend(cross_layout);

    // Phase 2: recursive sorting with adaptive axis selection
    let sorted = recursive_sort(&mut normal, DEFAULT_DENSITY_THRESHOLD, MIN_GAP_THRESHOLD);

    // Phase 3: merge cross-layout elements at top/bottom
    merge_cross_layout(sorted, &true_cross)
}

fn recursive_sort(elements: &mut [PdfElement], density_threshold: f64, min_gap: f64) -> Vec<PdfElement> {
    if elements.len() <= 1 {
        return elements.to_vec();
    }

    let region_bbox = bounding_box_of(elements);
    let density_x = compute_density_x(elements, &region_bbox);
    let density_y = compute_density_y(elements, &region_bbox);

    let prefer_x = density_x > density_threshold || density_x > density_y;

    if prefer_x {
        // Try X-cut (split by columns)
        if let Some(split_idx) = find_gap_split_x(elements, min_gap) {
            let (left, right) = elements.split_at_mut(split_idx);
            let mut result = recursive_sort(left, density_threshold, min_gap);
            result.extend(recursive_sort(right, density_threshold, min_gap));
            return result;
        }
    }

    // Try Y-cut (split by rows)
    if let Some(split_idx) = find_gap_split_y(elements, min_gap) {
        let (top, bottom) = elements.split_at_mut(split_idx);
        let mut result = recursive_sort(top, density_threshold, min_gap);
        result.extend(recursive_sort(bottom, density_threshold, min_gap));
        return result;
    }

    // Fallback: sort by Y then X
    sort_by_y_then_x(elements)
}

fn bounding_box_of(elements: &[PdfElement]) -> BoundingBox {
    let left = elements.iter().map(|e| e.bounding_box.left).fold(f64::INFINITY, f64::min);
    let bottom = elements.iter().map(|e| e.bounding_box.bottom).fold(f64::INFINITY, f64::min);
    let right = elements.iter().map(|e| e.bounding_box.right).fold(f64::NEG_INFINITY, f64::max);
    let top = elements.iter().map(|e| e.bounding_box.top).fold(f64::NEG_INFINITY, f64::max);
    BoundingBox { left, bottom, right, top }
}

fn compute_density_x(elements: &[PdfElement], region: &BoundingBox) -> f64 {
    let total_width: f64 = elements.iter().map(|e| e.bounding_box.width()).sum();
    total_width / region.width()
}

fn compute_density_y(elements: &[PdfElement], region: &BoundingBox) -> f64 {
    let total_height: f64 = elements.iter().map(|e| e.bounding_box.height()).sum();
    total_height / region.height()
}

fn find_gap_split_x(elements: &mut [PdfElement], min_gap: f64) -> Option<usize> {
    // Sort by center_x, look for largest gap
    elements.sort_by(|a, b| a.bounding_box.center_x().partial_cmp(&b.bounding_box.center_x()).unwrap());
    let mut max_gap = 0.0;
    let mut split_idx = None;
    for i in 1..elements.len() {
        let gap = elements[i].bounding_box.left - elements[i - 1].bounding_box.right;
        if gap > min_gap && gap > max_gap {
            max_gap = gap;
            split_idx = Some(i);
        }
    }
    split_idx
}

fn find_gap_split_y(elements: &mut [PdfElement], min_gap: f64) -> Option<usize> {
    elements.sort_by(|a, b| a.bounding_box.center_y().partial_cmp(&b.bounding_box.center_y()).unwrap());
    let mut max_gap = 0.0;
    let mut split_idx = None;
    for i in 1..elements.len() {
        let gap = elements[i].bounding_box.bottom - elements[i - 1].bounding_box.top;
        if gap > min_gap && gap > max_gap {
            max_gap = gap;
            split_idx = Some(i);
        }
    }
    split_idx
}

fn sort_by_y_then_x(elements: &mut [PdfElement]) -> Vec<PdfElement> {
    elements.sort_by(|a, b| {
        let y_cmp = b.bounding_box.top.partial_cmp(&a.bounding_box.top).unwrap(); // top-down
        if y_cmp != std::cmp::Ordering::Equal {
            return y_cmp;
        }
        a.bounding_box.left.partial_cmp(&b.bounding_box.left).unwrap()
    });
    elements.to_vec()
}

fn merge_cross_layout(mut sorted: Vec<PdfElement>, cross: &[PdfElement]) -> Vec<PdfElement> {
    // Full-width headers go at top, footers at bottom
    let mut headers: Vec<_> = cross.iter().filter(|e| e.bounding_box.top > 600.0).cloned().collect();
    let mut footers: Vec<_> = cross.iter().filter(|e| e.bounding_box.bottom < 200.0).cloned().collect();
    headers.sort_by(|a, b| b.bounding_box.top.partial_cmp(&a.bounding_box.top).unwrap());
    footers.sort_by(|a, b| b.bounding_box.top.partial_cmp(&a.bounding_box.top).unwrap());

    let mut result = headers;
    result.extend(sorted);
    result.extend(footers);
    result
}
```

**Step 2: Write tests**

In `crates/ingest/src/pdf/tests/reading_order_test.rs`:

```rust
#[test]
fn test_two_column_sorting() {
    let mut elements = vec![
        // Left column
        element_at(100.0, 700.0, "Left top"),
        element_at(100.0, 680.0, "Left middle"),
        element_at(100.0, 660.0, "Left bottom"),
        // Right column
        element_at(400.0, 700.0, "Right top"),
        element_at(400.0, 680.0, "Right middle"),
        element_at(400.0, 660.0, "Right bottom"),
    ];
    let sorted = sort_reading_order(&mut elements);
    let texts: Vec<_> = sorted.iter().map(|e| e.text.as_str()).collect();
    assert_eq!(texts, vec![
        "Left top", "Left middle", "Left bottom",
        "Right top", "Right middle", "Right bottom",
    ]);
}

#[test]
fn test_full_width_header_preserved() {
    let mut elements = vec![
        element_at(100.0, 700.0, "Left content"),
        element_wide_at(72.0, 750.0, "FULL WIDTH TITLE"), // cross-layout
        element_at(400.0, 700.0, "Right content"),
    ];
    let sorted = sort_reading_order(&mut elements);
    assert_eq!(sorted.first().unwrap().text, "FULL WIDTH TITLE");
}
```

**Step 3: Run tests**

Command: `cargo test -p forge-ingest pdf::reading_order`
Expected: PASS

**Step 4: Commit**

```bash
git add crates/ingest/src/pdf/reading_order.rs crates/ingest/src/pdf/tests/
git commit -m "feat(pdf): XY-Cut++ reading order for multi-column layouts"
```

---

## Task 3: Implement Processor Pipeline for Typed Element Extraction

**Objective:** Replace monolithic regex extraction in `paper.rs` with composable processors: Heading, Paragraph, Section, Table detection. Each processor enriches the `PdfElement` stream.

**Files:**
- Create: `crates/ingest/src/pdf/processors/mod.rs`
- Create: `crates/ingest/src/pdf/processors/heading.rs`
- Create: `crates/ingest/src/pdf/processors/section.rs`
- Create: `crates/ingest/src/pdf/processors/table.rs`
- Modify: `crates/ingest/src/pdf/mod.rs`
- Modify: `crates/ingest/src/paper.rs` (replace `extract_from_pdf` to use new pipeline)

**Step 1: Define the processor trait**

In `crates/ingest/src/pdf/processors/mod.rs`:

```rust
use crate::pdf::element::PageElements;

/// A processor that enriches a page of PDF elements.
pub trait PageProcessor {
    fn process(&self, page: &mut PageElements);
}

/// Run all processors in order.
pub fn process_page(processors: &[Box<dyn PageProcessor>], page: &mut PageElements) {
    for p in processors {
        p.process(page);
    }
}
```

**Step 2: Implement heading detection**

In `crates/ingest/src/pdf/processors/heading.rs`:

```rust
use crate::pdf::element::{ElementType, PdfElement};
use crate::pdf::processors::PageProcessor;

/// Detect headings via font size heuristics and numbered patterns.
pub struct HeadingProcessor {
    /// Minimum font size ratio vs median to qualify as heading
    pub size_ratio_threshold: f64,
}

impl Default for HeadingProcessor {
    fn default() -> Self {
        Self { size_ratio_threshold: 1.3 }
    }
}

impl PageProcessor for HeadingProcessor {
    fn process(&self, page: &mut crate::pdf::element::PageElements) {
        if page.is_empty() { return; }
        let median_size = median_font_size(page);
        for elem in page.iter_mut() {
            if let Some(size) = elem.font_size {
                if size > median_size * self.size_ratio_threshold {
                    elem.element_type = ElementType::Heading;
                    elem.level = Some(guess_heading_level(size, median_size));
                }
            }
            // Also detect "1. INTRODUCTION" style patterns
            if elem.text.trim().matches(|c: char| c.is_uppercase()).count() > elem.text.len() / 2 {
                if let Some(level) = detect_numbered_heading(&elem.text) {
                    elem.element_type = ElementType::Heading;
                    elem.level = Some(level);
                }
            }
        }
    }
}

fn median_font_size(page: &[PdfElement]) -> f64 {
    let mut sizes: Vec<f64> = page.iter().filter_map(|e| e.font_size).collect();
    sizes.sort_by(|a, b| a.partial_cmp(b).unwrap());
    sizes.get(sizes.len() / 2).copied().unwrap_or(12.0)
}

fn guess_heading_level(size: f64, median: f64) -> u8 {
    let ratio = size / median;
    if ratio > 2.0 { 1 } else if ratio > 1.5 { 2 } else { 3 }
}

fn detect_numbered_heading(text: &str) -> Option<u8> {
    let re = regex::Regex::new(r"^(\d+(?:\.\d+)?)\s+").ok()?;
    re.captures(text).map(|c| {
        let num = &c[1];
        if num.contains('.') { 2 } else { 1 }
    })
}
```

**Step 3: Implement section grouping**

In `crates/ingest/src/pdf/processors/section.rs`:

```rust
use crate::pdf::element::{ElementType, PdfElement};

/// Group consecutive non-heading elements into sections bounded by headings.
pub fn group_sections(elements: &[PdfElement]) -> Vec<PdfSection> {
    let mut sections = Vec::new();
    let mut current_heading = String::new();
    let mut current_text = Vec::new();

    for elem in elements {
        if elem.element_type == ElementType::Heading {
            if !current_text.is_empty() {
                sections.push(PdfSection {
                    heading: current_heading.clone(),
                    level: elem.level.unwrap_or(1) as u8,
                    text: current_text.join("\n"),
                });
            }
            current_heading = elem.text.clone();
            current_text = vec![elem.text.clone()];
        } else {
            current_text.push(elem.text.clone());
        }
    }

    if !current_text.is_empty() {
        sections.push(PdfSection {
            heading: current_heading,
            level: 1,
            text: current_text.join("\n"),
        });
    }

    sections
}

#[derive(Debug, Clone)]
pub struct PdfSection {
    pub heading: String,
    pub level: u8,
    pub text: String,
}
```

**Step 4: Implement table detection (heuristic)**

In `crates/ingest/src/pdf/processors/table.rs`:

```rust
use crate::pdf::element::{BoundingBox, ElementType, PdfElement};

/// Detect table-like structures via aligned text columns.
pub struct TableProcessor {
    pub min_rows: usize,
}

impl Default for TableProcessor {
    fn default() -> Self {
        Self { min_rows: 3 }
    }
}

impl crate::pdf::processors::PageProcessor for TableProcessor {
    fn process(&self, page: &mut crate::pdf::element::PageElements) {
        // Group elements by approximate Y position (rows)
        // Look for repeated aligned X positions (columns)
        // If enough rows have aligned columns, mark as table
        let row_groups = group_elements_by_y(page);
        let column_xs = detect_aligned_columns(&row_groups);
        if column_xs.len() >= 2 && row_groups.len() >= self.min_rows {
            for elem in page.iter_mut() {
                if row_groups.iter().any(|g| g.contains(&elem.id)) {
                    elem.element_type = ElementType::Table;
                }
            }
        }
    }
}

fn group_elements_by_y(page: &[PdfElement]) -> Vec<Vec<u64>> {
    // Sort by Y (top-down), group elements whose Y positions are within epsilon
    let mut sorted = page.to_vec();
    sorted.sort_by(|a, b| b.bounding_box.top.partial_cmp(&a.bounding_box.top).unwrap());
    let mut groups: Vec<Vec<u64>> = Vec::new();
    const EPSILON: f64 = 3.0; // PDF points
    for elem in sorted {
        let placed = groups.iter_mut().find(|g| {
            let first = page.iter().find(|e| e.id == g[0]).unwrap();
            (elem.bounding_box.top - first.bounding_box.top).abs() < EPSILON
        });
        if let Some(g) = placed {
            g.push(elem.id);
        } else {
            groups.push(vec![elem.id]);
        }
    }
    groups
}

fn detect_aligned_columns(row_groups: &[Vec<u64>]) -> Vec<f64> {
    // Find common X positions across rows
    // Simplified: return unique sorted left edges
    vec![] // TODO: implement clustering
}
```

**Step 5: Wire new pipeline into `extract_from_pdf`**

In `crates/ingest/src/paper.rs`, replace lines 616-663:

```rust
/// Extract text from a local PDF via pdftotext with structural awareness.
fn extract_from_pdf(path: &Path) -> Result<PaperMetadata> {
    use crate::pdf::element::DocumentElements;
    use crate::pdf::extract::extract_with_bbox;
    use crate::pdf::processors::heading::HeadingProcessor;
    use crate::pdf::processors::section::group_sections;
    use crate::pdf::processors::table::TableProcessor;
    use crate::pdf::reading_order::sort_reading_order;

    if !path.exists() {
        bail!("PDF file not found: {}", path.display());
    }

    eprintln!("[forge] Extracting structured text from: {}", path.display());

    // Step 1: Extract with bounding boxes
    let mut document: DocumentElements = extract_with_bbox(path)?;

    // Step 2: Apply reading order to each page
    for page in document.iter_mut() {
        *page = sort_reading_order(page);
    }

    // Step 3: Run typed processors per page
    let heading_proc = HeadingProcessor::default();
    let table_proc = TableProcessor::default();
    for page in document.iter_mut() {
        heading_proc.process(page);
        table_proc.process(page);
    }

    // Step 4: Flatten to linear text and group sections
    let all_elements: Vec<_> = document.into_iter().flatten().collect();
    let sections = group_sections(&all_elements);

    // Step 5: Fallback extraction for metadata not in bbox output
    let output = Command::new("pdftotext")
        .arg("-layout")
        .arg(path)
        .arg("-")
        .output()
        .context("pdftotext not found")?;
    let fallback_text = String::from_utf8_lossy(&output.stdout).to_string();

    let title = extract_title_from_text(&fallback_text);
    let authors = extract_authors_from_text(&fallback_text);
    let abstract_text = extract_abstract_from_text(&fallback_text);
    let keywords = extract_keywords_from_text(&fallback_text);

    Ok(PaperMetadata {
        title,
        authors,
        abstract_text,
        year: extract_year_from_text(&fallback_text),
        venue: None,
        doi: extract_doi_from_text(&fallback_text),
        arxiv_id: None,
        source_url: format!("file://{}", path.canonicalize().unwrap_or_default().display()),
        references: Vec::new(),
        sections: sections.into_iter().map(|s| PaperSection {
            heading: s.heading,
            level: s.level,
            text: s.text,
        }).collect(),
        keywords,
    })
}
```

**Step 6: Run integration test**

Create `crates/ingest/tests/pdf_pipeline.rs` with a fixture PDF (can be generated programmatically).

Command: `cargo test -p forge-ingest --test pdf_pipeline`
Expected: PASS (sections detected, reading order correct)

**Step 7: Commit**

```bash
git add crates/ingest/src/pdf/processors/ crates/ingest/src/paper.rs
git commit -m "feat(pdf): typed processor pipeline — heading, section, table detection"
```

---

## Task 4: Add PDF-Specific Content Filtering

**Objective:** Detect hidden text, transparent layers, and prompt injection embedded in PDFs before elements reach the LLM extraction stage.

**Files:**
- Create: `crates/ingest/src/pdf/filter.rs`
- Modify: `crates/ingest/src/pdf/extract.rs` (call filter after extraction)
- Modify: `crates/ingest/src/sanitize.rs` (add PDF-specific categories)

**Step 1: Implement PDF hidden-text detection**

In `crates/ingest/src/pdf/filter.rs`:

```rust
use crate::pdf::element::PdfElement;

/// Signals indicating suspicious content in a PDF element.
#[derive(Debug, Clone, PartialEq)]
pub enum FilterSignal {
    HiddenText,          // zero-size font, transparent color
    OffPage,             // coordinates outside page bounds
    SuspiciousLayer,     // text behind image or in odd-layer
    PromptInjection,     // text contains injection patterns
}

/// Result of filtering a PDF element stream.
pub struct FilterResult {
    pub kept: Vec<PdfElement>,
    pub removed: Vec<(PdfElement, FilterSignal)>,
}

/// Filter suspicious elements from PDF extraction before downstream processing.
pub fn filter_pdf_elements(elements: &[PdfElement]) -> FilterResult {
    let mut kept = Vec::new();
    let mut removed = Vec::new();

    for elem in elements {
        if let Some(signal) = detect_hidden_text(elem) {
            removed.push((elem.clone(), signal));
            continue;
        }
        if let Some(signal) = detect_off_page(elem) {
            removed.push((elem.clone(), signal));
            continue;
        }
        if let Some(signal) = detect_prompt_injection(elem) {
            removed.push((elem.clone(), signal));
            continue;
        }
        kept.push(elem.clone());
    }

    FilterResult { kept, removed }
}

fn detect_hidden_text(elem: &PdfElement) -> Option<FilterSignal> {
    // Hidden text: zero font size, or very small width/height
    if elem.font_size == Some(0.0) || elem.font_size == Some(0.1) {
        return Some(FilterSignal::HiddenText);
    }
    if elem.bounding_box.width() < 0.5 && elem.bounding_box.height() < 0.5 {
        return Some(FilterSignal::HiddenText);
    }
    None
}

fn detect_off_page(elem: &PdfElement) -> Option<FilterSignal> {
    // Standard US Letter page: 612x792 points
    // Elements entirely outside this are suspicious
    if elem.bounding_box.left > 650.0 || elem.bounding_box.bottom > 850.0 {
        return Some(FilterSignal::OffPage);
    }
    None
}

fn detect_prompt_injection(elem: &PdfElement) -> Option<FilterSignal> {
    let lower = elem.text.to_lowercase();
    let patterns = [
        "ignore previous instructions",
        "disregard all prior",
        "you are now",
        "system prompt",
        "<|im_start|>",
        "<|im_end|>",
    ];
    if patterns.iter().any(|p| lower.contains(p)) {
        return Some(FilterSignal::PromptInjection);
    }
    None
}
```

**Step 2: Wire filter into extraction pipeline**

In `crates/ingest/src/pdf/extract.rs`, after `extract_with_bbox` returns, call `filter_pdf_elements` and log removals:

```rust
let filtered = crate::pdf::filter::filter_pdf_elements(&all_elements);
if !filtered.removed.is_empty() {
    eprintln!("[forge-pdf] Filtered {} suspicious elements", filtered.removed.len());
    for (elem, signal) in &filtered.removed {
        eprintln!("  {:?}: {:?}", signal, &elem.text[..elem.text.len().min(80)]);
    }
}
let all_elements = filtered.kept;
```

**Step 3: Add PDF filter categories to sanitize.rs WarningCategory**

In `crates/ingest/src/sanitize.rs`, add to `WarningCategory`:
```rust
    /// Hidden text in PDF (zero-size font, transparent, off-page)
    PdfHiddenText,
    /// Suspicious PDF layer or content
    PdfSuspiciousLayer,
```

**Step 4: Run tests**

Command: `cargo test -p forge-ingest pdf::filter`
Expected: PASS

**Step 5: Commit**

```bash
git add crates/ingest/src/pdf/filter.rs crates/ingest/src/sanitize.rs crates/ingest/src/pdf/extract.rs
git commit -m "feat(pdf): content filtering — hidden text, off-page, prompt injection"
```

---

## Task 5: Add Per-Page Triage for Simple vs Complex Pages

**Objective:** Classify each page as "simple" (text-only, single column) or "complex" (tables, multi-column, figures, formulas). In future, complex pages can route to LLM backend. For now, we just tag pages and log triage decisions.

**Files:**
- Create: `crates/ingest/src/pdf/triage.rs`
- Modify: `crates/ingest/src/pdf/extract.rs` (call triage per page)
- Modify: `crates/ingest/src/paper.rs` (use triage to decide fallback extraction depth)

**Step 1: Implement TriageProcessor equivalent**

In `crates/ingest/src/pdf/triage.rs`:

```rust
use crate::pdf::element::{ElementType, PdfElement};

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TriageDecision {
    /// Fast deterministic path: text-only, single column, no tables.
    Simple,
    /// Needs deeper analysis: tables, multi-column, figures, formulas.
    Complex,
}

#[derive(Debug, Clone)]
pub struct TriageResult {
    pub decision: TriageDecision,
    pub confidence: f64,
    pub signals: Vec<TriageSignal>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TriageSignal {
    /// Detected table-like aligned columns
    AlignedColumns,
    /// Multiple text columns detected
    MultiColumn,
    /// Large image or figure detected
    LargeImage,
    /// Mathematical formula detected (LaTeX-like symbols)
    Formula,
    /// Scanned / image-based page (no selectable text)
    ImageBased,
}

/// Triage a page to determine processing path.
///
/// Conservative: false positives (simple marked complex) are acceptable.
/// False negatives (complex marked simple) mean missed tables/formulas.
pub fn triage_page(elements: &[PdfElement]) -> TriageResult {
    let mut signals = Vec::new();
    let mut score = 0.0;

    // Signal 1: Aligned columns (table indicator)
    if has_aligned_columns(elements) {
        signals.push(TriageSignal::AlignedColumns);
        score += 0.4;
    }

    // Signal 2: Multi-column layout
    if has_multi_column_layout(elements) {
        signals.push(TriageSignal::MultiColumn);
        score += 0.2;
    }

    // Signal 3: Large images
    if has_large_images(elements) {
        signals.push(TriageSignal::LargeImage);
        score += 0.3;
    }

    // Signal 4: Formulas (dollar signs, backslashes, Greek letters)
    if has_formulas(elements) {
        signals.push(TriageSignal::Formula);
        score += 0.3;
    }

    // Signal 5: Image-based page (very few or very small text elements)
    if is_image_based(elements) {
        signals.push(TriageSignal::ImageBased);
        score += 0.5;
    }

    let decision = if score >= 0.3 {
        TriageDecision::Complex
    } else {
        TriageDecision::Simple
    };

    TriageResult {
        decision,
        confidence: score.min(1.0),
        signals,
    }
}

fn has_aligned_columns(elements: &[PdfElement]) -> bool {
    // Group elements by approximate Y, check for repeated X intervals
    // Simplified: if >=3 elements share nearly the same X, it's a column
    let mut x_buckets: Vec<f64> = elements.iter().map(|e| e.bounding_box.left).collect();
    x_buckets.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let mut groups = 0usize;
    let mut i = 0;
    while i < x_buckets.len() {
        let x0 = x_buckets[i];
        let j = x_buckets.iter().skip(i).position(|x| (x - x0).abs() > 5.0).unwrap_or(x_buckets.len() - i);
        if j >= 3 {
            groups += 1;
        }
        i += j.max(1);
    }
    groups >= 2
}

fn has_multi_column_layout(elements: &[PdfElement]) -> bool {
    // If elements split into two distinct X clusters with a large gap
    let xs: Vec<f64> = elements.iter().map(|e| e.bounding_box.center_x()).collect();
    if xs.len() < 4 { return false; }
    let min_x = xs.iter().fold(f64::INFINITY, |a, &b| a.min(b));
    let max_x = xs.iter().fold(f64::NEG_INFINITY, |a, &b| a.max(b));
    let mid = (min_x + max_x) / 2.0;
    let left_count = xs.iter().filter(|&&x| x < mid).count();
    let right_count = xs.iter().filter(|&&x| x >= mid).count();
    left_count >= 3 && right_count >= 3 && (max_x - min_x) > 250.0
}

fn has_large_images(elements: &[PdfElement]) -> bool {
    elements.iter().any(|e| {
        e.element_type == ElementType::Image && e.bounding_box.width() > 200.0 && e.bounding_box.height() > 100.0
    })
}

fn has_formulas(elements: &[PdfElement]) -> bool {
    let formula_re = regex::Regex::new(r"[\\\$Σ∫αβγδεζηθικλμνξοπρστυφχψω]").unwrap();
    let formula_count = elements.iter().filter(|e| formula_re.is_match(&e.text)).count();
    formula_count >= 2
}

fn is_image_based(elements: &[PdfElement]) -> bool {
    // Very few text elements with small total area suggests scanned/image page
    let text_elements: Vec<_> = elements.iter().filter(|e| e.text.len() > 2).collect();
    text_elements.len() < 5 || text_elements.iter().map(|e| e.bounding_box.width() * e.bounding_box.height()).sum::<f64>() < 500.0
}
```

**Step 2: Wire triage into `extract_from_pdf`**

After reading-order sorting, log triage per page:

```rust
for (page_num, page) in document.iter().enumerate() {
    let triage = crate::pdf::triage::triage_page(page);
    eprintln!(
        "[forge-pdf] Page {}: {:?} (confidence: {:.2}, signals: {:?})",
        page_num + 1,
        triage.decision,
        triage.confidence,
        triage.signals
    );
}
```

**Step 3: Use triage to enhance `extract_from_pdf` behavior**

If a page is `Complex`, keep the structured `PdfElement` stream. If `Simple`, the fallback `-layout` text is sufficient. For now, this is informational. In future, `Complex` pages can route to an LLM for table/formula extraction.

**Step 4: Run tests**

```rust
#[test]
fn test_triage_simple_page() {
    let elements = vec![
        element_at(100.0, 700.0, "Introduction"),
        element_at(100.0, 680.0, "This is a simple paragraph."),
        element_at(100.0, 660.0, "Another paragraph."),
    ];
    let triage = triage_page(&elements);
    assert_eq!(triage.decision, TriageDecision::Simple);
}

#[test]
fn test_triage_table_page() {
    let elements = vec![
        element_at(100.0, 700.0, "Col 1"),
        element_at(300.0, 700.0, "Col 2"),
        element_at(500.0, 700.0, "Col 3"),
        element_at(100.0, 680.0, "A"),
        element_at(300.0, 680.0, "B"),
        element_at(500.0, 680.0, "C"),
        element_at(100.0, 660.0, "D"),
        element_at(300.0, 660.0, "E"),
        element_at(500.0, 660.0, "F"),
    ];
    let triage = triage_page(&elements);
    assert_eq!(triage.decision, TriageDecision::Complex);
    assert!(triage.signals.contains(&TriageSignal::AlignedColumns));
}
```

**Step 5: Commit**

```bash
git add crates/ingest/src/pdf/triage.rs crates/ingest/src/pdf/extract.rs crates/ingest/src/paper.rs
git commit -m "feat(pdf): per-page triage — simple vs complex routing"
```

---

## Task 6: Integration — End-to-End Test with Real Two-Column PDF

**Objective:** Verify the complete pipeline works on the bug-report PDF (arXiv:2604.28087 or a similar two-column paper).

**Files:**
- Create: `crates/ingest/tests/pdf_e2e.rs`

**Step 1: Download test fixture**

```bash
curl -L -o crates/ingest/tests/fixtures/twocol_paper.pdf "https://arxiv.org/pdf/2508.10104.pdf"
```

**Step 2: Write E2E test**

```rust
#[test]
#[ignore = "requires network + pdftotext"]
fn test_two_column_paper_sections() {
    let report = forge_ingest::paper::extract_paper("crates/ingest/tests/fixtures/twocol_paper.pdf").unwrap();
    let section_count = report.summary.sections;
    assert!(
        section_count >= 5,
        "Expected at least 5 sections from a real two-column paper, got {}",
        section_count
    );
}
```

**Step 3: Run (manually)**

```bash
cargo test -p forge-ingest --test pdf_e2e -- --ignored --nocapture
```

**Step 4: Commit**

```bash
git add crates/ingest/tests/pdf_e2e.rs crates/ingest/tests/fixtures/.gitignore
git commit -m "test(pdf): e2e test for two-column paper section extraction"
```

---

## Task 7: Update CLI to expose new PDF options

**Objective:** Allow users to request structured PDF output via `frg ingest-paper`.

**Files:**
- Modify: `crates/cli/src/main.rs` (add `--structured-pdf` flag)
- Modify: `crates/cli/tests/mcp_ingest.rs` (add test for structured path)

**Step 1: Add CLI flag**

In `crates/cli/src/main.rs`, add to the ingest-paper subcommand:
```rust
/// Extract structured elements (headings, sections, tables) from PDFs
#[arg(long)]
structured_pdf: bool,
```

**Step 2: Pass flag to `extract_paper`**

Update the `ingest_paper` call site to pass `structured_pdf`.

**Step 3: Run tests**

Command: `cargo test -p forge-cli`
Expected: PASS

**Step 4: Commit**

```bash
git add crates/cli/src/main.rs crates/cli/tests/mcp_ingest.rs
git commit -m "feat(cli): add --structured-pdf flag to ingest-paper"
```

---

## Summary of All Commits (intended)

1. `feat(pdf): add PdfElement taxonomy and bbox extraction skeleton`
2. `feat(pdf): XY-Cut++ reading order for multi-column layouts`
3. `feat(pdf): typed processor pipeline — heading, section, table detection`
4. `feat(pdf): content filtering — hidden text, off-page, prompt injection`
5. `feat(pdf): per-page triage — simple vs complex routing`
6. `test(pdf): e2e test for two-column paper section extraction`
7. `feat(cli): add --structured-pdf flag to ingest-paper`

---

## Acceptance Criteria

- [ ] `pdftotext -bbox-layout` successfully extracts text with bounding boxes
- [ ] XY-Cut++ correctly re-sorts two-column pages (left column before right)
- [ ] Heading detection marks section titles with proper levels
- [ ] At least 5 sections extracted from arXiv two-column PDF (vs current ~1)
- [ ] No hallucinated authors extracted from interleaved column text
- [ ] Hidden text and off-page elements filtered before downstream processing
- [ ] Triage tags each page as Simple or Complex with confidence score
- [ ] All new modules compile with `cargo build -p forge-ingest`
- [ ] All new tests pass: `cargo test -p forge-ingest pdf::`
- [ ] Existing paper ingestion tests still pass (no regression)
