//! Distill build logs, compiler output, and stack traces into actionable lines.
//!
//! Strips ANSI codes, timestamps, progress bars, and noise.
//! Keeps errors, warnings, and surrounding context.

pub mod distiller;
