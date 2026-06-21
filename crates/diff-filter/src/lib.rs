//! Smart git diff filtering.
//!
//! Skips lock files, generated code, and whitespace-only changes.
//! Collapses large hunks into summaries.

pub mod filter;
