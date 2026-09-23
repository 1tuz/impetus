//! ACP Gateway — official SDK V2 is the production path.
//!
//! Legacy custom JSON-RPC [`gateway`] stays for migration/tests only and is
//! **not** re-exported at the crate root. Production selection
//! (`Harness::with_acp_gateway`) uses [`AcpGatewayV2`] / [`ProductionAcpGateway`].

pub mod gateway_v2;
pub mod health;
pub mod mock;
pub mod profile;
pub mod redact;
pub mod registry;
pub mod session_config;

/// Legacy custom JSON-RPC gateway (quarantined from production selection).
///
/// Import as `impetus_acp_gateway::gateway::AcpGateway` in migration/tests only.
pub mod gateway;

pub use gateway_v2::{
    AcpGatewayV2, CachedAgentCapabilities, GatewayState, GatewayV2Error, PermissionChoiceKind,
    PermissionDecision, PermissionKind, PermissionOption, PermissionRequest, StreamUpdate,
    permission_outcome,
};
pub use health::{AcpBackendStatus, AcpHealthKind};
pub use mock::MockAgent;
pub use profile::{
    ACP_CHILD_CONTROL_OK_ENV, ACP_CHILD_ENV, AcpProfile, CredentialStrategy, agent_sdk_env_overlay,
};
pub use redact::{StreamExportAudit, redact_json, redact_text};
pub use registry::{
    AgentCandidate, BUILTIN_CANDIDATES, DiscoveredAgent, discover_agents, path_dirs_from_env,
    probe_version,
};
pub use session_config::{
    ConfigOptionSet, SessionConfigApplyError, SessionLaunchOptions, advertised_model_ids,
    advertised_thought_levels, plan_config_option_sets,
};

/// Production gateway type alias — documents the only supported selection.
pub type ProductionAcpGateway = AcpGatewayV2;

#[cfg(test)]
mod production_selection_tests {
    use super::*;

    #[test]
    fn production_alias_constructs_gateway_v2() {
        let _gw: ProductionAcpGateway =
            AcpGatewayV2::new(agent_client_protocol::AcpAgentConfig::new("echo"));
    }
}
