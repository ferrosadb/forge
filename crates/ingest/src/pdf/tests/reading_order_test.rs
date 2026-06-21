//! Tests for XY-Cut++ reading order reconstruction.

use crate::pdf::element::{BoundingBox, ElementType, PdfElement};
use crate::pdf::reading_order::sort_reading_order;

fn make_element(id: u64, left: f64, bottom: f64, right: f64, top: f64) -> PdfElement {
    PdfElement {
        id,
        element_type: ElementType::Paragraph,
        page_number: 1,
        bounding_box: BoundingBox {
            left,
            bottom,
            right,
            top,
        },
        text: format!("element-{}", id),
        font: None,
        font_size: None,
        level: None,
        table_shape: None,
    }
}

/// Helper: extract IDs in order.
fn ids(elements: &[PdfElement]) -> Vec<u64> {
    elements.iter().map(|e| e.id).collect()
}

#[test]
fn test_two_column_sorting() {
    // Page: 600 x 800 points.
    // Two columns, left [0, 280], right [320, 600].
    // Three rows of paragraphs in each column.
    let mut elements = vec![
        make_element(1, 0.0, 700.0, 280.0, 780.0),   // left col top
        make_element(2, 0.0, 500.0, 280.0, 600.0),   // left col mid
        make_element(3, 0.0, 300.0, 280.0, 450.0),   // left col bottom
        make_element(4, 320.0, 700.0, 600.0, 780.0), // right col top
        make_element(5, 320.0, 500.0, 600.0, 600.0), // right col mid
        make_element(6, 320.0, 300.0, 600.0, 450.0), // right col bottom
    ];

    let sorted = sort_reading_order(&mut elements);
    let order = ids(&sorted);

    // Reading order: left column top-to-bottom, then right column top-to-bottom.
    assert_eq!(order, vec![1, 2, 3, 4, 5, 6]);
}

#[test]
fn test_full_width_header_preserved() {
    // Page: 600 x 800.
    // A full-width title at the top, then two columns below.
    let mut elements = vec![
        make_element(10, 0.0, 700.0, 600.0, 780.0), // full-width title
        make_element(20, 0.0, 500.0, 280.0, 600.0), // left col top
        make_element(30, 0.0, 300.0, 280.0, 450.0), // left col bottom
        make_element(40, 320.0, 500.0, 600.0, 600.0), // right col top
        make_element(50, 320.0, 300.0, 600.0, 450.0), // right col bottom
    ];

    let sorted = sort_reading_order(&mut elements);
    let order = ids(&sorted);

    // Title must remain first, then left column, then right column.
    assert_eq!(order, vec![10, 20, 30, 40, 50]);
}

#[test]
fn test_single_column_trivial() {
    let mut elements = vec![
        make_element(1, 0.0, 500.0, 600.0, 600.0),
        make_element(2, 0.0, 300.0, 600.0, 450.0),
        make_element(3, 0.0, 100.0, 600.0, 250.0),
    ];

    let sorted = sort_reading_order(&mut elements);
    let order = ids(&sorted);
    assert_eq!(order, vec![1, 2, 3]);
}

#[test]
fn test_empty_input() {
    let mut empty: Vec<PdfElement> = vec![];
    let sorted = sort_reading_order(&mut empty);
    assert!(sorted.is_empty());
}
