//! MCP `initialize` handshake.
//!
//! P3 — before any `tools/call`, the client must call `initialize` and
//! announce its protocol version. The server replies with its own
//! protocolVersion + server info. We assert the protocol version
//! matches a known-supported set; anything else fails loud with an
//! upgrade hint (FMEA F15).

use serde::Deserialize;
use serde_json::json;

use crate::error::Error;
use crate::transport::Transport;

/// Protocol version forge expects. Mirrors what fmem advertises today
/// (`ferrosa-memory-mcp:dispatch.rs:962`). Update in lockstep with fmem.
pub const MCP_PROTOCOL_VERSION: &str = "2024-11-05";

/// Acceptable protocol versions — for now a single value, but kept as
/// a slice so we can widen compatibility without rewriting.
pub const SUPPORTED_PROTOCOL_VERSIONS: &[&str] = &[MCP_PROTOCOL_VERSION];

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

#[derive(Debug, Deserialize, Clone, Default)]
pub struct ServerInfo {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub version: String,
}

/// Perform the MCP initialize handshake.
///
/// Forge identifies itself as `forge-fmem-client` with its crate
/// version. On a version mismatch under [`ExpectedProtocolVersion::Strict`]
/// the call fails with [`Error::Protocol`] carrying both the advertised
/// and expected versions so the operator can see what to upgrade.
pub fn initialize<T: Transport>(
    transport: &T,
    mode: ExpectedProtocolVersion,
) -> Result<InitializeInfo, Error> {
    let params = json!({
        "protocolVersion": MCP_PROTOCOL_VERSION,
        "capabilities": { "tools": {} },
        "clientInfo": {
            "name": "forge-fmem-client",
            "version": env!("CARGO_PKG_VERSION"),
        },
    });
    let raw = transport.call("initialize", params)?;
    let info: InitializeInfo = serde_json::from_value(raw)
        .map_err(|e| Error::Protocol(format!("initialize response parse error: {e}")))?;

    if mode == ExpectedProtocolVersion::Strict
        && !SUPPORTED_PROTOCOL_VERSIONS.contains(&info.protocol_version.as_str())
    {
        return Err(Error::Protocol(format!(
            "MCP protocol mismatch: server advertises `{}`, client supports {:?}. Update forge-fmem-client or fmem.",
            info.protocol_version, SUPPORTED_PROTOCOL_VERSIONS
        )));
    }

    Ok(info)
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
            "initialize",
            ScriptedResponse::Ok(json!({
                "protocolVersion": MCP_PROTOCOL_VERSION,
                "serverInfo": { "name": "ferrosa-memory-mcp", "version": "0.1.0" },
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
            "initialize",
            ScriptedResponse::Ok(json!({
                "protocolVersion": "9999-99-99",
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
            "initialize",
            ScriptedResponse::Ok(json!({ "protocolVersion": "9999-99-99" })),
        );
        let info = initialize(&m, ExpectedProtocolVersion::Permissive).unwrap();
        assert_eq!(info.protocol_version, "9999-99-99");
    }

    #[test]
    fn malformed_response_is_protocol_error() {
        let m = MockTransport::new();
        m.expect_call(
            "initialize",
            ScriptedResponse::Ok(json!({ "wrong": "shape" })),
        );
        let err = initialize(&m, ExpectedProtocolVersion::Strict).unwrap_err();
        assert!(matches!(err, Error::Protocol(_)));
    }

    #[test]
    fn sends_expected_init_params() {
        let m = MockTransport::new();
        m.expect_call_with(
            "initialize",
            |p| {
                p["protocolVersion"] == MCP_PROTOCOL_VERSION
                    && p["clientInfo"]["name"] == "forge-fmem-client"
            },
            ScriptedResponse::Ok(json!({ "protocolVersion": MCP_PROTOCOL_VERSION })),
        );
        initialize(&m, ExpectedProtocolVersion::Strict).unwrap();
        m.assert_done();
    }
}
