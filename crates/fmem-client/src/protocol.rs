//! Module: Shared MCP protocol constants and request metadata for fmem clients.
//! Correctness: Correct when HTTP calls carry draft metadata/headers and legacy stdio fallback still negotiates.
//! Last revised: 2026-07-17
//! Last changed: Added draft 2026-07-28 per-request metadata helpers.

use serde_json::{json, Map, Value};

pub const LEGACY_PROTOCOL_VERSION: &str = "2024-11-05";
pub const MCP_PROTOCOL_VERSION: &str = "2026-07-28";
pub const SUPPORTED_PROTOCOL_VERSIONS: &[&str] = &[MCP_PROTOCOL_VERSION, LEGACY_PROTOCOL_VERSION];
const BASE64_SENTINEL_PREFIX: &str = "=?base64?";
const BASE64_SENTINEL_SUFFIX: &str = "?=";

pub fn client_info() -> Value {
    json!({
        "name": "forge-fmem-client",
        "version": env!("CARGO_PKG_VERSION"),
    })
}

pub fn client_meta() -> Value {
    json!({
        "io.modelcontextprotocol/protocolVersion": MCP_PROTOCOL_VERSION,
        "io.modelcontextprotocol/clientInfo": client_info(),
        "io.modelcontextprotocol/clientCapabilities": {}
    })
}

pub fn add_client_meta(params: Value) -> Value {
    let mut params = match params {
        Value::Object(map) => Value::Object(map),
        Value::Null => Value::Object(Map::new()),
        value => {
            let mut map = Map::new();
            map.insert("value".to_string(), value);
            Value::Object(map)
        }
    };

    let Some(params_obj) = params.as_object_mut() else {
        return params;
    };
    let meta = params_obj
        .entry("_meta".to_string())
        .or_insert_with(|| Value::Object(Map::new()));
    if !meta.is_object() {
        *meta = Value::Object(Map::new());
    }
    if let Some(meta_obj) = meta.as_object_mut() {
        let modern_meta = client_meta();
        if let Some(modern_map) = modern_meta.as_object() {
            for (key, value) in modern_map {
                meta_obj.entry(key.clone()).or_insert_with(|| value.clone());
            }
        }
    }

    params
}

pub fn params_name_for_mcp_header<'a>(method: &str, params: &'a Value) -> Option<&'a str> {
    match method {
        "tools/call" | "prompts/get" => params.get("name").and_then(Value::as_str),
        "resources/read" => params.get("uri").and_then(Value::as_str),
        _ => None,
    }
}

pub fn encode_mcp_header_value(value: &str) -> String {
    if is_plain_mcp_header_value(value) {
        return value.to_string();
    }
    use base64::Engine as _;
    let encoded = base64::engine::general_purpose::STANDARD.encode(value.as_bytes());
    format!("{BASE64_SENTINEL_PREFIX}{encoded}{BASE64_SENTINEL_SUFFIX}")
}

fn is_plain_mcp_header_value(value: &str) -> bool {
    if value.is_empty()
        || (value.starts_with(BASE64_SENTINEL_PREFIX) && value.ends_with(BASE64_SENTINEL_SUFFIX))
        || value.trim_matches([' ', '\t']) != value
    {
        return false;
    }
    value
        .as_bytes()
        .iter()
        .all(|byte| matches!(*byte, 0x20..=0x7e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_client_meta_preserves_existing_params() {
        let params = add_client_meta(json!({"name": "ingest_entities", "arguments": {}}));
        assert_eq!(params["name"], "ingest_entities");
        assert_eq!(
            params["_meta"]["io.modelcontextprotocol/protocolVersion"],
            MCP_PROTOCOL_VERSION
        );
        assert_eq!(
            params["_meta"]["io.modelcontextprotocol/clientInfo"]["name"],
            "forge-fmem-client"
        );
    }

    #[test]
    fn encode_mcp_header_value_uses_plain_ascii_when_safe() {
        assert_eq!(encode_mcp_header_value("search"), "search");
        assert_eq!(encode_mcp_header_value("hello world"), "hello world");
    }

    #[test]
    fn encode_mcp_header_value_uses_base64_sentinel_when_needed() {
        assert_eq!(
            encode_mcp_header_value("Hello, 世界"),
            "=?base64?SGVsbG8sIOS4lueVjA==?="
        );
        assert_eq!(
            encode_mcp_header_value(" padded "),
            "=?base64?IHBhZGRlZCA=?="
        );
        assert_eq!(
            encode_mcp_header_value("=?base64?literal?="),
            "=?base64?PT9iYXNlNjQ/bGl0ZXJhbD89?="
        );
    }
}
