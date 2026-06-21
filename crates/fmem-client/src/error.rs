//! Typed errors for fmem MCP client calls.
//!
//! No `Other(String)` catch-all by design — every new failure class
//! gets its own variant so error handling stays exhaustive at every
//! call site (see `skills/rules/safety.md` fail-loud philosophy).

use std::fmt;
use std::io;

#[derive(Debug)]
pub enum Error {
    /// Transport-level failure: subprocess spawn, stdio read/write, HTTP connect.
    Transport(io::Error),
    /// Malformed or unexpected JSON-RPC framing (bad envelope, missing id, etc.).
    Protocol(String),
    /// fmem returned a JSON-RPC error response for a tool call.
    Tool { code: i32, message: String },
    /// fmem rejected the call payload as not matching its expected schema.
    Schema(String),
    /// Per-call deadline exceeded waiting for a response.
    Timeout,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Transport(e) => write!(f, "fmem transport error: {e}"),
            Self::Protocol(msg) => write!(f, "fmem protocol error: {msg}"),
            Self::Tool { code, message } => {
                write!(f, "fmem tool error (code {code}): {message}")
            }
            Self::Schema(msg) => write!(f, "fmem schema rejection: {msg}"),
            Self::Timeout => write!(f, "fmem call timed out"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Transport(e) => Some(e),
            _ => None,
        }
    }
}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Self::Transport(e)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_variant_renders() {
        let cases = [
            Error::Transport(io::Error::other("broken pipe")),
            Error::Protocol("missing id".into()),
            Error::Tool {
                code: -32601,
                message: "method not found".into(),
            },
            Error::Schema("field required: name".into()),
            Error::Timeout,
        ];
        for e in &cases {
            let s = e.to_string();
            assert!(!s.is_empty(), "empty Display for {e:?}");
            assert!(s.starts_with("fmem"), "Display should be prefixed: {s}");
        }
    }

    #[test]
    fn transport_source_chains() {
        let e = Error::Transport(io::Error::other("eof"));
        assert!(std::error::Error::source(&e).is_some());
    }

    #[test]
    fn non_transport_has_no_source() {
        let e = Error::Timeout;
        assert!(std::error::Error::source(&e).is_none());
    }

    #[test]
    fn io_error_converts() {
        let io_err = io::Error::new(io::ErrorKind::BrokenPipe, "x");
        let e: Error = io_err.into();
        assert!(matches!(e, Error::Transport(_)));
    }
}
