//! XY-Cut++ reading-order reconstruction for multi-column PDF layouts.
//!
//! Algorithm overview:
//! 1. Pre-mask cross-layout elements (headers/footers spanning columns).
//! 2. Compute density ratio for adaptive axis selection.
//! 3. Recursively segment using gap detection (min gap 5.0 PDF points).
//! 4. Fallback: sort by Y then X.
//! 5. Merge cross-layout elements back at top/bottom of page.

use crate::pdf::element::{BoundingBox, PdfElement};

const MIN_GAP: f64 = 5.0;
const CROSS_LAYOUT_WIDTH_RATIO: f64 = 0.85;
const TOP_BOTTOM_MARGIN_RATIO: f64 = 0.20;

/// Sort a slice of page elements into reading order using XY-Cut++.
///
/// Cross-layout elements (headers/footers) are detected by width heuristic,
/// extracted, and re-inserted at the top or bottom of the page. The remaining
/// elements are recursively segmented using adaptive X/Y cuts, falling back
/// to Y-then-X sort when no usable gap is found.
pub fn sort_reading_order(elements: &mut [PdfElement]) -> Vec<PdfElement> {
    if elements.is_empty() {
        return Vec::new();
    }

    let page_bbox = bounding_box_of(elements);
    let page_width = page_bbox.width();
    let page_height = page_bbox.height();

    if page_width <= 0.0 || page_height <= 0.0 {
        // Degenerate case: fall back to simple sort.
        let mut sorted: Vec<PdfElement> = elements.to_vec();
        sorted.sort_by(cmp_y_then_x);
        return sorted;
    }

    let threshold = CROSS_LAYOUT_WIDTH_RATIO * page_width;

    let mut cross: Vec<PdfElement> = Vec::new();
    let mut body: Vec<PdfElement> = Vec::new();

    for el in elements.iter().cloned() {
        if el.bounding_box.width() >= threshold {
            cross.push(el);
        } else {
            body.push(el);
        }
    }

    // Recursively sort the body elements.
    let mut sorted_body = xy_cut(body);

    // Merge cross-layout elements.
    let mut top: Vec<PdfElement> = Vec::new();
    let mut bottom: Vec<PdfElement> = Vec::new();
    let mut middle: Vec<PdfElement> = Vec::new();

    let top_threshold = page_bbox.bottom + TOP_BOTTOM_MARGIN_RATIO * page_height;
    let bottom_threshold = page_bbox.top - TOP_BOTTOM_MARGIN_RATIO * page_height;

    for el in cross {
        let cy = el.bounding_box.center_y();
        // PDF coordinates: Y increases upward (bottom-left origin).
        // High Y = top of page, low Y = bottom of page.
        if cy >= bottom_threshold {
            top.push(el);
        } else if cy <= top_threshold {
            bottom.push(el);
        } else {
            middle.push(el);
        }
    }

    // Sort each cross-layout bucket by Y then X for stable ordering.
    top.sort_by(cmp_y_then_x);
    bottom.sort_by(cmp_y_then_x);
    middle.sort_by(cmp_y_then_x);

    let mut result = Vec::with_capacity(elements.len());
    result.extend(top);
    result.append(&mut sorted_body);
    result.extend(middle);
    result.extend(bottom);
    result
}

// ---------------------------------------------------------------------------
// XY-Cut recursion
// ---------------------------------------------------------------------------

fn xy_cut(elements: Vec<PdfElement>) -> Vec<PdfElement> {
    if elements.len() <= 1 {
        return elements;
    }

    let bbox = bounding_box_of(&elements);
    let width = bbox.width();
    let height = bbox.height();

    if width <= 0.0 || height <= 0.0 {
        let mut sorted = elements;
        sorted.sort_by(cmp_y_then_x);
        return sorted;
    }

    // Density heuristic: prefer the axis where elements are *more* densely packed.
    let total_width: f64 = elements.iter().map(|e| e.bounding_box.width()).sum();
    let total_height: f64 = elements.iter().map(|e| e.bounding_box.height()).sum();

    let density_x = total_width / width;
    let density_y = total_height / height;

    let prefer_x = density_x >= density_y; // cut vertically (X-cut)

    if prefer_x {
        if let Some((left, right)) = try_x_cut(&elements) {
            let mut result = xy_cut(left);
            result.append(&mut xy_cut(right));
            return result;
        }
        if let Some((top, bottom)) = try_y_cut(&elements) {
            let mut result = xy_cut(top);
            result.append(&mut xy_cut(bottom));
            return result;
        }
    } else {
        if let Some((top, bottom)) = try_y_cut(&elements) {
            let mut result = xy_cut(top);
            result.append(&mut xy_cut(bottom));
            return result;
        }
        if let Some((left, right)) = try_x_cut(&elements) {
            let mut result = xy_cut(left);
            result.append(&mut xy_cut(right));
            return result;
        }
    }

    // Fallback: sort by Y then X.
    let mut sorted = elements;
    sorted.sort_by(cmp_y_then_x);
    sorted
}

/// Attempt a vertical cut (X-cut) returning left and right groups.
fn try_x_cut(elements: &[PdfElement]) -> Option<(Vec<PdfElement>, Vec<PdfElement>)> {
    let mut indexed: Vec<(usize, &PdfElement)> = elements.iter().enumerate().collect();
    indexed.sort_by(|a, b| {
        a.1.bounding_box
            .center_x()
            .partial_cmp(&b.1.bounding_box.center_x())
            .unwrap()
    });

    let mut best_gap = MIN_GAP;
    let mut best_idx: Option<usize> = None;

    for i in 1..indexed.len() {
        let prev = indexed[i - 1].1;
        let curr = indexed[i].1;
        let gap = curr.bounding_box.left - prev.bounding_box.right;
        if gap >= best_gap {
            best_gap = gap;
            best_idx = Some(i);
        }
    }

    let split_idx = best_idx?;

    let left_ids: std::collections::HashSet<usize> =
        indexed[..split_idx].iter().map(|(idx, _)| *idx).collect();

    let mut left = Vec::new();
    let mut right = Vec::new();
    for (idx, el) in elements.iter().enumerate() {
        if left_ids.contains(&idx) {
            left.push(el.clone());
        } else {
            right.push(el.clone());
        }
    }

    // Only accept cuts that actually split both sides.
    if left.is_empty() || right.is_empty() {
        return None;
    }

    Some((left, right))
}

/// Attempt a horizontal cut (Y-cut) returning top and bottom groups.
fn try_y_cut(elements: &[PdfElement]) -> Option<(Vec<PdfElement>, Vec<PdfElement>)> {
    let mut indexed: Vec<(usize, &PdfElement)> = elements.iter().enumerate().collect();
    indexed.sort_by(|a, b| {
        a.1.bounding_box
            .center_y()
            .partial_cmp(&b.1.bounding_box.center_y())
            .unwrap()
    });

    let mut best_gap = MIN_GAP;
    let mut best_idx: Option<usize> = None;

    for i in 1..indexed.len() {
        let prev = indexed[i - 1].1;
        let curr = indexed[i].1;
        let gap = curr.bounding_box.bottom - prev.bounding_box.top;
        if gap >= best_gap {
            best_gap = gap;
            best_idx = Some(i);
        }
    }

    let split_idx = best_idx?;

    // Elements after split_idx have *higher* Y (closer to top of page).
    // In reading order, higher-Y elements come first.
    let above_ids: std::collections::HashSet<usize> =
        indexed[split_idx..].iter().map(|(idx, _)| *idx).collect();

    let _below_ids: std::collections::HashSet<usize> =
        indexed[..split_idx].iter().map(|(idx, _)| *idx).collect();

    let mut above = Vec::new();
    let mut below = Vec::new();
    for (idx, el) in elements.iter().enumerate() {
        if above_ids.contains(&idx) {
            above.push(el.clone());
        } else {
            below.push(el.clone());
        }
    }

    if above.is_empty() || below.is_empty() {
        return None;
    }

    Some((above, below))
}

// ---------------------------------------------------------------------------
// Utilities
// ---------------------------------------------------------------------------

fn bounding_box_of(elements: &[PdfElement]) -> BoundingBox {
    let mut left = f64::INFINITY;
    let mut bottom = f64::INFINITY;
    let mut right = f64::NEG_INFINITY;
    let mut top = f64::NEG_INFINITY;

    for el in elements {
        left = left.min(el.bounding_box.left);
        bottom = bottom.min(el.bounding_box.bottom);
        right = right.max(el.bounding_box.right);
        top = top.max(el.bounding_box.top);
    }

    BoundingBox {
        left,
        bottom,
        right,
        top,
    }
}

fn cmp_y_then_x(a: &PdfElement, b: &PdfElement) -> std::cmp::Ordering {
    let ay = a.bounding_box.center_y();
    let by = b.bounding_box.center_y();
    let ax = a.bounding_box.center_x();
    let bx = b.bounding_box.center_x();

    // Higher Y (top of page) comes first in PDF coordinates.
    match by.partial_cmp(&ay).unwrap() {
        std::cmp::Ordering::Equal => ax.partial_cmp(&bx).unwrap(),
        other => other,
    }
}
