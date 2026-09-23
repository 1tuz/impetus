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

    /// First Active package that declares `kind` **and** has a live host_process.
    ///
    /// Used by public capability routing (LSP/Browser/Memory/Context operate).
    pub fn active_host_for(&self, kind: ExtensionCapabilityKind) -> Option<&str> {
        for (id, cap) in &self.capabilities {
            if *cap != kind {
                continue;
            }
            let id_str = id.as_str();
            if self.host_process_ids.iter().any(|h| h == id_str) {
                return Some(id_str);
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use impetus_extension_sdk::ExtensionId;

    #[test]
    fn active_host_for_requires_live_process() {
        let id = ExtensionId::new("lsp-pack").unwrap();
        let reg = ExtensionCapabilityRegistry {
            capabilities: vec![(id.clone(), ExtensionCapabilityKind::LspIntegration)],
            host_process_ids: vec![],
            ..Default::default()
        };
        assert!(
            reg.active_host_for(ExtensionCapabilityKind::LspIntegration)
                .is_none()
        );

        let reg = ExtensionCapabilityRegistry {
            capabilities: vec![(id, ExtensionCapabilityKind::LspIntegration)],
            host_process_ids: vec!["lsp-pack".into()],
            ..Default::default()
        };
        assert_eq!(
            reg.active_host_for(ExtensionCapabilityKind::LspIntegration),
            Some("lsp-pack")
        );
    }
}
