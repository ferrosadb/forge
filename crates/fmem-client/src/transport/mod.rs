//! JSON-RPC transports for fmem.
//!
//! The [`Transport`] trait abstracts over the stdio subprocess used in
//! production and the scriptable [`MockTransport`] used in tests. Both
//! implementations match responses to requests by id strictly — a
//! mismatched response is a protocol error, not silently dropped
//! (FMEA F14).

pub mod http;
pub mod mock;
pub mod stdio;

pub use http::{HttpAuth, HttpConfig, HttpTransport};
pub use mock::MockTransport;
pub use stdio::StdioTransport;

use serde_json::{json, Value};

use crate::error::Error;

/// JSON-RPC client over any transport.
///
/// Implementations expose a single blocking `call` that sends a request
/// and returns the `result` field of the response. Errors returned from
/// the server map to [`Error::Tool`]; transport/protocol/timeout
/// failures map to their own variants.
pub trait Transport {
    /// Send a raw JSON-RPC request and return the unwrapped `result`.
    fn call(&self, method: &str, params: Value) -> Result<Value, Error>;

    /// Send an MCP `tools/call` for `tool_name` with the given arguments.
    ///
    /// MCP wraps tool responses in `{content: [{type, text}], isError}`.
    /// For typed tool wrappers it's easier to parse the `result` field
    /// directly, so this helper unwraps the MCP envelope: when
    /// `isError` is true it maps to [`Error::Tool`]; otherwise it
    /// returns the first `content[0].text` parsed as JSON.
    fn call_tool(&self, tool_name: &str, args: Value) -> Result<Value, Error> {
        let params = json!({ "name": tool_name, "arguments": args });
        let raw = self.call("tools/call", params)?;
        unwrap_tool_result(raw)
    }
}

/// Unwrap the MCP `tools/call` result envelope into the concrete tool
/// response. Public so callers that want the raw envelope can opt out.
pub fn unwrap_tool_result(raw: Value) -> Result<Value, Error> {
    let is_error = raw
        .get("isError")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let content = raw
        .get("content")
        .and_then(|v| v.as_array())
        .ok_or_else(|| Error::Protocol("tools/call response missing `content` array".into()))?;
    let first = content
        .first()
        .ok_or_else(|| Error::Protocol("tools/call response has empty content".into()))?;
    let text = first.get("text").and_then(|v| v.as_str()).ok_or_else(|| {
        Error::Protocol("tools/call content[0].text missing or not a string".into())
    })?;

    let parsed: Value = serde_json::from_str(text)
        .map_err(|e| Error::Protocol(format!("tools/call content[0].text is not JSON: {e}")))?;

    if is_error {
        // fmem error text often includes `{"code": N, "message": "..."}`.
        let code = parsed
            .get("code")
            .and_then(|v| v.as_i64())
            .unwrap_or(-32000) as i32;
        let message = parsed
            .get("message")
            .and_then(|v| v.as_str())
            .unwrap_or(text)
            .to_string();
        return Err(Error::Tool { code, message });
    }
    Ok(parsed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn unwraps_success_content_as_json() {
        let raw = json!({
            "content": [{ "type": "text", "text": "{\"entity_id\":\"abc\"}" }],
            "isError": false
        });
        let got = unwrap_tool_result(raw).unwrap();
        assert_eq!(got["entity_id"], "abc");
    }

    #[test]
    fn maps_is_error_to_tool_error() {
        let raw = json!({
            "content": [{ "type": "text", "text": "{\"code\":-32602,\"message\":\"bad arg\"}" }],
            "isError": true
        });
        let err = unwrap_tool_result(raw).unwrap_err();
        match err {
            Error::Tool { code, message } => {
                assert_eq!(code, -32602);
                assert_eq!(message, "bad arg");
            }
            other => panic!("expected Tool, got {other:?}"),
        }
    }

    #[test]
    fn missing_content_is_protocol_error() {
        let err = unwrap_tool_result(json!({"isError": false})).unwrap_err();
        assert!(matches!(err, Error::Protocol(_)));
    }

    #[test]
    fn non_json_text_is_protocol_error() {
        let raw = json!({
            "content": [{ "type": "text", "text": "not-json" }],
            "isError": false
        });
        let err = unwrap_tool_result(raw).unwrap_err();
        assert!(matches!(err, Error::Protocol(_)));
    }
}
