use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct BoundingBox {
    pub left: f64,
    pub bottom: f64,
    pub right: f64,
    pub top: f64,
}

impl BoundingBox {
    pub fn width(&self) -> f64 {
        self.right - self.left
    }
    pub fn height(&self) -> f64 {
        self.top - self.bottom
    }
    pub fn center_x(&self) -> f64 {
        (self.left + self.right) / 2.0
    }
    pub fn center_y(&self) -> f64 {
        (self.bottom + self.top) / 2.0
    }
    pub fn area(&self) -> f64 {
        self.width() * self.height()
    }
    /// Horizontal overlap ratio with another box [0.0, 1.0]
    pub fn overlap_ratio_x(&self, other: &BoundingBox) -> f64 {
        let overlap = (self.right.min(other.right) - self.left.max(other.left)).max(0.0);
        let denom = self.width().max(other.width());
        if denom <= 0.0 {
            0.0
        } else {
            overlap / denom
        }
    }
    /// Vertical overlap ratio
    pub fn overlap_ratio_y(&self, other: &BoundingBox) -> f64 {
        let overlap = (self.top.min(other.top) - self.bottom.max(other.bottom)).max(0.0);
        let denom = self.height().max(other.height());
        if denom <= 0.0 {
            0.0
        } else {
            overlap / denom
        }
    }
    /// Whether this box overlaps another at all
    pub fn overlaps(&self, other: &BoundingBox) -> bool {
        self.left < other.right
            && self.right > other.left
            && self.bottom < other.top
            && self.top > other.bottom
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
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

impl PdfElement {
    pub fn is_hidden(&self) -> bool {
        // Hidden text: zero font size, or very small width/height
        if self.font_size == Some(0.0) || self.font_size == Some(0.1) {
            return true;
        }
        if self.bounding_box.width() < 0.5 && self.bounding_box.height() < 0.5 {
            return true;
        }
        false
    }

    pub fn is_off_page(&self, page_width: f64, page_height: f64) -> bool {
        self.bounding_box.left > page_width + 10.0
            || self.bounding_box.bottom > page_height + 10.0
            || self.bounding_box.right < -10.0
            || self.bounding_box.top < -10.0
    }
}

/// A page is a list of elements in reading order.
pub type PageElements = Vec<PdfElement>;
pub type DocumentElements = Vec<PageElements>;

/// Raw word-level extraction result before any processing.
#[derive(Debug, Clone)]
pub struct RawPage {
    pub page_number: u32,
    pub width: f64,
    pub height: f64,
    pub words: Vec<RawWord>,
}

#[derive(Debug, Clone)]
pub struct RawWord {
    pub text: String,
    pub bbox: BoundingBox,
}
