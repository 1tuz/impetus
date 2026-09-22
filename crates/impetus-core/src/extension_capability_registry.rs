//! Typed capability registry view over Active extension packages.
//!
//! AgentLoop / Context consume this view — not install_state path scans.

use std::path::PathBuf;

use impetus_extension_sdk::{ExtensionCapabilityKind, ExtensionId, ExtensionPermission};

/// Snapshot of Active packages for AgentLoop / IPC consumers.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExtensionCapabilityRegistry {
    /// Absolute skill roots from Active `instruction_pack` packages.
    pub skill_roots: Vec<PathBuf>,
    /// Declared capabilities on Active packages.
    pub capabilities: Vec<(ExtensionId, ExtensionCapabilityKind)>,
    /// Declared permissions on Active packages (inventory; Policy gate at activate).
    pub permissions: Vec<(ExtensionId, ExtensionPermission)>,
    /// Extension ids with a live `host_process` session.
    pub host_process_ids: Vec<String>,
}

impl ExtensionCapabilityRegistry {
    pub fn skill_roots(&self) -> &[PathBuf] {
        &self.skill_roots
    }

    pub fn is_empty(&self) -> bool {
        self.skill_roots.is_empty()
            && self.capabilities.is_empty()
            && self.host_process_ids.is_empty()
    }
}
