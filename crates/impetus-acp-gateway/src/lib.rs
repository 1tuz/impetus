//! ACP Gateway для v0.3 — связывает harness с external coding-agent CLI.
//!
//! Один дочерний process на profile; stdout для ACP JSON-RPC, stderr для logs.
//! ACP session → внутренняя durable Session; permission проходит Policy.

pub mod gateway;
pub mod gateway_v2;
pub mod mock;
pub mod profile;
pub mod session_config;

pub use gateway::{AcpGateway, AgentStatus, GatewayError};
pub use gateway_v2::{
    AcpGatewayV2, CachedAgentCapabilities, GatewayState, GatewayV2Error, PermissionChoiceKind,
    PermissionDecision, PermissionKind, PermissionOption, PermissionRequest, StreamUpdate,
    permission_outcome,
};
pub use mock::MockAgent;
pub use profile::{ACP_CHILD_ENV, AcpProfile, CredentialStrategy, agent_sdk_env_overlay};
pub use session_config::{
    ConfigOptionSet, SessionConfigApplyError, SessionLaunchOptions, advertised_model_ids,
    advertised_thought_levels, plan_config_option_sets,
};
