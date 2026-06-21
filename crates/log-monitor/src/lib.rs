//! Log monitor: analyze logs for stalls, errors, repeated failures, and actionable patterns.
//!
//! Designed for skill workflows — detects conditions that an LLM agent should act on:
//! - Stalled processes (no output for N seconds)
//! - Error cascades (same error repeated)
//! - Build/test completion markers
//! - Resource warnings (OOM, disk full, timeouts)
//! - Stuck loops (repeated identical lines)

pub mod monitor;
