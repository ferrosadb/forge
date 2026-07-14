//! `forge-sheet-sync` — Google Sheets ↔ forge task board sync.
//!
//! See `specs/todo/feat-sheet-sync.md` for the full design and
//! `specs/plans/2026-07-14-sheet-sync.md` for the task-by-task build plan.
//! This crate is built incrementally. `board_plan`/`config`/`mapping`/
//! `model`/`normalize`/`push_plan`/`state` are pure, network-free building
//! blocks; `sheets`/`board` are the I/O seam traits (`SheetsApi`/
//! `BoardSink`) those blocks are wired to, and `sync` is the end-to-end
//! `pull`/`push` orchestration — see `sync`'s doc for how the seam keeps
//! that orchestration testable without network or CQL.

pub mod board;
pub mod board_exec;
pub mod board_plan;
pub mod config;
pub mod mapping;
pub mod model;
pub mod normalize;
pub mod oauth;
pub mod push_plan;
pub mod sheets;
pub mod state;
pub mod sync;

pub use board::BoardSink;
pub use config::resolve_alias;
pub use sheets::SheetsApi;
pub use sync::{pull, push, PullOptions, PullReport, PushOptions, PushReport};
