//! Black-box probe for the supported MCP draft HTTP subset.
//!
//! The probe intentionally validates a server-advertised profile rather than
//! claiming certification for the entire evolving MCP draft specification.

use std::time::Duration;

use base64::Engine as _;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

pub const MODERN_PROTOCOL_VERSION: &str = "2026-07-28";
const PROFILE_META_KEY: &str = "io.ferrosa-memory/supportedDraftProfile";
const REQUIRED_METHODS: &[&str] = &[
    "server/discover",
    "tools/list",
    "tools/call",
    "prompts/list",
    "prompts/get",
    "resources/list",
    "resources/read",
    "subscriptions/listen",
];

#[derive(Debug, Clone)]
pub struct ProbeConfig {
    pub endpoint: String,
    pub basic_auth: Option<BasicAuth>,
    pub timeout: Duration,
}

impl ProbeConfig {
    pub fn new(endpoint: impl Into<String>) -> Self {
        Self {
            endpoint: endpoint.into(),
            basic_auth: None,
            timeout: Duration::from_secs(15),
        }
    }
}

#[derive(Debug, Clone)]
pub struct BasicAuth {
    pub user: String,
    pub pass: String,
}

impl BasicAuth {
    fn header_value(&self) -> String {
        let encoded = base64::engine::general_purpose::STANDARD
            .encode(format!("{}:{}", self.user, self.pass));
        format!("Basic {encoded}")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProbeStatus {
    Pass,
    Fail,
    Unsupported,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProbeCheck {
    pub name: String,
    pub status: ProbeStatus,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProbeReport {
    pub endpoint: String,
    pub protocol_version: String,
    pub overall: ProbeStatus,
    pub supported_methods: Vec<String>,
    pub checks: Vec<ProbeCheck>,
    pub violations: Vec<String>,
}

impl ProbeReport {
    fn new(endpoint: &str) -> Self {
        Self {
            endpoint: endpoint.to_string(),
            protocol_version: MODERN_PROTOCOL_VERSION.to_string(),
            overall: ProbeStatus::Fail,
            supported_methods: Vec::new(),
            checks: Vec::new(),
            violations: Vec::new(),
        }
    }

    fn check(&mut self, name: impl Into<String>, status: ProbeStatus, detail: impl Into<String>) {
        self.checks.push(ProbeCheck {
            name: name.into(),
            status,
            detail: detail.into(),
        });
    }

    fn fail(&mut self, violation: impl Into<String>) {
        let violation = violation.into();
        self.violations.push(violation.clone());
        self.check("server/discover", ProbeStatus::Fail, violation);
    }

    fn finish(&mut self) {
        self.overall = if self
            .checks
            .iter()
            .any(|check| check.status == ProbeStatus::Fail)
        {
            ProbeStatus::Fail
        } else if self
            .checks
            .iter()
            .any(|check| check.status == ProbeStatus::Unsupported)
        {
            ProbeStatus::Unsupported
        } else {
            ProbeStatus::Pass
        };
    }
}

pub fn probe(config: &ProbeConfig) -> ProbeReport {
    let mut report = ProbeReport::new(&config.endpoint);
    let response = match discover(config) {
        Ok(response) => response,
        Err(error) => {
            report.fail(error);
            report.finish();
            return report;
        }
    };

    report.check(
        "server/discover",
        ProbeStatus::Pass,
        "accepted draft headers and request metadata",
    );

    let Some(result) = response.get("result") else {
        report.fail("JSON-RPC response is missing result");
        report.finish();
        return report;
    };

    validate_discovery(result, &mut report);
    report.finish();
    report
}

fn discover(config: &ProbeConfig) -> Result<Value, String> {
    let mut request = ureq::post(&config.endpoint)
        .header("Content-Type", "application/json")
        .header("Accept", "application/json")
        .header("MCP-Protocol-Version", MODERN_PROTOCOL_VERSION)
        .header("Mcp-Method", "server/discover");
    if let Some(auth) = &config.basic_auth {
        request = request.header("Authorization", auth.header_value());
    }

    let body = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "server/discover",
        "params": {
            "_meta": {
                "io.modelcontextprotocol/protocolVersion": MODERN_PROTOCOL_VERSION,
                "io.modelcontextprotocol/clientCapabilities": {},
                "io.modelcontextprotocol/clientInfo": {
                    "name": "forge-mcp-probe",
                    "version": env!("CARGO_PKG_VERSION")
                }
            }
        }
    });

    let mut response = request
        .config()
        .timeout_global(Some(config.timeout))
        .build()
        .send_json(body)
        .map_err(|error| format!("server/discover request failed: {error}"))?;
    let status = response.status();
    let body = response
        .body_mut()
        .read_to_string()
        .map_err(|error| format!("failed to read server/discover response: {error}"))?;
    if !status.is_success() {
        return Err(format!(
            "server/discover returned HTTP {}: {}",
            status.as_u16(),
            body.chars().take(500).collect::<String>()
        ));
    }
    serde_json::from_str(&body)
        .map_err(|error| format!("server/discover returned invalid JSON: {error}"))
}

fn validate_discovery(result: &Value, report: &mut ProbeReport) {
    if result.get("resultType").and_then(Value::as_str) == Some("complete") {
        report.check("result_type", ProbeStatus::Pass, "resultType is complete");
    } else {
        report.check(
            "result_type",
            ProbeStatus::Fail,
            "resultType must be complete for a supported draft profile",
        );
    }

    let versions = result
        .get("supportedVersions")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if versions
        .iter()
        .any(|version| version.as_str() == Some(MODERN_PROTOCOL_VERSION))
    {
        report.check(
            "protocol_version",
            ProbeStatus::Pass,
            format!("advertises {MODERN_PROTOCOL_VERSION}"),
        );
    } else {
        report.check(
            "protocol_version",
            ProbeStatus::Unsupported,
            format!("does not advertise {MODERN_PROTOCOL_VERSION}"),
        );
    }

    let Some(server_info) = result
        .get("_meta")
        .and_then(|meta| meta.get("io.modelcontextprotocol/serverInfo"))
    else {
        report.check(
            "server_identity",
            ProbeStatus::Fail,
            "missing _meta.io.modelcontextprotocol/serverInfo",
        );
        return;
    };
    if server_info.get("name").and_then(Value::as_str).is_some()
        && server_info.get("version").and_then(Value::as_str).is_some()
    {
        report.check(
            "server_identity",
            ProbeStatus::Pass,
            "server name and version present",
        );
    } else {
        report.check(
            "server_identity",
            ProbeStatus::Fail,
            "serverInfo must include name and version",
        );
    }

    let profile = result
        .get("_meta")
        .and_then(|meta| meta.get(PROFILE_META_KEY));
    let Some(profile) = profile else {
        report.check(
            "supported_profile",
            ProbeStatus::Unsupported,
            format!("missing _meta.{PROFILE_META_KEY}"),
        );
        return;
    };

    let methods = profile
        .get("methods")
        .and_then(Value::as_array)
        .map(|methods| {
            methods
                .iter()
                .filter_map(Value::as_str)
                .map(ToOwned::to_owned)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    report.supported_methods = methods.clone();

    let missing: Vec<_> = REQUIRED_METHODS
        .iter()
        .filter(|required| !methods.iter().any(|method| method == **required))
        .copied()
        .collect();
    if missing.is_empty() {
        report.check(
            "supported_methods",
            ProbeStatus::Pass,
            "advertises the documented supported draft methods",
        );
    } else {
        report.check(
            "supported_methods",
            ProbeStatus::Fail,
            format!("missing documented methods: {}", missing.join(", ")),
        );
    }

    let subscription = profile.get("resourceSubscriptions");
    let Some(subscription) = subscription else {
        report.check(
            "resource_subscriptions",
            ProbeStatus::Fail,
            "missing resourceSubscriptions profile",
        );
        return;
    };
    let valid = subscription.get("transport").and_then(Value::as_str) == Some("sse")
        && subscription.get("acknowledgement").and_then(Value::as_str)
            == Some("notifications/subscriptions/acknowledged")
        && subscription
            .get("updateNotification")
            .and_then(Value::as_str)
            == Some("notifications/resources/updated");
    report.check(
        "resource_subscriptions",
        if valid {
            ProbeStatus::Pass
        } else {
            ProbeStatus::Fail
        },
        if valid {
            "advertises the expected SSE acknowledgement and update notifications".to_string()
        } else {
            "subscription semantics do not match the documented supported subset".to_string()
        },
    );
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;

    use super::*;

    fn discovery_response(result: Value) -> String {
        json!({"jsonrpc": "2.0", "id": 1, "result": result}).to_string()
    }

    fn compliant_result() -> Value {
        json!({
            "resultType": "complete",
            "supportedVersions": [MODERN_PROTOCOL_VERSION],
            "_meta": {
                "io.modelcontextprotocol/serverInfo": {"name": "fixture", "version": "1"},
                PROFILE_META_KEY: {
                    "methods": REQUIRED_METHODS,
                    "resourceSubscriptions": {
                        "transport": "sse",
                        "acknowledgement": "notifications/subscriptions/acknowledged",
                        "updateNotification": "notifications/resources/updated"
                    }
                }
            }
        })
    }

    fn spawn_server(response: String) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0; 4096];
            let _ = stream.read(&mut request).unwrap();
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                response.len(),
                response
            );
            stream.write_all(response.as_bytes()).unwrap();
        });
        endpoint
    }

    #[test]
    fn probe_passes_for_compliant_profile() {
        let endpoint = spawn_server(discovery_response(compliant_result()));
        let report = probe(&ProbeConfig::new(endpoint));
        assert_eq!(report.overall, ProbeStatus::Pass);
        assert!(report.violations.is_empty());
        assert_eq!(report.supported_methods.len(), REQUIRED_METHODS.len());
    }

    #[test]
    fn probe_reports_unsupported_without_profile() {
        let mut result = compliant_result();
        result["_meta"]
            .as_object_mut()
            .unwrap()
            .remove(PROFILE_META_KEY);
        let endpoint = spawn_server(discovery_response(result));
        let report = probe(&ProbeConfig::new(endpoint));
        assert_eq!(report.overall, ProbeStatus::Unsupported);
        assert!(report
            .checks
            .iter()
            .any(|check| check.name == "supported_profile"
                && check.status == ProbeStatus::Unsupported));
    }

    #[test]
    fn probe_reports_failure_for_incomplete_profile() {
        let mut result = compliant_result();
        result["_meta"][PROFILE_META_KEY]["methods"] = json!(["server/discover"]);
        let endpoint = spawn_server(discovery_response(result));
        let report = probe(&ProbeConfig::new(endpoint));
        assert_eq!(report.overall, ProbeStatus::Fail);
        assert!(report
            .checks
            .iter()
            .any(|check| check.name == "supported_methods" && check.status == ProbeStatus::Fail));
    }
}
