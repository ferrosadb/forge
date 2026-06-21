//! Code digest: extract structural skeleton (signatures, types, imports).
//!
//! Produces a compact outline of source files — function signatures,
//! struct/class definitions, imports — without function bodies.
//! Uses regex-based extraction (no tree-sitter dependency).

pub mod excerpt;
pub mod lookup;
pub mod summarizer;
