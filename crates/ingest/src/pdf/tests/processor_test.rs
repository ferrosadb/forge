//! Tests for the processor pipeline (heading detection + section grouping).

use crate::pdf::element::{BoundingBox, ElementType, PdfElement};
use crate::pdf::processors::heading::HeadingProcessor;
use crate::pdf::processors::section::group_sections;
use crate::pdf::processors::PageProcessor;

fn make_element(id: u64, font_size: Option<f64>, text: &str) -> PdfElement {
    PdfElement {
        id,
        element_type: ElementType::Paragraph,
        page_number: 1,
        bounding_box: BoundingBox {
            left: 0.0,
            bottom: 0.0,
            right: 100.0,
            top: 20.0,
        },
        text: text.to_string(),
        font: None,
        font_size,
        level: None,
        table_shape: None,
    }
}

#[test]
fn test_heading_detection_by_font_size() {
    let elements = vec![
        make_element(1, Some(12.0), "Body paragraph one."),
        make_element(2, Some(12.0), "Body paragraph two."),
        make_element(3, Some(18.0), "Big Heading"),
        make_element(4, Some(12.0), "Body paragraph three."),
    ];

    let processor = HeadingProcessor::new().with_ratio(1.3);
    let processed = processor.process(elements);

    assert_eq!(processed[0].element_type, ElementType::Paragraph);
    assert_eq!(processed[1].element_type, ElementType::Paragraph);
    assert_eq!(processed[2].element_type, ElementType::Heading);
    assert_eq!(processed[2].level, Some(2)); // 18/12 = 1.5 = level 2
    assert_eq!(processed[3].element_type, ElementType::Paragraph);
}

#[test]
fn test_numbered_heading_pattern() {
    let elements = vec![
        make_element(1, Some(12.0), "1. INTRODUCTION"),
        make_element(2, Some(12.0), "Some body text."),
        make_element(3, Some(12.0), "2.3 Related Work"),
        make_element(4, Some(12.0), "A) Methodology"),
        make_element(5, Some(12.0), "IV. Results"),
    ];

    let processor = HeadingProcessor::new();
    let processed = processor.process(elements);

    assert_eq!(processed[0].element_type, ElementType::Heading);
    assert_eq!(processed[0].level, Some(1)); // "1." depth=1, ratio=1 → min(1,2)=1
    assert_eq!(processed[1].element_type, ElementType::Paragraph);
    assert_eq!(processed[2].element_type, ElementType::Heading);
    assert_eq!(processed[2].level, Some(2)); // "2.3" depth=2, ratio=1 → level 2
    assert_eq!(processed[3].element_type, ElementType::Heading);
    assert_eq!(processed[4].element_type, ElementType::Heading);
}

#[test]
fn test_section_grouping() {
    let elements = vec![
        make_element(1, Some(12.0), "Body before first heading."),
        make_element(2, Some(18.0), "1. INTRODUCTION"),
        make_element(3, Some(12.0), "Intro paragraph."),
        make_element(4, Some(14.0), "2. Method"),
        make_element(5, Some(12.0), "Method paragraph."),
    ];

    // Run heading processor first, then group.
    let hp = HeadingProcessor::new().with_ratio(1.3);
    let with_headings = hp.process(elements);

    let sections = group_sections(with_headings);

    // We expect 3 sections: preamble, intro, method.
    assert_eq!(sections.len(), 3);

    // Section 0: preamble (no heading)
    assert!(sections[0].heading.is_none());
    assert_eq!(sections[0].body.len(), 1);
    assert_eq!(sections[0].body[0].id, 1);

    // Section 1: introduction
    assert!(sections[1].heading.is_some());
    assert_eq!(sections[1].heading.as_ref().unwrap().id, 2);
    assert_eq!(sections[1].body.len(), 1);
    assert_eq!(sections[1].body[0].id, 3);

    // Section 2: method
    assert!(sections[2].heading.is_some());
    assert_eq!(sections[2].heading.as_ref().unwrap().id, 4);
    assert_eq!(sections[2].body.len(), 1);
    assert_eq!(sections[2].body[0].id, 5);
}
