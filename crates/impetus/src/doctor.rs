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

    // Offline built-in agent/skill/command id hygiene (anti-sprawl).
    probe_builtin_ids(&mut report);

    // Offline capability matrix (ARCHITECTURE-aligned); refreshed after daemon diagnostics.
    probe_capability_truth(&mut report, &[]);

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

fn probe_capability_truth(report: &mut DoctorReport, registered_providers: &[String]) {
    report.probes.retain(|probe| {
        probe.name != "capability_matrix" && !probe.name.starts_with("capability.")
    });

    let truth = impetus_core::CapabilityTruthReport::gather(registered_providers);
    report.add(
        ProbeResult::ok(
            "capability_matrix",
            format!(
                "Capability matrix schema v{} ({} rows)",
                truth.schema_version,
                truth.capabilities.len()
            ),
        )
        .with_details(serde_json::to_value(&truth).unwrap_or_default()),
    );

    for entry in &truth.capabilities {
        let name = format!("capability.{}", entry.id);
        let probe = match entry.level {
            impetus_core::CapabilityLevel::Implemented => {
                ProbeResult::ok(name, entry.summary.clone()).with_details(
                    entry
                        .details
                        .clone()
                        .unwrap_or_else(|| serde_json::json!({ "level": "IMPLEMENTED" })),
                )
            }
            impetus_core::CapabilityLevel::Partial => ProbeResult::warn(
                name,
                entry.summary.clone(),
                "Partial: see details; do not treat as full production capability",
            )
            .with_details(
                entry
                    .details
                    .clone()
                    .unwrap_or_else(|| serde_json::json!({ "level": "PARTIAL" })),
            ),
            impetus_core::CapabilityLevel::Missing => {
                ProbeResult::unavailable(name, entry.summary.clone()).with_details(
                    entry
                        .details
                        .clone()
                        .unwrap_or_else(|| serde_json::json!({ "level": "MISSING" })),
                )
            }
        };
        report.add(probe);
    }
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

fn probe_builtin_ids(report: &mut DoctorReport) {
    let audit = impetus_core::audit_shipped_builtin_ids();
    let details = serde_json::to_value(&audit).unwrap_or_default();
    if audit.ok() {
        report.add(
            ProbeResult::ok(
                "builtin_ids",
                format!(
                    "Shipped built-in ids unique ({} entries; unused-detect stub empty)",
                    audit.entries.len()
                ),
            )
            .with_details(details),
        );
    } else {
        let ids: Vec<_> = audit
            .duplicates
            .iter()
            .map(|d| format!("{}:{}", d.kind.as_str(), d.id))
            .collect();
        report.add(
            ProbeResult::error(
                "builtin_ids",
                format!("Duplicate built-in id(s): {}", ids.join(", ")),
                "Remove duplicate registrations from shipped_builtin_ids / sources",
            )
            .with_details(details),
        );
    }
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
                    let providers = subsystems
                        .provider_registry
                        .details
                        .as_ref()
                        .and_then(|details| details.get("providers"))
                        .and_then(|value| serde_json::from_value::<Vec<String>>(value.clone()).ok())
                        .unwrap_or_default();
                    probe_capability_truth(report, &providers);
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
    report.add(status_to_probe(
        "output_optimization",
        subsystems.output_optimization,
    ));

    let browser_status = subsystems
        .web_research
        .details
        .as_ref()
        .and_then(|details| details.get("browser_provider").cloned())
        .and_then(|value| {
            serde_json::from_value::<impetus_core::web_research::BrowserServiceStatus>(value).ok()
        });
    report.add(status_to_probe("web_research", subsystems.web_research));
    match browser_status {
        Some(status) => report.add(browser_status_to_probe(&status)),
        None => report.add(ProbeResult::unavailable(
            "browser_provider",
            "Browser provider status missing from diagnostics",
        )),
    }
}

fn browser_status_to_probe(
    status: &impetus_core::web_research::BrowserServiceStatus,
) -> ProbeResult {
    use impetus_core::web_research::BrowserServiceStatus;

    match status {
        BrowserServiceStatus::Unavailable { reason } => ProbeResult::unavailable(
            "browser_provider",
            format!("Browser provider unavailable: {reason}"),
        )
        .with_details(serde_json::json!({ "status": status })),
        BrowserServiceStatus::Degraded { reason } => ProbeResult::warn(
            "browser_provider",
            format!("Browser provider degraded: {reason}"),
            "Optional browser track; search/fetch remain available without a ready provider",
        )
        .with_details(serde_json::json!({ "status": status })),
        BrowserServiceStatus::Misconfigured { reason } => ProbeResult::error(
            "browser_provider",
            format!("Browser provider misconfigured: {reason}"),
            "Fix browser provider configuration or disable the optional browser module",
        )
        .with_details(serde_json::json!({ "status": status })),
        BrowserServiceStatus::Available {
            provider_id,
            capabilities,
        } => ProbeResult::ok(
            "browser_provider",
            format!("Browser provider available: {provider_id}"),
        )
        .with_details(serde_json::json!({
            "provider_id": provider_id,
            "capabilities": capabilities,
        })),
    }
}

async fn probe_web_research_live(report: &mut DoctorReport) {
    use impetus_core::web_research::{EgressPolicy, WebDoctor, WebResearchEngine};

    let engine = WebResearchEngine::production(EgressPolicy::default());
    let web_report = WebDoctor::probe_engine(&engine).await;

    // Replace offline browser probe with the live report's honest status.
    report
        .probes
        .retain(|probe| probe.name != "browser_provider");
    report.add(browser_status_to_probe(&web_report.browser));

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
    println!("Runtime Capability Matrix");
    println!("{}", "=".repeat(26));
    print_runtime_capability_matrix(report);

    println!("\n{}", "=".repeat(26));
    println!("Extension Compatibility");
    println!("{}", "=".repeat(26));
    print_compatibility_matrix();
}

fn print_runtime_capability_matrix(report: &DoctorReport) {
    let mut rows: Vec<_> = report
        .probes
        .iter()
        .filter(|probe| probe.name.starts_with("capability."))
        .collect();
    rows.sort_by_key(|probe| probe.name.as_str());

    if rows.is_empty() {
        println!("  (no capability probes)");
        return;
    }

    for probe in rows {
        let icon = match probe.status {
            ProbeStatus::Ok => "✓",
            ProbeStatus::Warn => "⚠",
            ProbeStatus::Error => "✗",
            ProbeStatus::Unavailable => "○",
        };
        let id = probe
            .name
            .strip_prefix("capability.")
            .unwrap_or(probe.name.as_str());
        println!("{} {} — {}", icon, id, probe.message);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn doctor_json_includes_honest_capability_matrix_shape() {
        let mut report = DoctorReport::new();
        probe_capability_truth(&mut report, &[]);

        let json = serde_json::to_value(&report).expect("serialize doctor report");
        assert_eq!(json["version"], 1);
        let probes = json["probes"].as_array().expect("probes");
        let matrix = probes
            .iter()
            .find(|probe| probe["name"] == "capability_matrix")
            .expect("capability_matrix probe");
        assert_eq!(matrix["details"]["schema_version"], 1);

        let seatbelt = probes
            .iter()
            .find(|probe| probe["name"] == "capability.seatbelt_process_wrap")
            .expect("seatbelt probe");
        assert_eq!(seatbelt["status"], "OK");
        assert_eq!(seatbelt["details"]["seatbelt_process_wrap"], true);

        let durable = probes
            .iter()
            .find(|probe| probe["name"] == "capability.durable_artifact_store")
            .expect("durable probe");
        assert_eq!(durable["status"], "OK");
        assert_eq!(durable["details"]["durable"], true);

        let schema = probes
            .iter()
            .find(|probe| probe["name"] == "capability.tool_schema_validation")
            .expect("schema probe");
        assert_eq!(schema["status"], "OK");
        assert_eq!(schema["details"]["gate"], true);

        let native = probes
            .iter()
            .find(|probe| probe["name"] == "capability.openai_native_chat_completions")
            .expect("native openai probe");
        assert_eq!(native["status"], "WARN");

        let ext = probes
            .iter()
            .find(|probe| probe["name"] == "capability.extension_runtime")
            .expect("extension runtime probe");
        assert_eq!(ext["status"], "OK");
        assert_eq!(ext["details"]["mcp_live_tools_in_loop"], true);
        assert_eq!(ext["details"]["impetusd_autoload"], true);

        let blob = json.to_string();
        assert!(!blob.contains("sk-"));
        assert!(!blob.to_lowercase().contains("password"));
    }

    #[test]
    fn doctor_json_includes_builtin_ids_probe() {
        let mut report = DoctorReport::new();
        probe_builtin_ids(&mut report);

        let json = serde_json::to_value(&report).expect("serialize");
        let probes = json["probes"].as_array().expect("probes");
        let builtin = probes
            .iter()
            .find(|probe| probe["name"] == "builtin_ids")
            .expect("builtin_ids probe");
        assert_eq!(builtin["status"], "OK");
        assert_eq!(builtin["details"]["duplicates"], serde_json::json!([]));
        assert_eq!(builtin["details"]["unused_stub"], serde_json::json!([]));
        let entries = builtin["details"]["entries"].as_array().expect("entries");
        assert_eq!(entries.len(), 4);
        let blob = json.to_string();
        assert!(!blob.contains("sk-"));
    }

    #[test]
    fn capability_probe_refresh_replaces_rows() {
        let mut report = DoctorReport::new();
        probe_capability_truth(&mut report, &[]);
        let first_count = report
            .probes
            .iter()
            .filter(|p| p.name.starts_with("capability"))
            .count();
        probe_capability_truth(&mut report, &["openai".into()]);
        let second_count = report
            .probes
            .iter()
            .filter(|p| p.name.starts_with("capability"))
            .count();
        assert_eq!(first_count, second_count);

        let native = report
            .probes
            .iter()
            .find(|p| p.name == "capability.openai_native_chat_completions")
            .expect("native");
        assert_eq!(native.status, ProbeStatus::Ok);
    }
}
