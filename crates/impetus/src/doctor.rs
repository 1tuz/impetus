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

pub async fn run_diagnostics(socket_path: &str, json: bool, probe_network: bool) -> Result<()> {
    let mut report = DoctorReport::new();

    // Probe: impetus/impetusd versions
    probe_versions(&mut report);

    // Probe: daemon discovery, socket path, permissions
    probe_socket(&mut report, socket_path);

    // Probe: daemon connection and protocol compatibility
    probe_daemon_connection(&mut report, socket_path, probe_network).await;

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

async fn probe_daemon_connection(
    report: &mut DoctorReport,
    socket_path: &str,
    probe_network: bool,
) {
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

                    // Live network probe if requested
                    if probe_network {
                        probe_web_research_live(report).await;
                    }
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
    report.add(status_to_probe("web_research", subsystems.web_research));
}

async fn probe_web_research_live(report: &mut DoctorReport) {
    use impetus_core::web_research::{EgressPolicy, WebDoctor, WebResearchEngine};

    let engine = WebResearchEngine::production(EgressPolicy::default());
    let web_report = WebDoctor::probe_engine(&engine).await;

    // Add per-backend probes
    for backend in &web_report.search_backends {
        let (status, message, remediation) = match &backend.status {
            impetus_core::web_research::doctor::BackendDoctorStatus::BuiltIn => (
                ProbeStatus::Ok,
                "Built-in backend available".to_string(),
                None,
            ),
            impetus_core::web_research::doctor::BackendDoctorStatus::Configured => (
                ProbeStatus::Ok,
                "External backend configured".to_string(),
                None,
            ),
            impetus_core::web_research::doctor::BackendDoctorStatus::Reachable => (
                ProbeStatus::Ok,
                "Search backend reachable".to_string(),
                None,
            ),
            impetus_core::web_research::doctor::BackendDoctorStatus::Unavailable { reason } => (
                ProbeStatus::Unavailable,
                format!("Backend unavailable: {}", reason),
                Some("Check network policy or firewall settings".to_string()),
            ),
            impetus_core::web_research::doctor::BackendDoctorStatus::Misconfigured { reason } => (
                ProbeStatus::Error,
                format!("Backend misconfigured: {}", reason),
                Some("Check backend configuration".to_string()),
            ),
            impetus_core::web_research::doctor::BackendDoctorStatus::Failed { reason } => (
                ProbeStatus::Error,
                format!("Backend probe failed: {}", reason),
                Some("Check network connectivity and backend availability".to_string()),
            ),
        };

        let probe = ProbeResult {
            name: format!("web_backend_{}", backend.id),
            status,
            message,
            remediation,
            details: None,
        };

        report.add(probe);
    }

    // Add summary notes
    if !web_report.notes.is_empty() {
        for note in &web_report.notes {
            if note.contains("DEGRADED") {
                report.add(ProbeResult::warn(
                    "web_research_status",
                    note.clone(),
                    "Primary backend unavailable, fallback active",
                ));
            }
        }
    }
}

fn print_compatibility_matrix() {
    use impetus_core::CompatibilityMatrix;

    let matrices = CompatibilityMatrix::all();

    for matrix in matrices {
        let source_name = format!("{:?}", matrix.source);
        println!("\n{}", source_name);

        // Count capability levels
        let mut supported = 0;
        let mut partial = 0;
        let mut _unsupported = 0;
        let mut _incompatible = 0;

        for cap in matrix.capabilities.values() {
            match cap {
                impetus_core::ImportCapability::Supported => supported += 1,
                impetus_core::ImportCapability::Partial => partial += 1,
                impetus_core::ImportCapability::Unsupported => _unsupported += 1,
                impetus_core::ImportCapability::Incompatible => _incompatible += 1,
            }
        }

        let total = matrix.capabilities.len();
        if supported == total {
            println!("  ✓ Full support ({} capabilities)", total);
        } else if supported + partial > 0 {
            println!(
                "  ⚠ Partial support ({}/{} capabilities)",
                supported + partial,
                total
            );
        } else {
            println!("  ✗ Not supported");
        }

        // Per-capability breakdown
        let mut caps: Vec<_> = matrix.capabilities.iter().collect();
        caps.sort_by_key(|(name, _)| name.as_str());

        for (cap_name, status) in &caps {
            let icon = match status {
                impetus_core::ImportCapability::Supported => "✓",
                impetus_core::ImportCapability::Partial => "⚠",
                impetus_core::ImportCapability::Unsupported => "○",
                impetus_core::ImportCapability::Incompatible => "✗",
            };
            println!("    {} {} — {:?}", icon, cap_name, status);
        }

        if !matrix.notes.is_empty() {
            for note in &matrix.notes {
                println!("    Note: {}", note);
            }
        }
    }
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

    println!("\n{}", "=".repeat(26));
    println!("Extension Compatibility");
    println!("{}", "=".repeat(26));
    print_compatibility_matrix();
}
