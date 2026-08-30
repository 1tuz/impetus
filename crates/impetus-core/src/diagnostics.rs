use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SubsystemHealth {
    pub event_store: SubsystemStatus,
    pub artifact_store: SubsystemStatus,
    pub policy_engine: SubsystemStatus,
    pub provider_registry: SubsystemStatus,
    pub sandbox: SubsystemStatus,
    pub credential_store: SubsystemStatus,
    pub tools_capabilities: SubsystemStatus,
    pub external_agents: SubsystemStatus,
    pub optional_modules: SubsystemStatus,
    pub disk_runtime: SubsystemStatus,
    pub web_research: SubsystemStatus,
    pub output_optimization: SubsystemStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SubsystemStatus {
    pub available: bool,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,
}

impl SubsystemStatus {
    pub fn ok(message: impl Into<String>) -> Self {
        Self {
            available: true,
            message: message.into(),
            details: None,
        }
    }

    pub fn unavailable(message: impl Into<String>) -> Self {
        Self {
            available: false,
            message: message.into(),
            details: None,
        }
    }

    pub fn with_details(mut self, details: serde_json::Value) -> Self {
        self.details = Some(details);
        self
    }
}
