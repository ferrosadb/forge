//! Heading detection via font-size heuristics and numbered patterns.

use crate::pdf::element::{ElementType, PdfElement};
use regex::Regex;

/// Detect headings by:
///
/// 1. **Font-size ratio** — elements whose font size is at least
///    `min_ratio` times the median body font size on the page are
///    promoted to `Heading`.
/// 2. **Numbered heading patterns** — text matching patterns like
///    "1. INTRODUCTION", "2.3 Method", "IV. Results" is forced to
///    `Heading` regardless of font size.
///
/// Detected headings have their `level` inferred from font size
/// (larger = lower level number) and from numbering depth.
pub struct HeadingProcessor {
    /// Minimum font-size ratio above the median to qualify as a heading.
    /// Default: 1.3
    pub min_ratio: f64,
    /// Minimum absolute font size to be considered a heading.
    /// Default: 10.0
    pub min_font_size: f64,
}

impl Default for HeadingProcessor {
    fn default() -> Self {
        Self {
            min_ratio: 1.3,
            min_font_size: 10.0,
        }
    }
}

impl HeadingProcessor {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_ratio(mut self, ratio: f64) -> Self {
        self.min_ratio = ratio;
        self
    }
}

impl super::PageProcessor for HeadingProcessor {
    fn process(&self, mut elements: Vec<PdfElement>) -> Vec<PdfElement> {
        let median_body = median_font_size(&elements);

        // Pre-compile regex once per page.
        let numbered_re = numbered_heading_regex();

        for el in &mut elements {
            let is_heading_by_size = if let Some(fs) = el.font_size {
                fs >= self.min_font_size && median_body > 0.0 && fs >= median_body * self.min_ratio
            } else {
                false
            };

            let is_heading_by_pattern = numbered_re.is_match(&el.text);

            if is_heading_by_size || is_heading_by_pattern {
                el.element_type = ElementType::Heading;
                el.level = Some(infer_level(el.font_size, median_body, &el.text));
            }
        }

        elements
    }
}

/// Regex for common academic heading patterns:
///   "1. INTRODUCTION"
///   "2.3 Method"
///   "IV. Results"
///   "A. Background"
///   "1) Sub-item"
fn numbered_heading_regex() -> Regex {
    Regex::new(r"(?i)^\s*(?:[0-9]+(?:\.[0-9]+)*\.?|[A-Z][.\)]|[IVXLC]+\.)\s+\S+")
        .expect("valid regex")
}

/// Infer heading depth (1 = top-level, 2 = section, 3 = subsection…).
fn infer_level(font_size: Option<f64>, median_body: f64, text: &str) -> u8 {
    // Use explicit numbering depth first.
    let depth_from_numbering = heading_number_depth(text);

    // Use font-size tiering as fallback / modifier.
    let depth_from_size = if let Some(fs) = font_size {
        if median_body > 0.0 {
            let ratio = fs / median_body;
            if ratio >= 2.0 {
                1
            } else if ratio >= 1.5 {
                2
            } else {
                3
            }
        } else {
            2
        }
    } else {
        2
    };

    // Prefer the shallower (more important) of the two signals.
    depth_from_numbering.min(depth_from_size)
}

/// Count how many dot-separated numbers appear at the start of the text.
fn heading_number_depth(text: &str) -> u8 {
    let trimmed = text.trim();
    let prefix: String = trimmed
        .chars()
        .take_while(|c| c.is_numeric() || *c == '.')
        .collect();
    if prefix.is_empty() {
        return 2; // default for roman / single-letter headings
    }
    let parts: Vec<&str> = prefix.split('.').filter(|s| !s.is_empty()).collect();
    parts.len().clamp(1, 3) as u8
}

/// Median font size of elements that look like body text.
fn median_font_size(elements: &[PdfElement]) -> f64 {
    let mut sizes: Vec<f64> = elements
        .iter()
        .filter(|e| e.font_size.is_some() && e.element_type == ElementType::Paragraph)
        .map(|e| e.font_size.unwrap())
        .collect();

    if sizes.is_empty() {
        // Fallback: take any element with a font size.
        sizes = elements.iter().filter_map(|e| e.font_size).collect();
    }

    if sizes.is_empty() {
        return 0.0;
    }

    sizes.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let mid = sizes.len() / 2;
    if sizes.len().is_multiple_of(2) {
        (sizes[mid - 1] + sizes[mid]) / 2.0
    } else {
        sizes[mid]
    }
}
