//! Processor pipeline for PDF element extraction.
//!
//! Provides a `PageProcessor` trait and composable processors that run
//! in sequence over a page's elements.  The entry point is
//! `process_page(page_elements, processors)` which applies each processor
//! in order and returns the final element list.

use crate::pdf::element::PdfElement;

/// A single step in the page-level extraction pipeline.
///
/// Implementors receive the current list of page elements (already in
/// reading order) and may mutate them in place or replace them.
pub trait PageProcessor {
    fn process(&self, elements: Vec<PdfElement>) -> Vec<PdfElement>;
}

/// Run a list of processors over a single page's elements.
///
/// Processors are applied in the given order; the output of one becomes
/// the input of the next.
pub fn process_page(
    mut elements: Vec<PdfElement>,
    processors: &[&dyn PageProcessor],
) -> Vec<PdfElement> {
    for p in processors {
        elements = p.process(elements);
    }
    elements
}

pub mod heading;
pub mod section;
