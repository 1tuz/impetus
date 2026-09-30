//! Internal helpers shared by [`crate::module_fallback`].
//!
//! Not a plugin API. Sole public extension substrate is
//! [`crate::extension_host::ExtensionHost`]
//! (`instruction_pack` / `mcp_bridge` / `host_process`).

use serde::{Deserialize, Serialize};

/// Classification used by [`crate::module_fallback::FallbackPolicy`] defaults.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModuleKind {
    AgentLoop,
    Scheduler,
    ToolProvider,
    SearchBackend,
    BrowserProvider,
    CredentialResolver,
    PolicyExtension,
    Custom,
}

pub use impetus_protocol::ExecutionSemantics;
