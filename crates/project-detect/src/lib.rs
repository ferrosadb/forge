//! Project type detector: auto-detect languages, frameworks, test runners,
//! and linters from repository file structure.
//!
//! Used by `/warp` to suggest which skills to symlink.

pub mod detector;
pub mod summary;
