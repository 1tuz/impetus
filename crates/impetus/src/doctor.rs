use anyhow::Result;
use impetus_client::HarnessClient;
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ProbeStatus {
    Ok,
    Warn,
    Error,
    Unavailable,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProbeResult {
    pub name: String,
    pub status: ProbeStatus,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remediation: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,
}

impl ProbeResult {
    fn ok(name: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            status: ProbeStatus::Ok,
            message: message.into(),
            remediation: None,
            details: None,
        }
    }

    fn warn(
        name: impl Into<String>,
        message: impl Into<String>,
        remediation: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into(),
            status: ProbeStatus::Warn,
            message: message.into(),
            remediation: Some(remediation.into()),
            details: None,
        }
    }

    fn error(
        name: impl Into<String>,
        message: impl Into<String>,
        remediation: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into(),
            status: ProbeStatus::Error,
            message: message.into(),
            remediation: Some(remediation.into()),
            details: None,
        }
    }

    fn unavailable(name: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            status: ProbeStatus::Unavailable,
            message: message.into(),
            remediation: None,
            details: None,
        }
    }

    fn with_details(mut self, details: serde_json::Value) -> Self {
        self.details = Some(details);
        self
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct DoctorReport {
    pub version: u16,
    pub probes: Vec<ProbeResult>,
}

impl DoctorReport {
    fn new() -> Self {
        Self {
            version: 1,
            probes: Vec::new(),
        }
    }

    fn add(&mut self, probe: ProbeResult) {
        self.probes.push(probe);
    }

    pub fn overall_status(&self) -> ProbeStatus {
        let has_error = self.probes.iter().any(|p| p.status == ProbeStatus::Error);
        let has_warn = self.probes.iter().any(|p| p.status == ProbeStatus::Warn);

        if has_error {
            ProbeStatus::Error
        } else if has_warn {
            ProbeStatus::Warn
        } else {
            ProbeStatus::Ok
        }
    }
}

pub async fn run_diagnostics(socket_path: &str, json: bool) -> Result<()> {
    let mut report = DoctorReport::new();

    // Probe: impetus/impetusd versions
    probe_versions(&mut report);

    // Probe: daemon discovery, socket path, permissions
    probe_socket(&mut report, socket_path);

    // Probe: daemon connection and protocol compatibility
    probe_daemon_connection(&mut report, socket_path).await;

    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print_human_report(&report);
    }

    Ok(())
}

fn probe_versions(report: &mut DoctorReport) {
    let impetus_version = env!("CARGO_PKG_VERSION");
    report.add(ProbeResult::ok(
        "impetus_version",
        format!("impetus {}", impetus_version),
    ));

    // impetusd version can only be determined after connection
    report.add(ProbeResult::unavailable(
        "impetusd_version",
        "Requires daemon connection",
    ));
}

fn probe_socket(report: &mut DoctorReport, socket_path: &str) {
    let path = Path::new(socket_path);

    if !path.exists() {
        report.add(
            ProbeResult::error(
                "socket_path",
                format!("Socket not found: {}", socket_path),
                "Start the daemon with: impetusd",
            )
            .with_details(serde_json::json!({ "path": socket_path })),
        );
        return;
    }

    match std::fs::metadata(path) {
        Ok(metadata) => {
            #[cfg(unix)]
            {
                use std::os::unix::fs::FileTypeExt;
                if !metadata.file_type().is_socket() {
                    report.add(
                        ProbeResult::error(
                            "socket_path",
                            format!("Path exists but is not a socket: {}", socket_path),
                            "Remove the file and restart impetusd",
                        )
                        .with_details(serde_json::json!({ "path": socket_path })),
                    );
                    return;
                }
            }

            report.add(
                ProbeResult::ok("socket_path", format!("Socket found: {}", socket_path))
                    .with_details(serde_json::json!({ "path": socket_path })),
            );
        }
        Err(e) => {
            report.add(
                ProbeResult::error(
                    "socket_path",
                    format!("Cannot access socket: {}", e),
                    "Check file permissions",
                )
                .with_details(serde_json::json!({ "path": socket_path, "error": e.to_string() })),
            );
        }
    }
}

async fn probe_daemon_connection(report: &mut DoctorReport, socket_path: &str) {
    match impetus_client::UnixSocketTransport::connect(socket_path).await {
        Ok(client) => {
            report.add(ProbeResult::ok(
                "daemon_connection",
                "Successfully connected to impetusd",
            ));

            // Probe: IPC handshake and protocol compatibility
            match client.hello().await {
                Ok(impetus_core::IpcResponse::Hello {
                    version,
                    capabilities,
                    ..
                }) => {
                    report.add(
                        ProbeResult::ok(
                            "ipc_protocol",
                            format!(
                                "Protocol version {}, capabilities: {:?}",
                                version, capabilities
                            ),
                        )
                        .with_details(serde_json::json!({
                            "version": version,
                            "capabilities": capabilities,
                        })),
                    );
                }
                Ok(impetus_core::IpcResponse::Incompatible {
                    supported_version,
                    client_version,
                    ..
                }) => {
                    report.add(
                        ProbeResult::error(
                            "ipc_protocol",
                            format!(
                                "Protocol incompatible: daemon {}, client {}",
                                supported_version, client_version
                            ),
                            "Update impetus and impetusd to matching versions",
                        )
                        .with_details(serde_json::json!({
                            "supported_version": supported_version,
                            "client_version": client_version,
                        })),
                    );
                }
                Ok(other) => {
                    report.add(ProbeResult::error(
                        "ipc_protocol",
                        format!("Unexpected hello response: {:?}", other),
                        "Restart impetusd",
                    ));
                }
                Err(e) => {
                    report.add(ProbeResult::error(
                        "ipc_protocol",
                        format!("Hello handshake failed: {}", e),
                        "Restart impetusd",
                    ));
                }
            }

            // Probe: daemon readiness (list sessions as health check)
            match client.list_sessions().await {
                Ok(sessions) => {
                    report.add(
                        ProbeResult::ok(
                            "daemon_readiness",
                            format!("Daemon ready, {} session(s)", sessions.len()),
                        )
                        .with_details(serde_json::json!({ "session_count": sessions.len() })),
                    );
                }
                Err(e) => {
                    report.add(ProbeResult::warn(
                        "daemon_readiness",
                        format!("Cannot list sessions: {}", e),
                        "Daemon may be starting or unhealthy",
                    ));
                    return;
                }
            }

            // Probe: subsystem health via Diagnostics endpoint
            match client.request(impetus_core::IpcRequest::Diagnostics).await {
                Ok(impetus_core::IpcResponse::Diagnostics { subsystems }) => {
                    add_subsystem_probes(report, *subsystems);
                }
                Ok(other) => {
                    report.add(ProbeResult::warn(
                        "subsystems",
                        format!("Unexpected diagnostics response: {:?}", other),
                        "Daemon may not support diagnostics",
                    ));
                }
                Err(e) => {
                    report.add(ProbeResult::warn(
                        "subsystems",
                        format!("Cannot query subsystems: {}", e),
                        "Daemon may not support diagnostics",
                    ));
                }
            }
        }
        Err(e) => {
            report.add(
                ProbeResult::error(
                    "daemon_connection",
                    format!("Cannot connect to impetusd: {}", e),
                    "Start the daemon with: impetusd",
                )
                .with_details(serde_json::json!({ "error": e.to_string() })),
            );
        }
    }
}

fn add_subsystem_probes(report: &mut DoctorReport, subsystems: impetus_core::SubsystemHealth) {
    let status_to_probe = |name: &str, sub: impetus_core::SubsystemStatus| {
        if sub.available {
            ProbeResult::ok(name, sub.message).with_details(sub.details.unwrap_or_default())
        } else {
            ProbeResult::warn(name, sub.message, "Check daemon configuration")
                .with_details(sub.details.unwrap_or_default())
        }
    };

    report.add(status_to_probe("event_store", subsystems.event_store));
    report.add(status_to_probe("artifact_store", subsystems.artifact_store));
    report.add(status_to_probe("policy_engine", subsystems.policy_engine));
    report.add(status_to_probe(
        "provider_registry",
        subsystems.provider_registry,
    ));
    report.add(status_to_probe("sandbox", subsystems.sandbox));
    report.add(status_to_probe(
        "credential_store",
        subsystems.credential_store,
    ));
    report.add(status_to_probe(
        "tools_capabilities",
        subsystems.tools_capabilities,
    ));
    report.add(status_to_probe(
        "external_agents",
        subsystems.external_agents,
    ));
    report.add(status_to_probe(
        "optional_modules",
        subsystems.optional_modules,
    ));
    report.add(status_to_probe("disk_runtime", subsystems.disk_runtime));
}

fn print_human_report(report: &DoctorReport) {
    println!("Impetus Diagnostics Report");
    println!("==========================\n");

    for probe in &report.probes {
        let icon = match probe.status {
            ProbeStatus::Ok => "✓",
            ProbeStatus::Warn => "⚠",
            ProbeStatus::Error => "✗",
            ProbeStatus::Unavailable => "○",
        };

        println!("{} {}: {}", icon, probe.name, probe.message);

        if let Some(remediation) = &probe.remediation {
            println!("  → {}", remediation);
        }
        println!();
    }

    let overall = report.overall_status();
    let summary = match overall {
        ProbeStatus::Ok => "All checks passed",
        ProbeStatus::Warn => "Some warnings detected",
        ProbeStatus::Error => "Critical issues detected",
        ProbeStatus::Unavailable => "Incomplete diagnostics",
    };

    println!("Overall: {}", summary);
}
