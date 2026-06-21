//! Docker Compose status: parse `docker compose ps` into structured output.

use crate::runner::*;
use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct DockerStatusResult {
    #[serde(flatten)]
    pub base: ToolOutput,
    pub services: Vec<ServiceInfo>,
    pub service_count: usize,
    pub all_running: bool,
}

#[derive(Debug, Serialize)]
pub struct ServiceInfo {
    pub name: String,
    pub service: String,
    pub state: String,
    pub status: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub ports: String,
}

pub fn status(dir: &str) -> DockerStatusResult {
    let cmd_str = "docker compose ps --format json".to_string();
    let r = run_cmd("docker", &["compose", "ps", "--format", "json"], dir, None);

    let mut services = Vec::new();
    for line in r.output.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Ok(val) = serde_json::from_str::<serde_json::Value>(trimmed) {
            services.push(ServiceInfo {
                name: val
                    .get("Name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown")
                    .to_string(),
                service: val
                    .get("Service")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown")
                    .to_string(),
                state: val
                    .get("State")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown")
                    .to_string(),
                status: val
                    .get("Status")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                ports: val
                    .get("Ports")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
            });
        }
    }

    let all_running = !services.is_empty() && services.iter().all(|s| s.state == "running");
    let sc = services.len();

    let stopped: Vec<&str> = services
        .iter()
        .filter(|s| s.state != "running")
        .map(|s| s.service.as_str())
        .collect();

    let hint = if r.exit_code == -1 {
        Some(binary_missing_hint("docker"))
    } else if sc == 0 && r.exit_code != 0 {
        Some("Docker may not be running, or no docker-compose.yml found. Start Docker or check the path.".to_string())
    } else if sc == 0 {
        Some("No services found. Run `docker compose up -d` to start services.".to_string())
    } else if !stopped.is_empty() {
        Some(format!(
            "Stopped services: {}. Run `docker compose up -d {}` to restart them.",
            stopped.join(", "),
            stopped.join(" ")
        ))
    } else {
        None
    };

    DockerStatusResult {
        base: ToolOutput::new(cmd_str, r.exit_code, all_running, hint, &r.output),
        service_count: sc,
        all_running,
        services,
    }
}
