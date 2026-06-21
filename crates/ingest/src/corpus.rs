//! Corpus markdown ingestion — 3-layer knowledge graph schema.
//!
//! Parses distillation files from `/corpus/` into:
//!   - L1: document entity (metadata)
//!   - L2: summary entity (Core Thesis / first section)
//!   - L3: one entity per `##` section
//!
//! All IDs are deterministic UUID v5 (NAMESPACE_DNS) so re-runs are idempotent.
//!
//! UUID keys:
//!   L1: uuid5(NS, "corpus:L1:{rel_path}")
//!   L2: uuid5(NS, "corpus:L2:{rel_path}")
//!   L3: uuid5(NS, "corpus:L3:{rel_path}:{heading}")

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use regex::Regex;
use uuid::Uuid;
use walkdir::WalkDir;

use crate::extractor::{Edge, Entity, IngestReport, IngestSummary, EXTRACTOR_SCHEMA_VERSION};

/// UUID v5 namespace for corpus entities (NAMESPACE_DNS).
const CORPUS_NS: Uuid = Uuid::from_bytes([
    0x6b, 0xa7, 0xb8, 0x10, 0x9d, 0xad, 0x11, 0xd1, 0x80, 0xb4, 0x00, 0xc0, 0x4f, 0xd4, 0x30, 0xc8,
]);

/// Files to skip during directory traversal (matches against filename).
const SKIP_PATTERNS: &[&str] = &[
    "INDEX",
    "symlink",
    "README",
    "references",
    "SKILL",
    "isolation-trap",
    "downloads-import",
];

// ---------------------------------------------------------------------------
// Parsed corpus document
// ---------------------------------------------------------------------------

#[derive(Debug)]
struct CorpusDoc {
    title: String,
    author: String,
    year: String,
    publisher: String,
    category: String,
    /// Relative path from corpus root parent (e.g. "corpus/functional-programming/foo.md")
    rel_path: String,
    summary_section: Section,
    sections: Vec<Section>,
}

#[derive(Debug, Clone)]
struct Section {
    heading: String,
    content: String,
}

// ---------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------

/// Parse a corpus markdown file into a `CorpusDoc`.
/// Returns `None` if the file cannot be parsed as a corpus document.
fn parse_corpus_file(path: &Path) -> Result<Option<CorpusDoc>> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("reading corpus file {}", path.display()))?;

    // Determine relative path for deterministic IDs.
    // We store as "corpus/<category>/<filename>" matching the Python script convention.
    let category = path
        .parent()
        .and_then(|p| p.file_name())
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();

    let filename = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();

    let rel_path = format!("corpus/{category}/{filename}");

    let mut lines: Vec<&str> = raw.lines().collect();

    // Strip YAML frontmatter (--- ... ---)
    if lines.first().map(|l| l.trim()) == Some("---") {
        if let Some(end) = lines[1..].iter().position(|l| l.trim() == "---") {
            lines = lines[end + 2..].to_vec();
        }
    }

    // First line must be `# Title`
    let title = match lines.first() {
        Some(l) if l.starts_with("# ") => l[2..].trim().to_string(),
        _ => return Ok(None),
    };

    // Parse **Key:** Value metadata from first 20 lines
    let re_meta = Regex::new(r"^\*\*(\w+):\*\*\s*(.*)$").unwrap();
    let mut author = String::new();
    let mut year = String::new();
    let mut publisher = String::new();

    for line in lines.iter().take(20) {
        if let Some(cap) = re_meta.captures(line) {
            match cap[1].to_lowercase().as_str() {
                "author" => author = cap[2].trim().to_string(),
                "year" => year = cap[2].trim().to_string(),
                "publisher" => publisher = cap[2].trim().to_string(),
                _ => {}
            }
        }
    }

    // Split into ## sections
    let mut sections: Vec<Section> = Vec::new();
    let mut current_heading: Option<String> = None;
    let mut current_lines: Vec<&str> = Vec::new();

    for line in &lines {
        if let Some(heading) = line.strip_prefix("## ") {
            // Flush the previous section
            if let Some(h) = current_heading.take() {
                sections.push(Section {
                    heading: h,
                    content: current_lines.join("\n").trim().to_string(),
                });
            }
            current_heading = Some(heading.trim().to_string());
            current_lines = Vec::new();
        } else if current_heading.is_some() {
            current_lines.push(line);
        }
    }
    // Flush last section
    if let Some(h) = current_heading {
        sections.push(Section {
            heading: h,
            content: current_lines.join("\n").trim().to_string(),
        });
    }

    if sections.is_empty() {
        return Ok(None);
    }

    // L2 summary: prefer "Core Thesis" heading, else first section
    let summary_section = sections
        .iter()
        .find(|s| s.heading.to_lowercase().contains("core thesis"))
        .cloned()
        .unwrap_or_else(|| sections[0].clone());

    Ok(Some(CorpusDoc {
        title,
        author,
        year,
        publisher,
        category,
        rel_path,
        summary_section,
        sections,
    }))
}

// ---------------------------------------------------------------------------
// Graph builder
// ---------------------------------------------------------------------------

fn corpus_id(key: &str) -> String {
    Uuid::new_v5(&CORPUS_NS, key.as_bytes()).to_string()
}

/// Build entities and edges for a single corpus document.
fn build_doc_graph(doc: &CorpusDoc) -> (Vec<Entity>, Vec<Edge>) {
    let mut entities: Vec<Entity> = Vec::new();
    let mut edges: Vec<Edge> = Vec::new();

    let l1_id = corpus_id(&format!("corpus:L1:{}", doc.rel_path));
    let l2_id = corpus_id(&format!("corpus:L2:{}", doc.rel_path));

    // Build topics string from first 8 section headings
    let topics: String = doc
        .sections
        .iter()
        .take(8)
        .map(|s| s.heading.as_str())
        .collect::<Vec<_>>()
        .join(", ");

    // L1 — document metadata entity
    let l1_context = format!(
        "BOOK METADATA | Title: {} | Author: {} | Publisher: {} | Year: {} | Category: {} | Corpus file: {} | Key topics: {} | Scope: global",
        doc.title, doc.author, doc.publisher, doc.year, doc.category, doc.rel_path, topics
    );
    entities.push(Entity {
        id: l1_id.clone(),
        name: doc.title.clone(),
        entity_type: "document".to_string(),
        context: l1_context,
        extractor_schema_version: Some(EXTRACTOR_SCHEMA_VERSION),
        ..Default::default()
    });

    // L2 — summary / core thesis entity
    let l2_context = format!(
        "## {}\nSource: {} ({}, {}) — {}\n\n{}",
        doc.summary_section.heading,
        doc.title,
        doc.author,
        doc.year,
        doc.rel_path,
        truncate(&doc.summary_section.content, 8000),
    );
    entities.push(Entity {
        id: l2_id.clone(),
        name: format!("{} [Summary]", doc.title),
        entity_type: "section".to_string(),
        context: l2_context,
        extractor_schema_version: Some(EXTRACTOR_SCHEMA_VERSION),
        ..Default::default()
    });

    // L1 → L2 contains edge
    edges.push(Edge {
        src_id: l1_id.clone(),
        dst_id: l2_id,
        edge_type: "contains".to_string(),
        weight: 1.0,
        ..Default::default()
    });

    // L3 — one entity per section, plus sequential related_to chain
    let mut prev_l3_id: Option<String> = None;
    for section in &doc.sections {
        let l3_id = corpus_id(&format!("corpus:L3:{}:{}", doc.rel_path, section.heading));
        let l3_context = format!(
            "## {}\nSource: {} ({}, {}) — {}\n\n{}",
            section.heading,
            doc.title,
            doc.author,
            doc.year,
            doc.rel_path,
            truncate(&section.content, 8000),
        );
        entities.push(Entity {
            id: l3_id.clone(),
            name: format!("{} | {}", doc.title, section.heading),
            entity_type: "section".to_string(),
            context: l3_context,
            extractor_schema_version: Some(EXTRACTOR_SCHEMA_VERSION),
            ..Default::default()
        });

        // L1 → L3 contains
        edges.push(Edge {
            src_id: l1_id.clone(),
            dst_id: l3_id.clone(),
            edge_type: "contains".to_string(),
            weight: 1.0,
            ..Default::default()
        });

        // L3[n-1] → L3[n] sequential chain
        if let Some(prev) = prev_l3_id.take() {
            edges.push(Edge {
                src_id: prev,
                dst_id: l3_id.clone(),
                edge_type: "related_to".to_string(),
                weight: 0.9,
                ..Default::default()
            });
        }
        prev_l3_id = Some(l3_id);
    }

    (entities, edges)
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Extract corpus documents from a path (file or directory) and return an IngestReport.
///
/// When `path` is a directory, all `.md` files are processed recursively,
/// skipping files whose names match known non-document patterns.
pub fn extract_corpus(path: &Path) -> Result<IngestReport> {
    let files = collect_corpus_files(path)?;
    if files.is_empty() {
        anyhow::bail!("No corpus markdown files found at {}", path.display());
    }

    let mut all_entities: Vec<Entity> = Vec::new();
    let mut all_edges: Vec<Edge> = Vec::new();
    let mut docs_parsed: usize = 0;

    for file in &files {
        let doc = match parse_corpus_file(file)? {
            Some(d) => d,
            None => {
                eprintln!(
                    "[forge corpus] skipping (not a corpus doc): {}",
                    file.display()
                );
                continue;
            }
        };
        eprintln!(
            "[forge corpus] parsed: \"{}\" ({} sections)",
            doc.title,
            doc.sections.len()
        );
        let (entities, edges) = build_doc_graph(&doc);
        all_entities.extend(entities);
        all_edges.extend(edges);
        docs_parsed += 1;
    }

    if docs_parsed == 0 {
        anyhow::bail!("No parseable corpus documents found at {}", path.display());
    }

    let documents = all_entities
        .iter()
        .filter(|e| e.entity_type == "document")
        .count();
    let sections_count = all_entities
        .iter()
        .filter(|e| e.entity_type == "section")
        .count();
    let contains_edges = all_edges
        .iter()
        .filter(|e| e.edge_type == "contains")
        .count();

    Ok(IngestReport {
        path: path.to_string_lossy().into_owned(),
        language: "corpus".to_string(),
        session_id: Uuid::new_v4().to_string(),
        summary: IngestSummary {
            crates: 0,
            modules: 0,
            code_symbols: 0,
            documents,
            sections: sections_count,
            depends_on_edges: 0,
            contains_edges,
            calls_edges: 0,
            total_entities: all_entities.len(),
            total_edges: all_edges.len(),
        },
        entities: all_entities,
        edges: all_edges,
    })
}

/// Collect `.md` files from a path (file or directory).
fn collect_corpus_files(path: &Path) -> Result<Vec<PathBuf>> {
    if path.is_file() {
        return Ok(vec![path.to_path_buf()]);
    }

    let mut files: Vec<PathBuf> = Vec::new();
    for entry in WalkDir::new(path)
        .follow_links(false)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        let entry_path = entry.path().to_path_buf();
        if !entry_path.is_file() {
            continue;
        }
        let ext = entry_path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("");
        if ext != "md" {
            continue;
        }
        let name = entry_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("");
        if SKIP_PATTERNS.iter().any(|p| name.contains(p)) {
            continue;
        }
        files.push(entry_path);
    }
    files.sort();
    Ok(files)
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn truncate(s: &str, max: usize) -> &str {
    if s.len() <= max {
        s
    } else {
        &s[..max]
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_corpus_id_deterministic() {
        let id1 = corpus_id("corpus:L1:corpus/fp/foo.md");
        let id2 = corpus_id("corpus:L1:corpus/fp/foo.md");
        assert_eq!(id1, id2);
        // Ensure different keys produce different IDs
        let id3 = corpus_id("corpus:L1:corpus/fp/bar.md");
        assert_ne!(id1, id3);
    }

    #[test]
    fn test_build_doc_graph_structure() {
        let doc = CorpusDoc {
            title: "Test Book".to_string(),
            author: "Test Author".to_string(),
            year: "2024".to_string(),
            publisher: "Test Press".to_string(),
            category: "test".to_string(),
            rel_path: "corpus/test/test-book.md".to_string(),
            summary_section: Section {
                heading: "Core Thesis".to_string(),
                content: "The core thesis content.".to_string(),
            },
            sections: vec![
                Section {
                    heading: "Core Thesis".to_string(),
                    content: "The core thesis content.".to_string(),
                },
                Section {
                    heading: "Chapter 1".to_string(),
                    content: "Chapter 1 content.".to_string(),
                },
                Section {
                    heading: "Chapter 2".to_string(),
                    content: "Chapter 2 content.".to_string(),
                },
            ],
        };

        let (entities, edges) = build_doc_graph(&doc);

        // 1 L1 + 1 L2 + 3 L3 = 5
        assert_eq!(entities.len(), 5);

        // entity types
        assert_eq!(entities[0].entity_type, "document");
        assert_eq!(entities[1].entity_type, "section"); // L2
        assert!(entities[2..].iter().all(|e| e.entity_type == "section")); // L3

        // L1 name = title
        assert_eq!(entities[0].name, "Test Book");
        // L2 name has [Summary]
        assert!(entities[1].name.contains("[Summary]"));
        // L3 names are "Title | Heading"
        assert_eq!(entities[2].name, "Test Book | Core Thesis");

        // edges: 1 L1→L2 + 3 L1→L3 + 2 L3→L3 sequential = 6
        assert_eq!(edges.len(), 6);
        let contains: Vec<_> = edges.iter().filter(|e| e.edge_type == "contains").collect();
        let related: Vec<_> = edges
            .iter()
            .filter(|e| e.edge_type == "related_to")
            .collect();
        assert_eq!(contains.len(), 4); // L1→L2 + 3 L1→L3
        assert_eq!(related.len(), 2); // L3[0]→L3[1], L3[1]→L3[2]

        // related_to weight
        assert!((related[0].weight - 0.9).abs() < f64::EPSILON);
    }

    #[test]
    fn test_skip_patterns() {
        let skips = ["INDEX.md", "README.md", "symlink-index.md"];
        for name in &skips {
            assert!(
                SKIP_PATTERNS.iter().any(|p| name.contains(p)),
                "{name} should be skipped"
            );
        }
        assert!(!SKIP_PATTERNS
            .iter()
            .any(|p| "becomingfunctional.md".contains(p)));
    }
}
