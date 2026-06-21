//! Coverage gate: validate coverage + cyclomatic complexity coupling.
//!
//! Parses lcov coverage reports and computes CC from source files.
//! Enforces the skill-defined gates:
//! - Baseline: 80% line coverage for new/changed code
//! - CC >= 15: 90% coverage + local documentation required
//! - CC >= 25: refactor plan required

pub mod gate;
