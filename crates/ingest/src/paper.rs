//! Academic paper ingestion: extract structured knowledge from papers.
//!
//! Supports: arxiv, Semantic Scholar, IEEE, ACM, bioRxiv, DOI resolution,
//! local PDFs, and any paywalled source via browser-based download.
//!
//! Pipeline: source → resolve → fetch metadata → extract text → build graph → IngestReport

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};
use regex::Regex;
use uuid::Uuid;

use crate::extractor::{Edge, Entity, IngestReport, IngestSummary, EXTRACTOR_SCHEMA_VERSION};
use crate::url;

// UUID v5 namespace for paper entities (deterministic IDs)
const PAPER_NS: Uuid = Uuid::from_bytes([
    0xb2, 0xc3, 0xd4, 0xe5, 0xf6, 0x07, 0x18, 0x29, 0x3a, 0x4b, 0x5c, 0x6d, 0x7e, 0x8f, 0x90, 0xa1,
]);

const MAX_REFERENCES: usize = 100;
const MAX_CONCEPTS: usize = 200;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Detected academic source type.
#[derive(Debug, Clone, PartialEq)]
pub enum PaperSource {
    Arxiv { arxiv_id: String },
    SemanticScholar { paper_id: String },
    Doi { doi: String },
    Ieee { url: String },
    Acm { url: String },
    BioRxiv { doi: String },
    PubMed { pmid: String },
    GenericUrl { url: String },
    LocalPdf { path: PathBuf },
}

/// Metadata extracted from a paper.
#[derive(Debug, Clone)]
pub struct PaperMetadata {
    pub title: String,
    pub authors: Vec<Author>,
    pub abstract_text: String,
    pub year: Option<u16>,
    pub venue: Option<String>,
    pub doi: Option<String>,
    pub arxiv_id: Option<String>,
    pub source_url: String,
    pub references: Vec<Reference>,
    pub sections: Vec<PaperSection>,
    pub keywords: Vec<String>,
}

/// An author with optional affiliation.
#[derive(Debug, Clone)]
pub struct Author {
    pub name: String,
    pub affiliation: Option<String>,
}

/// A cited reference.
#[derive(Debug, Clone)]
pub struct Reference {
    pub title: String,
    pub authors: Vec<String>,
    pub year: Option<u16>,
    pub doi: Option<String>,
}

/// A section of the paper.
#[derive(Debug, Clone)]
pub struct PaperSection {
    pub heading: String,
    pub level: u8,
    pub text: String,
}

// ---------------------------------------------------------------------------
// Source detection
// ---------------------------------------------------------------------------

/// Detect the academic source from a URL, DOI, or file path.
pub fn detect_source(input: &str) -> PaperSource {
    let input_lower = input.to_lowercase();

    // Local PDF file
    if input.ends_with(".pdf") && !input.starts_with("http") {
        return PaperSource::LocalPdf {
            path: PathBuf::from(input),
        };
    }

    // DOI shorthand: doi:10.xxxx/yyyy
    if let Some(doi) = input.strip_prefix("doi:") {
        return PaperSource::Doi {
            doi: doi.to_string(),
        };
    }

    // arxiv
    if input_lower.contains("arxiv.org") {
        if let Some(id) = extract_arxiv_id(input) {
            return PaperSource::Arxiv { arxiv_id: id };
        }
    }

    // bioRxiv / medRxiv
    if input_lower.contains("biorxiv.org") || input_lower.contains("medrxiv.org") {
        if let Some(doi) = extract_doi_from_url(input) {
            return PaperSource::BioRxiv { doi };
        }
    }

    // Semantic Scholar
    if input_lower.contains("semanticscholar.org") || input_lower.contains("api.semanticscholar") {
        if let Some(id) = extract_s2_id(input) {
            return PaperSource::SemanticScholar { paper_id: id };
        }
    }

    // IEEE
    if input_lower.contains("ieee.org") || input_lower.contains("ieeexplore") {
        return PaperSource::Ieee {
            url: input.to_string(),
        };
    }

    // ACM
    if input_lower.contains("acm.org") || input_lower.contains("dl.acm.org") {
        return PaperSource::Acm {
            url: input.to_string(),
        };
    }

    // PubMed
    if input_lower.contains("pubmed.ncbi") || input_lower.contains("ncbi.nlm.nih.gov") {
        if let Some(pmid) = extract_pubmed_id(input) {
            return PaperSource::PubMed { pmid };
        }
    }

    // DOI in URL: https://doi.org/10.xxxx/yyyy
    if input_lower.contains("doi.org/10.") {
        if let Some(doi) = extract_doi_from_url(input) {
            return PaperSource::Doi { doi };
        }
    }

    // Any URL with a DOI pattern in the path
    let re_doi = Regex::new(r"10\.\d{4,}/[^\s]+").unwrap();
    if let Some(m) = re_doi.find(input) {
        return PaperSource::Doi {
            doi: m.as_str().to_string(),
        };
    }

    // Generic URL fallback
    if input.starts_with("http://") || input.starts_with("https://") {
        return PaperSource::GenericUrl {
            url: input.to_string(),
        };
    }

    // Last resort: treat as local PDF
    PaperSource::LocalPdf {
        path: PathBuf::from(input),
    }
}

fn extract_arxiv_id(url: &str) -> Option<String> {
    // Matches: arxiv.org/abs/2401.12345, arxiv.org/pdf/2401.12345
    let re = Regex::new(r"arxiv\.org/(?:abs|pdf|html)/(\d{4}\.\d{4,5}(?:v\d+)?)").unwrap();
    re.captures(url).map(|c| c[1].to_string())
}

fn extract_s2_id(url: &str) -> Option<String> {
    // Matches: semanticscholar.org/paper/TITLE/HASH or api.semanticscholar.org/graph/v1/paper/ID
    let re = Regex::new(r"paper/(?:.*/)?([a-f0-9]{40})").unwrap();
    if let Some(c) = re.captures(url) {
        return Some(c[1].to_string());
    }
    // Also match Corpus ID or S2 ID patterns
    let re2 = Regex::new(r"paper/(\S+)$").unwrap();
    re2.captures(url).map(|c| c[1].to_string())
}

fn extract_doi_from_url(url: &str) -> Option<String> {
    let re = Regex::new(r"(10\.\d{4,}/[^\s?#]+)").unwrap();
    re.captures(url).map(|c| c[1].to_string())
}

fn extract_pubmed_id(url: &str) -> Option<String> {
    let re = Regex::new(r"(?:/|=)(\d{7,9})\b").unwrap();
    re.captures(url).map(|c| c[1].to_string())
}

// ---------------------------------------------------------------------------
// Metadata fetching
// ---------------------------------------------------------------------------

/// Fetch paper metadata from the detected source.
pub fn fetch_metadata(source: &PaperSource) -> Result<PaperMetadata> {
    match source {
        PaperSource::Arxiv { arxiv_id } => fetch_arxiv(arxiv_id),
        PaperSource::SemanticScholar { paper_id } => fetch_semantic_scholar(paper_id),
        PaperSource::Doi { doi } => fetch_doi(doi),
        PaperSource::BioRxiv { doi } => fetch_doi(doi), // bioRxiv uses DOI resolution
        PaperSource::PubMed { pmid } => fetch_pubmed(pmid),
        PaperSource::Ieee { url } | PaperSource::Acm { url } => {
            // Always open paywalled sources in browser (user has membership)
            eprintln!("[forge] Opening paywalled source in browser: {url}");
            open_in_browser(url);
            fetch_via_browser_or_html(url)
        }
        PaperSource::GenericUrl { url } => fetch_via_browser_or_html(url),
        PaperSource::LocalPdf { path } => extract_from_pdf(path),
    }
}

/// Fetch arxiv paper metadata from the abs page + Semantic Scholar for references.
fn fetch_arxiv(arxiv_id: &str) -> Result<PaperMetadata> {
    let abs_url = format!("https://arxiv.org/abs/{arxiv_id}");
    eprintln!("[forge] Fetching arxiv: {abs_url}");

    let result = url::fetch_html(&abs_url)?;
    let html = &result.html;

    // Extract title from <meta name="citation_title">
    let title = extract_meta(html, "citation_title")
        .or_else(|| extract_og(html, "title"))
        .unwrap_or_else(|| format!("arxiv:{arxiv_id}"));

    // Extract abstract from <blockquote class="abstract">
    let abstract_text = extract_between(html, r#"class="abstract">"#, "</blockquote>")
        .map(|s| {
            let re = Regex::new(r"<[^>]+>").unwrap();
            re.replace_all(&s, "").trim().to_string()
        })
        .unwrap_or_default();

    // Extract authors — prefer arxiv search URLs for canonical names
    let re_author_link =
        Regex::new(r#"(?i)<a\s+href="/search/\?searchtype=author&query=([^"]+)"[^>]*>([^<]+)</a>"#)
            .unwrap();
    let mut authors: Vec<Author> = re_author_link
        .captures_iter(html)
        .map(|cap| {
            // Use the URL query param (canonical form), falling back to link text
            let name = urldecode(&cap[1]).replace('+', " ");
            Author {
                name: normalize_author_name(&name),
                affiliation: None,
            }
        })
        .collect();

    // Fall back to citation_author meta tags if no links found
    if authors.is_empty() {
        authors = extract_all_meta(html, "citation_author")
            .into_iter()
            .map(|name| Author {
                name: normalize_author_name(&name),
                affiliation: None,
            })
            .collect();
    }

    // Extract year
    let year = extract_meta(html, "citation_date")
        .and_then(|d| d.split('/').next().and_then(|y| y.parse().ok()));

    // Extract keywords from <meta name="citation_keywords">
    let keywords = extract_all_meta(html, "citation_keywords");

    // Try to get references from Semantic Scholar. A failed fetch
    // produces an empty reference list — log the cause so the user can
    // distinguish "paper has no references" from "S2 fetch failed".
    let references = match fetch_references_from_s2(&format!("ARXIV:{arxiv_id}")) {
        Ok(refs) => refs,
        Err(e) => {
            eprintln!("[forge paper] Semantic Scholar refs fetch failed for arxiv:{arxiv_id}: {e}");
            Vec::new()
        }
    };

    Ok(PaperMetadata {
        title,
        authors,
        abstract_text,
        year,
        venue: extract_meta(html, "citation_journal_title"),
        doi: extract_meta(html, "citation_doi"),
        arxiv_id: Some(arxiv_id.to_string()),
        source_url: abs_url,
        references,
        sections: Vec::new(), // sections from full text require HTML or PDF
        keywords,
    })
}

/// Fetch metadata from Semantic Scholar API.
fn fetch_semantic_scholar(paper_id: &str) -> Result<PaperMetadata> {
    let api_url = format!(
        "https://api.semanticscholar.org/graph/v1/paper/{paper_id}?fields=title,authors,abstract,year,venue,externalIds,references.title,references.authors,references.year,references.externalIds"
    );
    eprintln!("[forge] Fetching Semantic Scholar: {api_url}");

    let result = url::fetch_html(&api_url)?;
    let data: serde_json::Value =
        serde_json::from_str(&result.html).context("failed to parse Semantic Scholar response")?;

    let title = data["title"].as_str().unwrap_or("Untitled").to_string();
    let abstract_text = data["abstract"].as_str().unwrap_or("").to_string();
    let year = data["year"].as_u64().map(|y| y as u16);
    let venue = data["venue"].as_str().map(|s| s.to_string());

    let authors = data["authors"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .map(|a| Author {
                    name: normalize_author_name(a["name"].as_str().unwrap_or("")),
                    affiliation: None,
                })
                .collect()
        })
        .unwrap_or_default();

    let doi = data["externalIds"]["DOI"].as_str().map(|s| s.to_string());
    let arxiv_id = data["externalIds"]["ArXiv"].as_str().map(|s| s.to_string());

    let references = data["references"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .take(MAX_REFERENCES)
                .filter_map(|r| {
                    let t = r["title"].as_str()?;
                    Some(Reference {
                        title: t.to_string(),
                        authors: r["authors"]
                            .as_array()
                            .map(|a| {
                                a.iter()
                                    .filter_map(|x| x["name"].as_str().map(|s| s.to_string()))
                                    .collect()
                            })
                            .unwrap_or_default(),
                        year: r["year"].as_u64().map(|y| y as u16),
                        doi: r["externalIds"]["DOI"].as_str().map(|s| s.to_string()),
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    Ok(PaperMetadata {
        title,
        authors,
        abstract_text,
        year,
        venue,
        doi,
        arxiv_id,
        source_url: format!("https://api.semanticscholar.org/paper/{paper_id}"),
        references,
        sections: Vec::new(),
        keywords: Vec::new(),
    })
}

/// Fetch references for a paper from Semantic Scholar by external ID.
fn fetch_references_from_s2(external_id: &str) -> Result<Vec<Reference>> {
    let api_url = format!(
        "https://api.semanticscholar.org/graph/v1/paper/{external_id}?fields=references.title,references.authors,references.year,references.externalIds"
    );
    let result = url::fetch_html(&api_url)?;
    let data: serde_json::Value = serde_json::from_str(&result.html)?;

    Ok(data["references"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .take(MAX_REFERENCES)
                .filter_map(|r| {
                    let t = r["title"].as_str()?;
                    Some(Reference {
                        title: t.to_string(),
                        authors: r["authors"]
                            .as_array()
                            .map(|a| {
                                a.iter()
                                    .filter_map(|x| x["name"].as_str().map(|s| s.to_string()))
                                    .collect()
                            })
                            .unwrap_or_default(),
                        year: r["year"].as_u64().map(|y| y as u16),
                        doi: r["externalIds"]["DOI"].as_str().map(|s| s.to_string()),
                    })
                })
                .collect()
        })
        .unwrap_or_default())
}

/// Resolve DOI to metadata via doi.org content negotiation.
fn fetch_doi(doi: &str) -> Result<PaperMetadata> {
    let doi_url = format!("https://doi.org/{doi}");
    eprintln!("[forge] Resolving DOI: {doi}");

    // Try Semantic Scholar first — it has the richest metadata
    let s2_result = fetch_semantic_scholar(&format!("DOI:{doi}"));
    if let Ok(mut meta) = s2_result {
        meta.doi = Some(doi.to_string());
        meta.source_url = doi_url;
        return Ok(meta);
    }

    // Fall back to CrossRef-style JSON via content negotiation
    let config = ureq::Agent::config_builder()
        .timeout_global(Some(std::time::Duration::from_secs(30)))
        .build();
    let agent = ureq::Agent::new_with_config(config);

    let response = agent
        .get(&doi_url)
        .header("Accept", "application/json")
        .call()
        .context("DOI resolution failed")?;

    let body = response
        .into_body()
        .with_config()
        .limit(5 * 1024 * 1024)
        .read_to_string()
        .context("failed to read DOI response")?;

    let data: serde_json::Value =
        serde_json::from_str(&body).context("failed to parse DOI JSON")?;

    let title = data["title"]
        .as_str()
        .or_else(|| data["message"]["title"].as_array()?.first()?.as_str())
        .unwrap_or("Untitled")
        .to_string();

    let authors = data["author"]
        .as_array()
        .or_else(|| data["message"]["author"].as_array())
        .map(|arr| {
            arr.iter()
                .map(|a| {
                    let given = a["given"].as_str().unwrap_or("");
                    let family = a["family"].as_str().unwrap_or("");
                    Author {
                        name: normalize_author_name(format!("{given} {family}").trim()),
                        affiliation: a["affiliation"]
                            .as_array()
                            .and_then(|aff| aff.first())
                            .and_then(|a| a["name"].as_str())
                            .map(|s| s.to_string()),
                    }
                })
                .collect()
        })
        .unwrap_or_default();

    Ok(PaperMetadata {
        title,
        authors,
        abstract_text: data["abstract"].as_str().unwrap_or("").to_string(),
        year: data["published-print"]["date-parts"][0][0]
            .as_u64()
            .or_else(|| data["issued"]["date-parts"][0][0].as_u64())
            .or_else(|| data["message"]["issued"]["date-parts"][0][0].as_u64())
            .map(|y| y as u16),
        venue: data["container-title"]
            .as_str()
            .or_else(|| {
                data["message"]["container-title"]
                    .as_array()
                    .and_then(|a| a.first())
                    .and_then(|v| v.as_str())
            })
            .map(|s| s.to_string()),
        doi: Some(doi.to_string()),
        arxiv_id: None,
        source_url: doi_url,
        references: Vec::new(),
        sections: Vec::new(),
        keywords: Vec::new(),
    })
}

/// Fetch PubMed metadata via NCBI E-utilities.
fn fetch_pubmed(pmid: &str) -> Result<PaperMetadata> {
    let api_url = format!(
        "https://eutils.ncbi.nlm.nih.gov/entrez/eutils/esummary.fcgi?db=pubmed&id={pmid}&retmode=json"
    );
    eprintln!("[forge] Fetching PubMed: {pmid}");

    let result = url::fetch_html(&api_url)?;
    let data: serde_json::Value = serde_json::from_str(&result.html)?;
    let doc = &data["result"][pmid];

    let title = doc["title"].as_str().unwrap_or("Untitled").to_string();
    let authors = doc["authors"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .map(|a| Author {
                    name: normalize_author_name(a["name"].as_str().unwrap_or("")),
                    affiliation: None,
                })
                .collect()
        })
        .unwrap_or_default();

    let year = doc["pubdate"]
        .as_str()
        .and_then(|d| d.split(' ').next())
        .and_then(|y| y.parse().ok());

    let doi = doc["elocationid"]
        .as_str()
        .and_then(|s| s.strip_prefix("doi: "))
        .map(|s| s.to_string());

    Ok(PaperMetadata {
        title,
        authors,
        abstract_text: String::new(), // esummary doesn't include abstract
        year,
        venue: doc["source"].as_str().map(|s| s.to_string()),
        doi,
        arxiv_id: None,
        source_url: format!("https://pubmed.ncbi.nlm.nih.gov/{pmid}/"),
        references: Vec::new(),
        sections: Vec::new(),
        keywords: Vec::new(),
    })
}

/// For paywalled sources (IEEE, ACM) or generic URLs: try HTML scraping first,
/// offer browser-based download for full text.
fn fetch_via_browser_or_html(source_url: &str) -> Result<PaperMetadata> {
    eprintln!("[forge] Fetching: {source_url}");

    // Try to get metadata from the page HTML
    let result = url::fetch_html(source_url)?;
    let html = &result.html;

    let title = extract_meta(html, "citation_title")
        .or_else(|| extract_og(html, "title"))
        .or_else(|| extract_html_title(html))
        .unwrap_or_else(|| "Untitled".to_string());

    let authors = extract_all_meta(html, "citation_author")
        .into_iter()
        .map(|name| Author {
            name: normalize_author_name(&name),
            affiliation: None,
        })
        .collect::<Vec<_>>();

    let abstract_text = extract_meta(html, "citation_abstract")
        .or_else(|| extract_meta(html, "description"))
        .or_else(|| extract_og(html, "description"))
        .unwrap_or_default();

    let year = extract_meta(html, "citation_publication_date")
        .or_else(|| extract_meta(html, "citation_date"))
        .and_then(|d| d.split('/').next().and_then(|y| y.parse().ok()));

    let doi = extract_meta(html, "citation_doi");

    // If we got a DOI, try to enrich via Semantic Scholar
    if let Some(ref doi_val) = doi {
        if let Ok(mut enriched) = fetch_semantic_scholar(&format!("DOI:{doi_val}")) {
            enriched.source_url = source_url.to_string();
            if enriched.doi.is_none() {
                enriched.doi = doi.clone();
            }
            return Ok(enriched);
        }
    }

    // Open in browser so the user (or LLM reading tool output) can see the page
    // Especially useful for paywalled IEEE/ACM where the user is authenticated
    eprintln!("[forge] Opening in browser: {source_url}");
    open_in_browser(source_url);

    // If we couldn't get metadata, report what we have and suggest PDF fallback
    if title == "Untitled" && authors.is_empty() {
        eprintln!(
            "[forge] Limited metadata extracted. \
             For full text, download the PDF and run: \
             frg ingest-paper <path-to-downloaded.pdf>"
        );
    }

    Ok(PaperMetadata {
        title,
        authors,
        abstract_text,
        year,
        venue: extract_meta(html, "citation_journal_title"),
        doi,
        arxiv_id: extract_meta(html, "citation_arxiv_id"),
        source_url: source_url.to_string(),
        references: Vec::new(),
        sections: Vec::new(),
        keywords: extract_all_meta(html, "citation_keywords"),
    })
}

/// Extract text from a local PDF via pdftotext.
fn extract_from_pdf(path: &Path) -> Result<PaperMetadata> {
    if !path.exists() {
        bail!("PDF file not found: {}", path.display());
    }

    eprintln!("[forge] Extracting text from: {}", path.display());

    let output = Command::new("pdftotext")
        .arg("-layout")
        .arg(path)
        .arg("-")
        .output()
        .context("pdftotext not found — install poppler: brew install poppler")?;

    if !output.status.success() {
        bail!(
            "pdftotext failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let text = String::from_utf8_lossy(&output.stdout).to_string();

    // Parse basic structure from the extracted text
    let title = extract_title_from_text(&text);
    let authors = extract_authors_from_text(&text);
    let abstract_text = extract_abstract_from_text(&text);
    let sections = extract_sections_from_text(&text);
    let keywords = extract_keywords_from_text(&text);

    Ok(PaperMetadata {
        title,
        authors,
        abstract_text,
        year: extract_year_from_text(&text),
        venue: None,
        doi: extract_doi_from_text(&text),
        arxiv_id: None,
        source_url: format!(
            "file://{}",
            path.canonicalize().unwrap_or_default().display()
        ),
        references: Vec::new(),
        sections,
        keywords,
    })
}

// ---------------------------------------------------------------------------
// Graph builder
// ---------------------------------------------------------------------------

/// Build an IngestReport from paper metadata.
pub fn build_paper_graph(meta: &PaperMetadata) -> IngestReport {
    let mut entities: Vec<Entity> = Vec::new();
    let mut edges: Vec<Edge> = Vec::new();

    let canonical_id = meta
        .doi
        .as_deref()
        .or(meta.arxiv_id.as_deref())
        .unwrap_or(&meta.title);

    // Paper entity (document)
    let paper_id = make_id(canonical_id, "paper");
    let year_str = meta.year.map(|y| format!(" ({y})")).unwrap_or_default();
    let venue_str = meta
        .venue
        .as_deref()
        .map(|v| format!(". {v}"))
        .unwrap_or_default();

    entities.push(Entity {
        id: paper_id.clone(),
        name: meta.title.clone(),
        entity_type: "document".to_string(),
        context: format!(
            "{}{year_str}{venue_str}\n\n{}\n\nsource_type: academic_paper | source_url: {}",
            meta.title,
            truncate(&meta.abstract_text, 1000),
            meta.source_url,
        ),
        extractor_schema_version: Some(EXTRACTOR_SCHEMA_VERSION),
        ..Default::default()
    });

    // Author entities + wrote edges
    let mut author_ids: Vec<String> = Vec::new();
    for author in &meta.authors {
        let author_id = make_id(&author.name, "person");
        let affil = author
            .affiliation
            .as_deref()
            .map(|a| format!(", {a}"))
            .unwrap_or_default();
        entities.push(Entity {
            id: author_id.clone(),
            name: author.name.clone(),
            entity_type: "person".to_string(),
            context: format!(
                "{}{affil}\nsource_type: academic_paper | source_url: {}",
                author.name, meta.source_url,
            ),
            extractor_schema_version: Some(EXTRACTOR_SCHEMA_VERSION),
            ..Default::default()
        });
        edges.push(Edge {
            src_id: author_id.clone(),
            dst_id: paper_id.clone(),
            edge_type: "wrote".to_string(),
            weight: 1.0,
            ..Default::default()
        });

        // Affiliation edges
        if let Some(ref affil_name) = author.affiliation {
            let org_id = make_id(affil_name, "org");
            entities.push(Entity {
                id: org_id.clone(),
                name: affil_name.clone(),
                entity_type: "org".to_string(),
                context: format!(
                    "{affil_name}\nsource_type: academic_paper | source_url: {}",
                    meta.source_url,
                ),
                extractor_schema_version: Some(EXTRACTOR_SCHEMA_VERSION),
                ..Default::default()
            });
            edges.push(Edge {
                src_id: author_id.clone(),
                dst_id: org_id,
                edge_type: "affiliated_with".to_string(),
                weight: 1.0,
                ..Default::default()
            });
        }
        author_ids.push(author_id);
    }

    // Concept entities from keywords + abstract
    let concepts = extract_concepts_from_paper(meta);
    for concept in &concepts {
        let concept_id = make_id(concept, "concept");
        entities.push(Entity {
            id: concept_id.clone(),
            name: concept.clone(),
            entity_type: "concept".to_string(),
            context: format!(
                "{concept} — discussed in \"{}\"\nsource_type: academic_paper | source_url: {}",
                meta.title, meta.source_url,
            ),
            extractor_schema_version: Some(EXTRACTOR_SCHEMA_VERSION),
            ..Default::default()
        });
        edges.push(Edge {
            src_id: paper_id.clone(),
            dst_id: concept_id,
            edge_type: "discusses".to_string(),
            weight: 0.8,
            ..Default::default()
        });
    }

    // Section entities
    for section in &meta.sections {
        let section_id = make_id(&format!("{}::{}", canonical_id, section.heading), "section");
        entities.push(Entity {
            id: section_id.clone(),
            name: section.heading.clone(),
            entity_type: "section".to_string(),
            context: format!(
                "{}: {}\nsource_type: academic_paper | source_url: {}",
                section.heading,
                truncate(&section.text, 500),
                meta.source_url,
            ),
            extractor_schema_version: Some(EXTRACTOR_SCHEMA_VERSION),
            ..Default::default()
        });
        edges.push(Edge {
            src_id: paper_id.clone(),
            dst_id: section_id,
            edge_type: "contains".to_string(),
            weight: 1.0,
            ..Default::default()
        });
    }

    // Reference edges (cited papers)
    for reference in &meta.references {
        let ref_canonical = reference.doi.as_deref().unwrap_or(&reference.title);
        let ref_id = make_id(ref_canonical, "paper");
        let ref_year = reference
            .year
            .map(|y| format!(" ({y})"))
            .unwrap_or_default();
        entities.push(Entity {
            id: ref_id.clone(),
            name: reference.title.clone(),
            entity_type: "document".to_string(),
            context: format!("{}{ref_year}\nsource_type: academic_paper", reference.title,),
            extractor_schema_version: Some(EXTRACTOR_SCHEMA_VERSION),
            ..Default::default()
        });
        edges.push(Edge {
            src_id: paper_id.clone(),
            dst_id: ref_id,
            edge_type: "references".to_string(),
            weight: 0.5,
            ..Default::default()
        });
    }

    // Deduplicate entities by ID
    let mut seen_ids: HashSet<String> = HashSet::new();
    entities.retain(|e| seen_ids.insert(e.id.clone()));

    // Deduplicate edges
    let mut seen_edges: HashSet<(String, String, String)> = HashSet::new();
    edges.retain(|e| seen_edges.insert((e.src_id.clone(), e.edge_type.clone(), e.dst_id.clone())));

    let documents = entities
        .iter()
        .filter(|e| e.entity_type == "document")
        .count();
    let sections_count = entities
        .iter()
        .filter(|e| e.entity_type == "section")
        .count();
    let contains_edges = edges.iter().filter(|e| e.edge_type == "contains").count();

    IngestReport {
        path: meta.source_url.clone(),
        language: "academic_paper".to_string(),
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
            total_entities: entities.len(),
            total_edges: edges.len(),
        },
        entities,
        edges,
    }
}

/// Build an [`IngestReport`] from paper metadata after cleansing untrusted text.
///
/// Paper PDFs/HTML are adversarial input: abstracts, section text, titles, and
/// references can contain prompt-injection strings that should never be stored
/// as memory. Unlike the generic report sanitizer, this pass removes suspicious
/// spans before graph construction so a poisoned abstract does not cause the
/// whole paper entity to disappear.
pub fn build_sanitized_paper_graph(meta: &PaperMetadata) -> IngestReport {
    let (clean_meta, metadata_warnings) = cleanse_paper_metadata(meta);
    let report = build_paper_graph(&clean_meta);

    // Defense-in-depth: the metadata cleanser removes suspicious spans while
    // preserving safe paper content; the generic report sanitizer still catches
    // hidden HTML, Unicode trickery, encoded blobs, and any pattern that made it
    // through graph construction.
    let (sanitized, report_warnings) = crate::sanitize::sanitize_report(report);
    let warnings = metadata_warnings + report_warnings;
    if warnings > 0 {
        eprintln!(
            "[forge] paper sanitization: {} warnings, {} entities after filtering",
            warnings, sanitized.summary.total_entities
        );
    }
    sanitized
}

fn cleanse_paper_metadata(meta: &PaperMetadata) -> (PaperMetadata, usize) {
    let mut warnings = 0usize;
    let mut clean = meta.clone();

    clean.title = cleanse_untrusted_paper_text(&clean.title, &mut warnings);
    clean.abstract_text = cleanse_untrusted_paper_text(&clean.abstract_text, &mut warnings);
    clean.venue = clean
        .venue
        .map(|v| cleanse_untrusted_paper_text(&v, &mut warnings));
    clean.source_url = cleanse_untrusted_paper_text(&clean.source_url, &mut warnings);

    for author in &mut clean.authors {
        author.name = cleanse_untrusted_paper_text(&author.name, &mut warnings);
        author.affiliation = author
            .affiliation
            .take()
            .map(|a| cleanse_untrusted_paper_text(&a, &mut warnings));
    }

    for reference in &mut clean.references {
        reference.title = cleanse_untrusted_paper_text(&reference.title, &mut warnings);
        for author in &mut reference.authors {
            *author = cleanse_untrusted_paper_text(author, &mut warnings);
        }
    }

    for section in &mut clean.sections {
        section.heading = cleanse_untrusted_paper_text(&section.heading, &mut warnings);
        section.text = cleanse_untrusted_paper_text(&section.text, &mut warnings);
    }

    for keyword in &mut clean.keywords {
        *keyword = cleanse_untrusted_paper_text(keyword, &mut warnings);
    }

    (clean, warnings)
}

fn cleanse_untrusted_paper_text(text: &str, warning_count: &mut usize) -> String {
    let first = crate::sanitize::sanitize_web_content(text);
    *warning_count += first.warnings.len();

    let stripped = strip_prompt_injection_spans(&first.clean);
    if stripped != first.clean {
        *warning_count += 1;
    }

    let second = crate::sanitize::sanitize_web_content(&stripped);
    *warning_count += second.warnings.len();
    second.clean.trim().to_string()
}

fn strip_prompt_injection_spans(text: &str) -> String {
    let mut clean = text.to_string();
    let patterns = [
        r"(?i)\bignore\s+(all\s+)?(previous|prior|above|earlier)\s+(instructions?|context|prompts?|rules?)[^.!?\n]*(?:[.!?]|$)",
        r"(?i)\bdisregard\s+(all\s+)?(previous|prior|above)\s+(instructions?|context)[^.!?\n]*(?:[.!?]|$)",
        r"(?i)\bforget\s+(everything|all|what)\s+(you|about)[^.!?\n]*(?:[.!?]|$)",
        r"(?i)\byou\s+are\s+(now|actually)\s+[^.!?\n]{0,160}(?:[.!?]|$)",
        r"(?i)\b(pretend\s+(you\s+are|to\s+be)|act\s+as\s+(if\s+you\s+are|a))\s+[^.!?\n]{0,160}(?:[.!?]|$)",
        r"(?i)\b(do\s+anything\s+now|DAN\s+mode|jailbreak|bypass\s+(safety|filter|restriction))[^.!?\n]*(?:[.!?]|$)",
        r"(?i)\b(output|print|show|display|reveal|repeat)\s+(the\s+)?(system\s+prompt|instructions|your\s+rules)[^.!?\n]*(?:[.!?]|$)",
        r"(?i)\bwhat\s+are\s+your\s+(instructions|rules|guidelines|system\s+prompt)[^.!?\n]*(?:[.!?]|$)",
        r"(?i)<\|?(system|user|assistant|endoftext|im_start|im_end)\|?>",
        r"(?i)\[INST\]|\[/INST\]|<<SYS>>|<</SYS>>",
    ];

    for pattern in patterns {
        if let Ok(re) = Regex::new(pattern) {
            clean = re.replace_all(&clean, " ").to_string();
        }
    }

    Regex::new(r"\s+")
        .expect("whitespace regex compiles")
        .replace_all(&clean, " ")
        .trim()
        .to_string()
}

/// Top-level: detect source, fetch metadata, build graph.
pub fn extract_paper(input: &str) -> Result<IngestReport> {
    let source = detect_source(input);
    eprintln!("[forge] Detected source: {source:?}");
    let metadata = fetch_metadata(&source)?;
    eprintln!(
        "[forge] Parsed: \"{}\" — {} authors, {} references, {} keywords",
        metadata.title,
        metadata.authors.len(),
        metadata.references.len(),
        metadata.keywords.len(),
    );
    Ok(build_sanitized_paper_graph(&metadata))
}

// ---------------------------------------------------------------------------
// HTML metadata helpers
// ---------------------------------------------------------------------------

fn extract_meta(html: &str, name: &str) -> Option<String> {
    let re = Regex::new(&format!(
        r#"(?i)<meta\s+name\s*=\s*["']{name}["'][^>]*content\s*=\s*["']([^"']+)["']"#
    ))
    .ok()?;
    re.captures(html).map(|c| decode_html(&c[1]))
}

fn extract_all_meta(html: &str, name: &str) -> Vec<String> {
    let re = Regex::new(&format!(
        r#"(?i)<meta\s+name\s*=\s*["']{name}["'][^>]*content\s*=\s*["']([^"']+)["']"#
    ))
    .unwrap();
    re.captures_iter(html).map(|c| decode_html(&c[1])).collect()
}

fn extract_og(html: &str, property: &str) -> Option<String> {
    let re = Regex::new(&format!(
        r#"(?i)<meta\s+property\s*=\s*["']og:{property}["'][^>]*content\s*=\s*["']([^"']+)["']"#
    ))
    .ok()?;
    re.captures(html).map(|c| decode_html(&c[1]))
}

fn extract_html_title(html: &str) -> Option<String> {
    let re = Regex::new(r"(?is)<title[^>]*>(.*?)</title>").ok()?;
    let title = re.captures(html).map(|c| c[1].trim().to_string())?;
    if title.is_empty() {
        None
    } else {
        Some(decode_html(&title))
    }
}

fn extract_between(html: &str, start: &str, end: &str) -> Option<String> {
    let start_idx = html.find(start)? + start.len();
    let end_idx = html[start_idx..].find(end)? + start_idx;
    Some(html[start_idx..end_idx].to_string())
}

fn decode_html(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&nbsp;", " ")
}

// ---------------------------------------------------------------------------
// PDF text extraction helpers
// ---------------------------------------------------------------------------

fn extract_title_from_text(text: &str) -> String {
    // First non-empty line that's reasonably long is often the title
    text.lines()
        .map(|l| l.trim())
        .find(|l| l.len() > 10 && l.len() < 300)
        .unwrap_or("Untitled")
        .to_string()
}

fn extract_authors_from_text(text: &str) -> Vec<Author> {
    // Look for lines after title that contain comma-separated names
    // before the abstract. Heuristic: lines with multiple commas and no periods.
    let lines: Vec<&str> = text.lines().map(|l| l.trim()).collect();
    let mut authors = Vec::new();

    for line in lines.iter().take(20) {
        if line.len() > 300 || line.is_empty() {
            continue;
        }
        // Lines with commas that look like "Name1, Name2, Name3"
        if line.contains(',') && !line.contains('.') && line.len() < 200 {
            for name in line.split(',') {
                let name = name.trim();
                if name.len() >= 3 && name.len() <= 60 && name.contains(' ') {
                    authors.push(Author {
                        name: normalize_author_name(name),
                        affiliation: None,
                    });
                }
            }
            if !authors.is_empty() {
                break;
            }
        }
    }
    authors
}

fn extract_abstract_from_text(text: &str) -> String {
    let lower = text.to_lowercase();
    if let Some(start) = lower.find("abstract") {
        let after = &text[start + 8..];
        // Take text until "introduction" or "1." or 2000 chars
        let end = after
            .to_lowercase()
            .find("introduction")
            .or_else(|| after.find("\n1."))
            .or_else(|| after.find("\n1 "))
            .unwrap_or_else(|| after.len().min(2000));
        after[..end].trim().to_string()
    } else {
        String::new()
    }
}

fn extract_sections_from_text(text: &str) -> Vec<PaperSection> {
    // Look for numbered section headings: "1. Introduction", "2.1 Background"
    let re = Regex::new(r"(?m)^(\d+(?:\.\d+)?)\s+([A-Z][^\n]{3,80})$").unwrap();
    let mut sections = Vec::new();
    let matches: Vec<_> = re.captures_iter(text).collect();

    for (i, cap) in matches.iter().enumerate() {
        let heading = format!("{} {}", &cap[1], &cap[2]);
        let level: u8 = if cap[1].contains('.') { 2 } else { 1 };
        let start = cap.get(0).unwrap().end();
        let end = matches
            .get(i + 1)
            .map(|next| next.get(0).unwrap().start())
            .unwrap_or_else(|| text.len().min(start + 5000));
        let section_text = text[start..end].trim().to_string();

        sections.push(PaperSection {
            heading,
            level,
            text: truncate(&section_text, 1000).to_string(),
        });
    }
    sections
}

fn extract_keywords_from_text(text: &str) -> Vec<String> {
    let lower = text.to_lowercase();
    if let Some(start) = lower.find("keywords") {
        let after = &text[start + 8..];
        // Skip leading punctuation/whitespace (e.g., "Keywords: " or "Keywords—")
        let after = after.trim_start_matches(|c: char| c == ':' || c == '—' || c.is_whitespace());
        let end = after
            .find('\n')
            .unwrap_or(after.len())
            .min(after.len())
            .min(200);
        let line = after[..end].trim();
        // Split by comma, semicolon, or bullet
        line.split([',', ';', '·'])
            .map(|s| s.trim().to_string())
            .filter(|s| s.len() >= 2 && s.len() <= 80)
            .collect()
    } else {
        Vec::new()
    }
}

fn extract_year_from_text(text: &str) -> Option<u16> {
    let re = Regex::new(r"\b(20[0-2]\d|19[89]\d)\b").unwrap();
    re.captures(text).and_then(|c| c[1].parse().ok())
}

fn extract_doi_from_text(text: &str) -> Option<String> {
    let re = Regex::new(r"(10\.\d{4,}/[^\s]+)").unwrap();
    re.captures(text).map(|c| c[1].to_string())
}

// ---------------------------------------------------------------------------
// Concept extraction from paper metadata
// ---------------------------------------------------------------------------

fn extract_concepts_from_paper(meta: &PaperMetadata) -> Vec<String> {
    let mut concepts: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();

    // Keywords are the highest signal
    for kw in &meta.keywords {
        let key = kw.to_lowercase();
        if seen.insert(key) {
            concepts.push(kw.clone());
        }
    }

    // Capitalized multi-word phrases from abstract
    let re = Regex::new(r"\b([A-Z][a-z]+(?:\s+[A-Z][a-z]+)+)\b").unwrap();
    for cap in re.captures_iter(&meta.abstract_text) {
        let phrase = cap[1].to_string();
        if phrase.len() >= 4 && phrase.len() <= 80 {
            let key = phrase.to_lowercase();
            if seen.insert(key) {
                concepts.push(phrase);
            }
        }
    }

    // Section headings as concepts (exclude generic ones)
    let generic = [
        "introduction",
        "conclusion",
        "abstract",
        "references",
        "acknowledgments",
        "related work",
        "background",
        "methodology",
        "methods",
        "results",
        "discussion",
        "appendix",
        "future work",
    ];
    for section in &meta.sections {
        let key = section.heading.to_lowercase();
        // Strip leading numbers
        let clean: String = key
            .trim_start_matches(|c: char| c.is_ascii_digit() || c == '.')
            .trim()
            .to_string();
        if !generic.contains(&clean.as_str()) && clean.len() >= 3 && seen.insert(clean.clone()) {
            concepts.push(section.heading.clone());
        }
    }

    concepts.truncate(MAX_CONCEPTS);
    concepts
}

// ---------------------------------------------------------------------------
// Browser integration
// ---------------------------------------------------------------------------

/// Open a URL in the system default browser.
///
/// Failures are best-effort (the UX has a URL in the terminal anyway) but
/// they must be visible — otherwise a missing `open` / `xdg-open` binary
/// looks identical to the browser silently ignoring the call.
fn open_in_browser(url: &str) {
    let (cmd, args): (&str, Vec<&str>) = if cfg!(target_os = "macos") {
        ("open", vec![url])
    } else if cfg!(target_os = "linux") {
        ("xdg-open", vec![url])
    } else if cfg!(target_os = "windows") {
        ("cmd", vec!["/C", "start", url])
    } else {
        eprintln!("[forge paper] no browser-open strategy for this platform; URL: {url}");
        return;
    };
    if let Err(e) = Command::new(cmd).args(&args).spawn() {
        eprintln!(
            "[forge paper] failed to launch '{cmd}' to open {url}: {e} \
             (install it, or open the URL manually)"
        );
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn make_id(input: &str, kind: &str) -> String {
    let combined = format!("{kind}::{input}");
    Uuid::new_v5(&PAPER_NS, combined.as_bytes()).to_string()
}

/// Decode URL percent-encoding (basic: +, %20, %26, etc.)
fn urldecode(s: &str) -> String {
    let s = s.replace('+', " ");
    let mut result = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '%' {
            let hex: String = chars.by_ref().take(2).collect();
            if let Ok(byte) = u8::from_str_radix(&hex, 16) {
                result.push(byte as char);
            } else {
                result.push('%');
                result.push_str(&hex);
            }
        } else {
            result.push(c);
        }
    }
    result
}

/// Normalize author names to "First Last" format for consistent dedup.
/// Handles: "Last, First" → "First Last", "First Last" → "First Last"
fn normalize_author_name(name: &str) -> String {
    let name = name.trim();
    if let Some((last, first)) = name.split_once(", ") {
        let first = first.trim();
        let last = last.trim();
        if !first.is_empty() && !last.is_empty() {
            return format!("{first} {last}");
        }
    }
    name.to_string()
}

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
    fn test_detect_arxiv() {
        assert!(matches!(
            detect_source("https://arxiv.org/abs/2401.12345"),
            PaperSource::Arxiv { arxiv_id } if arxiv_id == "2401.12345"
        ));
        assert!(matches!(
            detect_source("https://arxiv.org/pdf/2401.12345v2"),
            PaperSource::Arxiv { arxiv_id } if arxiv_id == "2401.12345v2"
        ));
    }

    #[test]
    fn test_detect_doi() {
        assert!(matches!(
            detect_source("doi:10.1234/example.2024"),
            PaperSource::Doi { doi } if doi == "10.1234/example.2024"
        ));
        assert!(matches!(
            detect_source("https://doi.org/10.1145/3580305.3599498"),
            PaperSource::Doi { doi } if doi == "10.1145/3580305.3599498"
        ));
    }

    #[test]
    fn test_detect_ieee() {
        assert!(matches!(
            detect_source("https://ieeexplore.ieee.org/document/9999999"),
            PaperSource::Ieee { .. }
        ));
    }

    #[test]
    fn test_detect_acm() {
        assert!(matches!(
            detect_source("https://dl.acm.org/doi/10.1145/3580305.3599498"),
            PaperSource::Acm { .. }
        ));
    }

    #[test]
    fn test_detect_biorxiv() {
        assert!(matches!(
            detect_source("https://www.biorxiv.org/content/10.1101/2024.01.01.123456v1"),
            PaperSource::BioRxiv { .. }
        ));
    }

    #[test]
    fn test_detect_pubmed() {
        assert!(matches!(
            detect_source("https://pubmed.ncbi.nlm.nih.gov/12345678/"),
            PaperSource::PubMed { pmid } if pmid == "12345678"
        ));
    }

    #[test]
    fn test_detect_local_pdf() {
        assert!(matches!(
            detect_source("./papers/my-paper.pdf"),
            PaperSource::LocalPdf { .. }
        ));
    }

    #[test]
    fn test_detect_semantic_scholar() {
        assert!(matches!(
            detect_source("https://api.semanticscholar.org/graph/v1/paper/abc123"),
            PaperSource::SemanticScholar { .. }
        ));
    }

    #[test]
    fn test_detect_generic_url() {
        assert!(matches!(
            detect_source("https://proceedings.mlr.press/v202/paper.html"),
            PaperSource::GenericUrl { .. }
        ));
    }

    #[test]
    fn test_build_paper_graph() {
        let meta = PaperMetadata {
            title: "Test Paper: A Study".to_string(),
            authors: vec![
                Author {
                    name: "Alice Researcher".to_string(),
                    affiliation: Some("MIT".to_string()),
                },
                Author {
                    name: "Bob Scientist".to_string(),
                    affiliation: None,
                },
            ],
            abstract_text: "We study Knowledge Graphs and Retrieval Augmented Generation."
                .to_string(),
            year: Some(2024),
            venue: Some("NeurIPS 2024".to_string()),
            doi: Some("10.1234/test.2024".to_string()),
            arxiv_id: None,
            source_url: "https://example.com/paper".to_string(),
            references: vec![Reference {
                title: "Prior Work".to_string(),
                authors: vec!["Charlie Prior".to_string()],
                year: Some(2023),
                doi: Some("10.1234/prior.2023".to_string()),
            }],
            sections: vec![PaperSection {
                heading: "1 Memory Architecture".to_string(),
                level: 1,
                text: "We propose a novel architecture.".to_string(),
            }],
            keywords: vec!["knowledge graphs".to_string(), "RAG".to_string()],
        };

        let report = build_paper_graph(&meta);

        assert_eq!(report.language, "academic_paper");

        // Should have: paper + 2 authors + 1 org + concepts + 1 section + 1 reference
        assert!(report.summary.total_entities >= 6);
        assert!(report.summary.total_edges >= 4); // 2 wrote + 1 affiliated + concepts + section + ref

        // Check entity types
        assert!(report
            .entities
            .iter()
            .any(|e| e.entity_type == "document" && e.name == "Test Paper: A Study"));
        assert!(report
            .entities
            .iter()
            .any(|e| e.entity_type == "person" && e.name == "Alice Researcher"));
        assert!(report
            .entities
            .iter()
            .any(|e| e.entity_type == "org" && e.name == "MIT"));

        // Check edge types
        assert!(report.edges.iter().any(|e| e.edge_type == "wrote"));
        assert!(report
            .edges
            .iter()
            .any(|e| e.edge_type == "affiliated_with"));
        assert!(report.edges.iter().any(|e| e.edge_type == "references"));
        assert!(report.edges.iter().any(|e| e.edge_type == "contains"));
        assert!(report.edges.iter().any(|e| e.edge_type == "discusses"));

        // Context should include provenance
        assert!(report.entities[0]
            .context
            .contains("source_type: academic_paper"));
    }

    #[test]
    fn test_deterministic_ids() {
        let meta = PaperMetadata {
            title: "Same Paper".to_string(),
            authors: vec![],
            abstract_text: String::new(),
            year: None,
            venue: None,
            doi: Some("10.1234/same".to_string()),
            arxiv_id: None,
            source_url: "https://example.com".to_string(),
            references: vec![],
            sections: vec![],
            keywords: vec![],
        };

        let r1 = build_paper_graph(&meta);
        let r2 = build_paper_graph(&meta);
        assert_eq!(r1.entities[0].id, r2.entities[0].id);
    }

    #[test]
    fn test_concept_extraction() {
        let meta = PaperMetadata {
            title: "Test".to_string(),
            authors: vec![],
            abstract_text: "We study Large Language Models and Retrieval Augmented Generation for Knowledge Graphs.".to_string(),
            year: None,
            venue: None,
            doi: None,
            arxiv_id: None,
            source_url: String::new(),
            references: vec![],
            sections: vec![],
            keywords: vec!["transformer".to_string(), "attention".to_string()],
        };

        let concepts = extract_concepts_from_paper(&meta);
        assert!(concepts.contains(&"transformer".to_string()));
        assert!(concepts.contains(&"attention".to_string()));
        // Should also pick up capitalized phrases from abstract
        assert!(concepts.iter().any(|c| c.contains("Language Models")));
    }

    #[test]
    fn test_pdf_text_helpers() {
        let text = "A Novel Method for Graph Learning\n\nAlice Smith, Bob Jones\n\nAbstract\nWe present a new approach.\n\n1 Introduction\nGraphs are everywhere.\n\n2 Methods\nWe use GNNs.\n\nKeywords: graph neural networks, representation learning";

        assert_eq!(
            extract_title_from_text(text),
            "A Novel Method for Graph Learning"
        );

        let authors = extract_authors_from_text(text);
        assert_eq!(authors.len(), 2);
        assert_eq!(authors[0].name, "Alice Smith");

        let abs = extract_abstract_from_text(text);
        assert!(abs.contains("new approach"));

        let sections = extract_sections_from_text(text);
        assert!(sections.len() >= 2);

        let kw = extract_keywords_from_text(text);
        assert!(kw.contains(&"graph neural networks".to_string()));
    }
}
