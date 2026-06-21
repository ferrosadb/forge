//! Deduplicate and group lint output by rule and severity.
//!
//! Supports: clippy, ruff, eslint, and generic `file:line: rule message` formats.

pub mod dedup;
