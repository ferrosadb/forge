//! Dev-tool wrappers that run common build/test/lint commands and return
//! structured, token-minimized JSON output.
//!
//! Every tool follows the same contract:
//! - Accept a working directory and optional Docker container name.
//! - Run the underlying command, capturing stdout+stderr.
//! - Parse the output into typed fields (counts, error lists, etc.).
//! - Cap lists at a fixed maximum to bound token usage.
//! - Include truncated raw output **only** on failure.

pub mod runner;

pub mod cargo;
pub mod ci;
pub mod docker;
pub mod dotnet;
pub mod elixir;
pub mod git;
pub mod go;
pub mod npm;
pub mod python;
