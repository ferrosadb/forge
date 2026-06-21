//! Shared utilities for forge crates.
//!
//! Provides common output formatting, token tracking, and tee/fallback
//! used across all subcommands.

pub mod filters;
pub mod secrets;
pub mod tee;
pub mod tracking;

use serde::Serialize;

/// Format output as JSON (default) or pretty-printed JSON.
pub fn emit_json<T: Serialize>(value: &T, pretty: bool) -> anyhow::Result<String> {
    if pretty {
        Ok(serde_json::to_string_pretty(value)?)
    } else {
        Ok(serde_json::to_string(value)?)
    }
}

/// Read all of stdin into a String.
pub fn read_stdin() -> anyhow::Result<String> {
    use std::io::Read;
    let mut buf = String::new();
    std::io::stdin().read_to_string(&mut buf)?;
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emit_json_compact() {
        let val = serde_json::json!({"a": 1});
        let out = emit_json(&val, false).unwrap();
        assert_eq!(out, r#"{"a":1}"#);
    }

    #[test]
    fn emit_json_pretty() {
        let val = serde_json::json!({"a": 1});
        let out = emit_json(&val, true).unwrap();
        assert!(out.contains('\n'));
    }

    /// Regression (task t_2c779031): a value containing literal control
    /// characters (U+0000–U+001F) — e.g. a `frg task` body with embedded
    /// newlines/tabs — must emit JSON in which those control chars are escaped,
    /// so the output re-parses cleanly (`frg task list | jq` must not fail with
    /// "Invalid string: control characters must be escaped"). serde_json
    /// guarantees this; routing all `frg` output through `emit_json` (rather
    /// than any hand-rolled string concatenation) keeps the guarantee, and this
    /// test locks it to the API so a future regression is caught.
    #[test]
    fn emit_json_escapes_control_chars() {
        let body = "line one\nline two\twith a tab\rand a return\u{0001}and a SOH";
        let val = serde_json::json!({ "body": body });

        for pretty in [false, true] {
            let out = emit_json(&val, pretty).unwrap();
            // No raw C0 control character may appear inside the JSON output;
            // serde escapes them (\n, \t, \r, , …). Pretty mode legitimately
            // contains real newlines/spaces for layout, so only scan the quoted
            // string region for the SOH which has no layout role.
            assert!(
                !out.contains('\u{0001}'),
                "raw SOH leaked into JSON output (pretty={pretty}): {out:?}"
            );
            // The output must be valid JSON and round-trip the original bytes.
            let parsed: serde_json::Value =
                serde_json::from_str(&out).expect("emit_json output must be valid JSON");
            assert_eq!(parsed["body"], serde_json::Value::String(body.to_string()));
        }
    }
}
