//! Section grouping: consecutive non-heading elements become sections
//! bounded by detected headings.

use crate::pdf::element::{ElementType, PdfElement};

/// A contiguous block of page elements beginning with an optional heading.
#[derive(Debug, Clone)]
pub struct Section {
    pub heading: Option<PdfElement>,
    pub body: Vec<PdfElement>,
}

/// Group a page's elements into sections.
///
/// Every time a `Heading` element is encountered it starts a new
/// section.  The heading is stored in `section.heading`; all
/// subsequent non-heading elements are appended to `section.body`
/// until the next heading or end of page.
///
/// Initial non-heading elements before the first heading are placed
/// into a section with `heading: None` (a preamble).
pub fn group_sections(elements: Vec<PdfElement>) -> Vec<Section> {
    let mut sections: Vec<Section> = Vec::new();
    let mut current_body: Vec<PdfElement> = Vec::new();
    let mut pending_heading: Option<PdfElement> = None;

    for el in elements {
        if el.element_type == ElementType::Heading {
            // Flush the previous section.
            if pending_heading.is_some() || !current_body.is_empty() {
                sections.push(Section {
                    heading: pending_heading.take(),
                    body: std::mem::take(&mut current_body),
                });
            }
            pending_heading = Some(el);
        } else {
            current_body.push(el);
        }
    }

    // Flush the tail.
    if pending_heading.is_some() || !current_body.is_empty() {
        sections.push(Section {
            heading: pending_heading.take(),
            body: current_body,
        });
    }

    sections
}
