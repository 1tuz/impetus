//! Runtime capability truth for `impetus doctor` / Diagnostics.
//!
//! Values mirror the architecture capability matrix: code-backed claims only.
//! Seatbelt process wrap stays false until production exec wires `sandbox-exec`.

use serde::{Deserialize, Serialize};

/// Architecture-aligned capability level (not extension ImportCapability).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum CapabilityLevel {
    Implemented,
    Partial,
    Missing,
}

/// One machine-readable capability row for doctor JSON / docs checks.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CapabilityEntry {
    pub id: String,
    pub level: CapabilityLevel,
    pub summary: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,
}

/// Snapshot of harness capability truth, optionally enriched with live providers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CapabilityTruthReport {
    pub schema_version: u16,
    pub capabilities: Vec<CapabilityEntry>,
}

fn entry(
    id: &str,
    level: CapabilityLevel,
    summary: &str,
    details: Option<serde_json::Value>,
) -> CapabilityEntry {
    CapabilityEntry {
        id: id.to_string(),
        level,
        summary: summary.to_string(),
        details,
    }
}

impl CapabilityTruthReport {
    /// Static + compile-time truths. `registered_providers` is live daemon state when known.
    pub fn gather(registered_providers: &[String]) -> Self {
        let schema_count = crate::builtin_tool_schemas().len();
        let native_wired = registered_providers
            .iter()
            .any(|id| id == "openai" || id.starts_with("openai-"));
        // Daemon `--provider-profile` registers OpenAiCompatibleAdapter today, not OpenAiProvider.
        let compat_wired =
            !registered_providers.is_empty() && registered_providers.iter().any(|id| id != "mock");
        let openai_native_level = if native_wired {
            CapabilityLevel::Implemented
        } else {
            CapabilityLevel::Partial
        };
        let openai_native_summary = if native_wired {
            "Native OpenAI Chat Completions tool-call SSE wired in provider registry"
        } else {
            "OpenAiProvider (tool-call SSE) exists; daemon --provider-profile uses OpenAiCompatibleAdapter"
        };

        Self {
            schema_version: 1,
            capabilities: vec![
                entry(
                    "sandbox_path_scope",
                    CapabilityLevel::Implemented,
                    "Path/network scope admission fail-closed before execution",
                    Some(serde_json::json!({
                        "admission": "path_scope",
                        "fail_closed": true,
                    })),
                ),
                entry(
                    "seatbelt_process_wrap",
                    CapabilityLevel::Partial,
                    "macOS Seatbelt spike only; not wired in process/tool exec",
                    Some(serde_json::json!({
                        "seatbelt_process_wrap": false,
                        "spike": "crates/impetus-core/tests/macos_sandbox_spike.rs",
                        "production_exec": "execution/process.rs",
                    })),
                ),
                entry(
                    "durable_artifact_store",
                    CapabilityLevel::Implemented,
                    "DurableArtifactStore SHA-256 restart-safe bodies for tools/web/shell/upload",
                    Some(serde_json::json!({
                        "durable": true,
                        "module": "durable_artifacts",
                    })),
                ),
                entry(
                    "ephemeral_attachment_store",
                    CapabilityLevel::Implemented,
                    "AttachmentStore in-memory backing for approval previews only",
                    Some(serde_json::json!({
                        "durable": false,
                        "module": "attachments",
                    })),
                ),
                entry(
                    "tool_schema_validation",
                    CapabilityLevel::Implemented,
                    "JSON Schema tool-arg gate before policy/execution",
                    Some(serde_json::json!({
                        "gate": true,
                        "builtin_schema_count": schema_count,
                        "provider_http_tools": false,
                    })),
                ),
                entry(
                    "openai_native_chat_completions",
                    openai_native_level,
                    openai_native_summary,
                    Some(serde_json::json!({
                        "library": "openai_provider::OpenAiProvider",
                        "daemon_default": "mock",
                        "registered_providers": registered_providers,
                        "native_wired": native_wired,
                        "compat_adapter_in_tree": true,
                    })),
                ),
                entry(
                    "openai_compat_text_adapter",
                    if compat_wired {
                        CapabilityLevel::Implemented
                    } else {
                        CapabilityLevel::Partial
                    },
                    if compat_wired {
                        "OpenAiCompatibleAdapter registered via --provider-profile"
                    } else {
                        "OpenAiCompatibleAdapter in tree; not registered until --provider-profile"
                    },
                    Some(serde_json::json!({
                        "registered_providers": registered_providers,
                        "compat_wired": compat_wired,
                    })),
                ),
                entry(
                    "extension_import",
                    CapabilityLevel::Implemented,
                    "Import adapters for Skills/MCP/Claude/Codex/Cursor/Plugins",
                    Some(serde_json::json!({
                        "adapters": [
                            "agent_skills",
                            "mcp",
                            "claude_code",
                            "codex",
                            "cursor",
                            "agent_plugins"
                        ],
                    })),
                ),
                entry(
                    "extension_runtime",
                    CapabilityLevel::Partial,
                    "Skills via filesystem InstructionResolver; MCP tools not live in agent loop",
                    Some(serde_json::json!({
                        "skills_instruction_resolver": true,
                        "mcp_live_tools_in_loop": false,
                        "lifecycle_plan_apply_ownership": false,
                    })),
                ),
            ],
        }
    }

    pub fn entry(&self, id: &str) -> Option<&CapabilityEntry> {
        self.capabilities.iter().find(|entry| entry.id == id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capability_truth_seatbelt_wrap_false_and_durable_artifacts() {
        let report = CapabilityTruthReport::gather(&[]);
        let seatbelt = report.entry("seatbelt_process_wrap").expect("seatbelt row");
        assert_eq!(seatbelt.level, CapabilityLevel::Partial);
        assert_eq!(
            seatbelt.details.as_ref().unwrap()["seatbelt_process_wrap"],
            false
        );

        let durable = report.entry("durable_artifact_store").expect("durable row");
        assert_eq!(durable.level, CapabilityLevel::Implemented);
        assert_eq!(durable.details.as_ref().unwrap()["durable"], true);

        let schema = report.entry("tool_schema_validation").expect("schema row");
        assert_eq!(schema.level, CapabilityLevel::Implemented);
        assert_eq!(schema.details.as_ref().unwrap()["gate"], true);
        assert!(
            schema.details.as_ref().unwrap()["builtin_schema_count"]
                .as_u64()
                .unwrap()
                >= 1
        );

        let native = report
            .entry("openai_native_chat_completions")
            .expect("openai native");
        assert_eq!(native.level, CapabilityLevel::Partial);

        let ext_rt = report.entry("extension_runtime").expect("ext runtime");
        assert_eq!(ext_rt.level, CapabilityLevel::Partial);
        assert_eq!(
            ext_rt.details.as_ref().unwrap()["mcp_live_tools_in_loop"],
            false
        );

        let json = serde_json::to_value(&report).expect("serialize");
        assert_eq!(json["schema_version"], 1);
        assert!(json["capabilities"].as_array().unwrap().len() >= 8);
        let blob = json.to_string();
        assert!(!blob.contains("sk-"));
        assert!(!blob.contains("Bearer "));
    }

    #[test]
    fn capability_truth_marks_native_when_openai_provider_registered() {
        let report = CapabilityTruthReport::gather(&["mock".into(), "openai".into()]);
        let native = report
            .entry("openai_native_chat_completions")
            .expect("openai native");
        assert_eq!(native.level, CapabilityLevel::Implemented);
        assert_eq!(native.details.as_ref().unwrap()["native_wired"], true);
    }
}
