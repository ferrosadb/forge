//! PDF structural extraction pipeline for academic paper ingestion.
//!
//! Provides typed element taxonomy, XY-Cut++ reading-order reconstruction,
//! and content filtering for trustworthy PDF-to-KG extraction.

pub mod element;
pub mod filter;
pub mod processors;
pub mod reading_order;

#[cfg(test)]
mod tests;
