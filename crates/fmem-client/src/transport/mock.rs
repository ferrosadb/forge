//! In-memory scriptable transport for tests.
//!
//! Tests script expected calls and their responses up front; the mock
//! pops the next scripted response on each `call`. A mismatch between
//! the expected and actual call panics so the test fails with a clear
//! message.
//!
//! Scripts support three response flavors:
//!
//! - `ok(value)` — succeeds, MCP envelope is synthesized automatically
//! - `tool_error(code, message)` — synthesizes an `isError: true`
//!   envelope so the transport returns `Error::Tool`
//! - `raw_error(error)` — returns the given `Error` directly
//!
//! For FMEA F14 (id shuffle) and F18 (dry-run must never call), the
//! mock also supports "panic on any call" mode and expected-call-count
//! assertions.

use std::sync::Mutex;

use serde_json::{json, Value};

use crate::error::Error;
use crate::transport::Transport;

/// One scripted expectation.
#[derive(Debug)]
pub struct Expectation {
    /// JSON-RPC method the caller must invoke.
    pub method: String,
    /// Optional predicate over params — `None` means "any params match".
    pub params_matcher: Option<fn(&Value) -> bool>,
    /// The response to return (already wrapped as needed).
    pub response: ScriptedResponse,
}

/// What the mock should do in response to an expected call.
#[derive(Debug)]
pub enum ScriptedResponse {
    /// Return an `Ok(result)` value. For `tools/call` the result is
    /// auto-wrapped in an MCP envelope.
    Ok(Value),
    /// Simulate an fmem tool error — returns `Error::Tool` through the
    /// `tools/call` envelope.
    ToolError { code: i32, message: String },
    /// Simulate a transport / protocol / timeout failure.
    RawError(Error),
}

/// Scriptable transport.
///
/// Tests construct it via [`MockTransport::new`], push expectations via
/// [`expect_call`](Self::expect_call), then pass `&self` to code under
/// test. [`assert_done`](Self::assert_done) checks at teardown that
/// every expectation was consumed (F18).
pub struct MockTransport {
    inner: Mutex<Inner>,
}

struct Inner {
    script: Vec<Expectation>,
    cursor: usize,
    panic_on_call: bool,
    calls: Vec<(String, Value)>,
}

impl Default for MockTransport {
    fn default() -> Self {
        Self::new()
    }
}

impl MockTransport {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(Inner {
                script: Vec::new(),
                cursor: 0,
                panic_on_call: false,
                calls: Vec::new(),
            }),
        }
    }

    /// Construct a transport that panics on any call — for FMEA F18
    /// (dry-run must not reach the wire).
    pub fn panicking() -> Self {
        let m = Self::new();
        m.inner.lock().unwrap().panic_on_call = true;
        m
    }

    /// Add an expected call, matching `method` exactly; `response` is
    /// returned when it is consumed.
    pub fn expect_call(&self, method: &str, response: ScriptedResponse) {
        self.inner.lock().unwrap().script.push(Expectation {
            method: method.to_string(),
            params_matcher: None,
            response,
        });
    }

    /// Add an expected call with a predicate over the params.
    pub fn expect_call_with(
        &self,
        method: &str,
        params_matcher: fn(&Value) -> bool,
        response: ScriptedResponse,
    ) {
        self.inner.lock().unwrap().script.push(Expectation {
            method: method.to_string(),
            params_matcher: Some(params_matcher),
            response,
        });
    }

    /// Assert every expectation was consumed.
    pub fn assert_done(&self) {
        let inner = self.inner.lock().unwrap();
        assert_eq!(
            inner.cursor,
            inner.script.len(),
            "mock transport: {} expectation(s) left unused (first unused method: {:?})",
            inner.script.len() - inner.cursor,
            inner.script.get(inner.cursor).map(|e| &e.method),
        );
    }

    /// Return every call that landed (in order).
    pub fn calls(&self) -> Vec<(String, Value)> {
        self.inner.lock().unwrap().calls.clone()
    }
}

impl Transport for MockTransport {
    fn call(&self, method: &str, params: Value) -> Result<Value, Error> {
        let response = self.match_expectation(method, &params);
        wrap_response(method, response)
    }
}

impl MockTransport {
    /// Validate the next scripted expectation against `(method, params)`,
    /// record the call, advance the cursor, and return the response
    /// the test declared. Panics on any mismatch (unknown method,
    /// wrong method, failed params matcher, no more expectations,
    /// panicking-mode transport).
    fn match_expectation(&self, method: &str, params: &Value) -> ScriptedResponse {
        let mut inner = self.inner.lock().unwrap();
        if inner.panic_on_call {
            panic!("MockTransport::panicking called with method={method} params={params}");
        }
        inner.calls.push((method.to_string(), params.clone()));

        if inner.cursor >= inner.script.len() {
            panic!(
                "MockTransport: unexpected call {} (no more expectations); cursor={} script.len={}",
                method,
                inner.cursor,
                inner.script.len()
            );
        }

        let idx = inner.cursor;
        let response = {
            let expectation = &inner.script[idx];
            if expectation.method != method {
                panic!(
                    "MockTransport: expected method `{}` at position {} but got `{}`",
                    expectation.method, idx, method
                );
            }
            if let Some(matcher) = expectation.params_matcher {
                if !matcher(params) {
                    panic!(
                        "MockTransport: params matcher rejected params for method `{method}`: {params}"
                    );
                }
            }
            clone_response(&expectation.response)
        };
        inner.cursor += 1;
        response
    }
}

/// Synthesize the real transport's `Result<Value, Error>` from a
/// scripted response, wrapping the MCP `tools/call` envelope when the
/// method requires it.
fn wrap_response(method: &str, response: ScriptedResponse) -> Result<Value, Error> {
    match response {
        ScriptedResponse::Ok(value) if method == "tools/call" => Ok(json!({
            "content": [{ "type": "text", "text": value.to_string() }],
            "isError": false,
        })),
        ScriptedResponse::Ok(value) => Ok(value),
        ScriptedResponse::ToolError { code, message } if method == "tools/call" => {
            let body = json!({ "code": code, "message": message }).to_string();
            Ok(json!({
                "content": [{ "type": "text", "text": body }],
                "isError": true,
            }))
        }
        ScriptedResponse::ToolError { code, message } => Err(Error::Tool { code, message }),
        ScriptedResponse::RawError(e) => Err(e),
    }
}

fn clone_response(r: &ScriptedResponse) -> ScriptedResponse {
    match r {
        ScriptedResponse::Ok(v) => ScriptedResponse::Ok(v.clone()),
        ScriptedResponse::ToolError { code, message } => ScriptedResponse::ToolError {
            code: *code,
            message: message.clone(),
        },
        ScriptedResponse::RawError(e) => ScriptedResponse::RawError(clone_error(e)),
    }
}

fn clone_error(e: &Error) -> Error {
    use std::io;
    match e {
        Error::Transport(err) => Error::Transport(io::Error::new(err.kind(), err.to_string())),
        Error::Protocol(m) => Error::Protocol(m.clone()),
        Error::Tool { code, message } => Error::Tool {
            code: *code,
            message: message.clone(),
        },
        Error::Schema(m) => Error::Schema(m.clone()),
        Error::Timeout => Error::Timeout,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ok_response_round_trips_for_tools_call() {
        let m = MockTransport::new();
        m.expect_call(
            "tools/call",
            ScriptedResponse::Ok(json!({"ok": true, "n": 42})),
        );
        let got = m.call_tool("any", json!({})).unwrap();
        assert_eq!(got["ok"], true);
        assert_eq!(got["n"], 42);
        m.assert_done();
    }

    #[test]
    fn ok_response_passthrough_for_non_tools_call() {
        let m = MockTransport::new();
        m.expect_call(
            "initialize",
            ScriptedResponse::Ok(json!({"hello": "world"})),
        );
        let got = m.call("initialize", json!({})).unwrap();
        assert_eq!(got["hello"], "world");
    }

    #[test]
    fn tool_error_surfaces_as_error_tool() {
        let m = MockTransport::new();
        m.expect_call(
            "tools/call",
            ScriptedResponse::ToolError {
                code: -32602,
                message: "bad args".into(),
            },
        );
        let err = m.call_tool("x", json!({})).unwrap_err();
        match err {
            Error::Tool { code, message } => {
                assert_eq!(code, -32602);
                assert_eq!(message, "bad args");
            }
            other => panic!("expected Tool, got {other:?}"),
        }
    }

    #[test]
    fn raw_error_returned_directly() {
        let m = MockTransport::new();
        m.expect_call("tools/call", ScriptedResponse::RawError(Error::Timeout));
        let err = m.call_tool("x", json!({})).unwrap_err();
        assert!(matches!(err, Error::Timeout));
    }

    #[test]
    fn params_matcher_validates_body() {
        let m = MockTransport::new();
        m.expect_call_with(
            "tools/call",
            |p| p["name"] == "ingest_skill",
            ScriptedResponse::Ok(json!({"ok": true})),
        );
        m.call_tool("ingest_skill", json!({"x": 1})).unwrap();
    }

    #[test]
    #[should_panic(expected = "params matcher rejected")]
    fn params_matcher_panics_on_mismatch() {
        let m = MockTransport::new();
        m.expect_call_with(
            "tools/call",
            |p| p["name"] == "ingest_skill",
            ScriptedResponse::Ok(json!({})),
        );
        let _ = m.call_tool("different_tool", json!({}));
    }

    #[test]
    #[should_panic(expected = "MockTransport::panicking")]
    fn panicking_transport_panics_on_any_call() {
        let m = MockTransport::panicking();
        let _ = m.call("tools/call", json!({}));
    }

    #[test]
    #[should_panic(expected = "unused")]
    fn assert_done_flags_unused_expectations() {
        let m = MockTransport::new();
        m.expect_call("tools/call", ScriptedResponse::Ok(json!({})));
        m.assert_done();
    }

    #[test]
    fn records_every_call_in_order() {
        let m = MockTransport::new();
        m.expect_call("a", ScriptedResponse::Ok(json!({})));
        m.expect_call("b", ScriptedResponse::Ok(json!({})));
        m.call("a", json!({"i": 1})).unwrap();
        m.call("b", json!({"i": 2})).unwrap();
        let calls = m.calls();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].0, "a");
        assert_eq!(calls[1].0, "b");
    }
}
