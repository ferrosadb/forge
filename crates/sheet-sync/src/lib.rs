//! `forge-sheet-sync` — Google Sheets ↔ forge task board sync.
//!
//! See `specs/todo/feat-sheet-sync.md` for the full design and
//! `specs/plans/2026-07-14-sheet-sync.md` for the task-by-task build plan.
//! This crate is built incrementally; the current modules are pure,
//! network-free building blocks used by every later stage of the sync
//! engine (header mapping, board planning, push planning).

pub mod board_plan;
pub mod config;
pub mod mapping;
pub mod model;
pub mod normalize;
pub mod push_plan;
pub mod state;
