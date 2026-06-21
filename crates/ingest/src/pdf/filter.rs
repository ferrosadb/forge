//! PDF content filtering for trustworthy extraction.
//!
//! Detects and removes hidden text, off-page elements, suspicious layers,
//! and prompt-injection attacks embedded in PDFs.

use serde::{Deserialize, Serialize};
use std::sync::OnceLock;

use regex::Regex;

use crate::pdf::element::{ElementType, PdfElement};

/// Reason an element was filtered out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum FilterSignal {
    HiddenText,
    OffPage,
    SuspiciousLayer,
    PromptInjection,
}

/// Result of filtering a collection of PDF elements.
#[derive(Debug, Clone, PartialEq)]
pub struct FilterResult {
    pub kept: Vec<PdfElement>,
    pub removed: Vec<(PdfElement, FilterSignal)>,
}

impl FilterResult {
    pub fn empty() -> Self {
        Self {
            kept: Vec::new(),
            removed: Vec::new(),
        }
    }
}

/// US Letter page dimensions in PDF points (1 pt = 1/72 inch).
pub const US_LETTER_WIDTH: f64 = 612.0;
pub const US_LETTER_HEIGHT: f64 = 792.0;

/// Tolerance outside the page bounds before an element is considered off-page.
pub const OFF_PAGE_TOLERANCE: f64 = 10.0;

/// Minimum bounding-box dimension to consider text visible.
pub const MIN_VISIBLE_SIZE: f64 = 0.5;

/// Lazy-initialized regex set for prompt-injection patterns.
fn prompt_injection_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        let pattern = [
            r"ignore\s+(all\s+)?previous\s+instructions",
            r"ignore\s+(the\s+)?(above|below)",
            r"system\s+prompt",
            r"user\s+prompt",
            r"assistant\s+prompt",
            r"<\|im_start\|>",
            r"<\|im_end\|>",
            r"<\|system\|>",
            r"<\|user\|>",
            r"<\|assistant\|>",
            r"\bdan\b",
            r"jailbreak",
            r"prompt\s+injection",
            r"you\s+are\s+(now\s+)?(a\s+)?(helpful\s+)?assistant",
            r"from\s+now\s+on\s+you\s+are",
            r"developer\s+mode",
            r"sudo\s+mode",
            r"root\s+access",
            r"disregard\s+(all\s+)?(prior\s+)?instructions",
            r"forget\s+(all\s+)?(prior\s+)?instructions",
            r"do\s+anything\s+now",
            r"you\s+are\s+in\s+maintenance\s+mode",
        ]
        .join("|");
        Regex::new(&pattern).expect("prompt injection regex compiles")
    })
}

/// Check whether the element text contains known prompt-injection fragments.
pub fn contains_prompt_injection(element: &PdfElement) -> bool {
    let text_lower = element.text.to_lowercase();
    prompt_injection_regex().is_match(&text_lower)
}

/// Check whether the element appears to belong to a suspicious/background layer.
///
/// Heuristic: element is explicitly a watermark, or its font name suggests a
/// background / annotation / comment layer.
pub fn is_suspicious_layer(element: &PdfElement) -> bool {
    if element.element_type == ElementType::Watermark {
        return true;
    }
    if let Some(ref font) = element.font {
        let font_lower = font.to_lowercase();
        if font_lower.contains("watermark")
            || font_lower.contains("background")
            || font_lower.contains("annotation")
            || font_lower.contains("comment")
            || font_lower.contains("popup")
            || font_lower.contains("overlay")
        {
            return true;
        }
    }
    false
}

/// Filter a slice of PDF elements, returning those that should be kept and
/// those that were removed together with the reason.
///
/// Detection order (first match wins):
/// 1. Hidden text (zero-size font or bbox < 0.5)
/// 2. Off-page (outside reasonable US Letter bounds ± tolerance)
/// 3. Suspicious layer (watermark type or suspicious font name)
/// 4. Prompt injection (suspicious instruction patterns in text)
pub fn filter_pdf_elements(elements: &[PdfElement]) -> FilterResult {
    let mut result = FilterResult::empty();

    for el in elements {
        if el.is_hidden() {
            result.removed.push((el.clone(), FilterSignal::HiddenText));
            continue;
        }
        if el.is_off_page(US_LETTER_WIDTH, US_LETTER_HEIGHT) {
            result.removed.push((el.clone(), FilterSignal::OffPage));
            continue;
        }
        if is_suspicious_layer(el) {
            result
                .removed
                .push((el.clone(), FilterSignal::SuspiciousLayer));
            continue;
        }
        if contains_prompt_injection(el) {
            result
                .removed
                .push((el.clone(), FilterSignal::PromptInjection));
            continue;
        }
        result.kept.push(el.clone());
    }

    result
}
