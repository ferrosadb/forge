//! Detect likely unbounded materialization in I/O paths.
//!
//! This is a heuristic audit tool for bugs where code reads a large disk/storage/query
//! source into an expanding in-memory collection (`Vec`, maps of `Vec`, `collect`,
//! `rows_or_empty`, whole-file reads) instead of streaming, paging, or chunking.

pub mod scanner;
