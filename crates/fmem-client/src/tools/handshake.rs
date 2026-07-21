//! MCP protocol negotiation.
//!
//! Draft MCP uses stateless `server/discover`; legacy fmem servers still use
//! `initialize`. Forge probes discovery first and falls back to initialize only
//! when the server does not implement the modern method.

use serde::Deserialize;
use serde_json::{json, Value};

use crate::error::Error;
use crate::protocol::{
    client_meta, LEGACY_PROTOCOL_VERSION, MCP_PROTOCOL_VERSION, SUPPORTED_PROTOCOL_VERSIONS,
};
use crate::transport::Transport;

/// Whether the handshake allows a protocol-version mismatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExpectedProtocolVersion {
    /// Only versions in `SUPPORTED_PROTOCOL_VERSIONS` are accepted.
    Strict,
    /// Accept whatever the server advertises, but record it for logging.
    Permissive,
}

/// Server info returned from `initialize`. Only the fields forge looks
/// at are typed; extras are ignored.
#[derive(Debug, Deserialize, Clone)]
pub struct InitializeInfo {
    #[serde(rename = "protocolVersion")]
    pub protocol_version: String,
    #[serde(rename = "serverInfo", default)]
    pub server_info: Option<ServerInfo>,
}

#[derive(Debug, Deserialize)]
struct DiscoverInfo {
    #[serde(rename = "supportedVersions", default)]
    supported_versions: Vec<String>,
    #[serde(rename = "_meta", default)]
    meta: Value,
}

#[derive(Debug, Deserialize, Clone, Default)]
pub struct ServerInfo {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub version: String,
}

/// Perform the MCP initialize handshake.
///
/// Modern servers answer `server/discover`; legacy servers that return
/// `Method not found` are retried with `initialize`. On a version mismatch
/// under [`ExpectedProtocolVersion::Strict`] the call fails with
/// [`Error::Protocol`] carrying both advertised and expected versions.
pub fn initialize<T: Transport>(
    transport: &T,
    mode: ExpectedProtocolVersion,
) -> Result<InitializeInfo, Error> {
    match discover(transport, mode) {
        Ok(info) => {
            validate_version(&info.protocol_version, mode)?;
            return Ok(info);
        }
        Err(Error::Tool { code: -32601, .. }) => {}
        Err(error) => return Err(error),
    }

    legacy_initialize(transport, mode)
}

fn discover<T: Transport>(
    transport: &T,
    mode: ExpectedProtocolVersion,
) -> Result<InitializeInfo, Error> {
    let raw = transport.call(
        "server/discover",
        json!({
            "_meta": client_meta()
        }),
    )?;
    let info: DiscoverInfo = serde_json::from_value(raw)
        .map_err(|e| Error::Protocol(format!("server/discover response parse error: {e}")))?;
    let selected = info
        .supported_versions
        .iter()
        .find(|version| version.as_str() == MCP_PROTOCOL_VERSION)
        .or_else(|| {
            info.supported_versions
                .iter()
                .find(|version| version.as_str() == LEGACY_PROTOCOL_VERSION)
        })
        .or_else(|| {
            if mode == ExpectedProtocolVersion::Permissive {
                info.supported_versions.first()
            } else {
                None
            }
        })
        .cloned()
        .ok_or_else(|| {
            Error::Protocol(format!(
                "MCP protocol mismatch: server supports {:?}, client supports {:?}. Update forge-fmem-client or fmem.",
                info.supported_versions, SUPPORTED_PROTOCOL_VERSIONS
            ))
        })?;
    Ok(InitializeInfo {
        protocol_version: selected,
        server_info: parse_server_info_from_meta(&info.meta),
    })
}

fn legacy_initialize<T: Transport>(
    transport: &T,
    mode: ExpectedProtocolVersion,
) -> Result<InitializeInfo, Error> {
    let params = json!({
        "protocolVersion": LEGACY_PROTOCOL_VERSION,
        "capabilities": { "tools": {} },
        "clientInfo": {
            "name": "forge-fmem-client",
            "version": env!("CARGO_PKG_VERSION"),
        },
    });
    let raw = transport.call("initialize", params)?;
    let info: InitializeInfo = serde_json::from_value(raw)
        .map_err(|e| Error::Protocol(format!("initialize response parse error: {e}")))?;

    validate_version(&info.protocol_version, mode)?;
    Ok(info)
}

fn validate_version(version: &str, mode: ExpectedProtocolVersion) -> Result<(), Error> {
    if mode == ExpectedProtocolVersion::Strict && !SUPPORTED_PROTOCOL_VERSIONS.contains(&version) {
        return Err(Error::Protocol(format!(
            "MCP protocol mismatch: server advertises `{}`, client supports {:?}. Update forge-fmem-client or fmem.",
            version, SUPPORTED_PROTOCOL_VERSIONS
        )));
    }
    Ok(())
}

fn parse_server_info_from_meta(meta: &Value) -> Option<ServerInfo> {
    serde_json::from_value(
        meta.get("io.modelcontextprotocol/serverInfo")
            .cloned()
            .unwrap_or(Value::Null),
    )
    .ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::mock::{MockTransport, ScriptedResponse};
    use serde_json::json;

    #[test]
    fn strict_mode_accepts_supported_version() {
        let m = MockTransport::new();
        m.expect_call(
            "server/discover",
            ScriptedResponse::Ok(json!({
                "resultType": "complete",
                "supportedVersions": [MCP_PROTOCOL_VERSION],
                "_meta": {
                    "io.modelcontextprotocol/serverInfo": {
                        "name": "ferrosa-memory-mcp",
                        "version": "0.1.0"
                    }
                }
            })),
        );
        let info = initialize(&m, ExpectedProtocolVersion::Strict).unwrap();
        assert_eq!(info.protocol_version, MCP_PROTOCOL_VERSION);
        assert_eq!(info.server_info.unwrap().name, "ferrosa-memory-mcp");
    }

    #[test]
    fn strict_mode_rejects_unknown_version() {
        let m = MockTransport::new();
        m.expect_call(
            "server/discover",
            ScriptedResponse::Ok(json!({
                "resultType": "complete",
                "supportedVersions": ["9999-99-99"],
            })),
        );
        let err = initialize(&m, ExpectedProtocolVersion::Strict).unwrap_err();
        match err {
            Error::Protocol(msg) => {
                assert!(msg.contains("9999-99-99"));
                assert!(msg.contains("MCP protocol mismatch"));
            }
            other => panic!("expected Protocol, got {other:?}"),
        }
    }

    #[test]
    fn permissive_mode_accepts_any_version() {
        let m = MockTransport::new();
        m.expect_call(
            "server/discover",
            ScriptedResponse::Ok(json!({
                "resultType": "complete",
                "supportedVersions": ["9999-99-99"],
            })),
        );
        let info = initialize(&m, ExpectedProtocolVersion::Permissive).unwrap();
        assert_eq!(info.protocol_version, "9999-99-99");
    }

    #[test]
    fn malformed_response_is_protocol_error() {
        let m = MockTransport::new();
        m.expect_call(
            "server/discover",
            ScriptedResponse::Ok(json!({ "wrong": "shape" })),
        );
        let err = initialize(&m, ExpectedProtocolVersion::Strict).unwrap_err();
        assert!(matches!(err, Error::Protocol(_)));
    }

    #[test]
    fn sends_expected_discover_params() {
        let m = MockTransport::new();
        m.expect_call_with(
            "server/discover",
            |p| {
                p["_meta"]["io.modelcontextprotocol/protocolVersion"] == MCP_PROTOCOL_VERSION
                    && p["_meta"]["io.modelcontextprotocol/clientInfo"]["name"]
                        == "forge-fmem-client"
            },
            ScriptedResponse::Ok(json!({
                "supportedVersions": [MCP_PROTOCOL_VERSION],
            })),
        );
        initialize(&m, ExpectedProtocolVersion::Strict).unwrap();
        m.assert_done();
    }

    #[test]
    fn falls_back_to_legacy_initialize_when_discover_is_missing() {
        let m = MockTransport::new();
        m.expect_call(
            "server/discover",
            ScriptedResponse::RawError(Error::Tool {
                code: -32601,
                message: "Method not found".into(),
            }),
        );
        m.expect_call_with(
            "initialize",
            |p| {
                p["protocolVersion"] == LEGACY_PROTOCOL_VERSION
                    && p["clientInfo"]["name"] == "forge-fmem-client"
            },
            ScriptedResponse::Ok(json!({
                "protocolVersion": LEGACY_PROTOCOL_VERSION,
                "serverInfo": { "name": "legacy-fmem", "version": "0.1.0" },
            })),
        );
        let info = initialize(&m, ExpectedProtocolVersion::Strict).unwrap();
        assert_eq!(info.protocol_version, LEGACY_PROTOCOL_VERSION);
        assert_eq!(info.server_info.unwrap().name, "legacy-fmem");
        m.assert_done();
    }
}
