//! ACP backend health / status surface (labels only).

use crate::gateway_v2::{CachedAgentCapabilities, GatewayState};
use serde::{Deserialize, Serialize};

/// Coarse health for provider/doctor surfaces.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AcpHealthKind {
    Unknown,
    Healthy,
    Unavailable,
}

/// Snapshot suitable for doctor / status IPC (no secrets).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcpBackendStatus {
    pub health: AcpHealthKind,
    pub gateway_state: GatewayState,
    pub agent_name: Option<String>,
    pub agent_version: Option<String>,
    pub auth_method_ids: Vec<String>,
    pub detail_redacted: Option<String>,
}

impl AcpBackendStatus {
    pub fn from_state(state: GatewayState, caps: Option<&CachedAgentCapabilities>) -> Self {
        let (health, detail_redacted) = match state {
            GatewayState::Ready => (AcpHealthKind::Healthy, None),
            GatewayState::NotStarted | GatewayState::Initializing | GatewayState::AuthRequired => {
                (AcpHealthKind::Unknown, None)
            }
            GatewayState::Incompatible => (
                AcpHealthKind::Unavailable,
                Some("agent protocol or auth incompatible".into()),
            ),
            GatewayState::Crashed => (
                AcpHealthKind::Unavailable,
                Some("agent process crashed or disconnected".into()),
            ),
        };
        Self {
            health,
            gateway_state: state,
            agent_name: caps.and_then(|c| c.agent_name.clone()),
            agent_version: caps.and_then(|c| c.agent_version.clone()),
            auth_method_ids: caps.map(|c| c.auth_method_ids.clone()).unwrap_or_default(),
            detail_redacted,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ready_maps_to_healthy() {
        let status = AcpBackendStatus::from_state(GatewayState::Ready, None);
        assert_eq!(status.health, AcpHealthKind::Healthy);
    }

    #[test]
    fn crashed_is_unavailable_with_label() {
        let status = AcpBackendStatus::from_state(GatewayState::Crashed, None);
        assert_eq!(status.health, AcpHealthKind::Unavailable);
        assert!(status.detail_redacted.is_some());
    }

    #[test]
    fn caps_surface_agent_labels_only() {
        let caps = CachedAgentCapabilities {
            agent_name: Some("mock-agent".into()),
            agent_version: Some("1.2.3".into()),
            auth_method_ids: vec!["env".into()],
            ..Default::default()
        };
        let status = AcpBackendStatus::from_state(GatewayState::Ready, Some(&caps));
        assert_eq!(status.agent_name.as_deref(), Some("mock-agent"));
        assert_eq!(status.agent_version.as_deref(), Some("1.2.3"));
        assert_eq!(status.auth_method_ids, vec!["env"]);
    }
}
