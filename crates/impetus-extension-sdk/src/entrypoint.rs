//! Typed extension entrypoints (closed set for v1).
//!
//! No `dlopen` / arbitrary in-process native ABI in v1.

use std::path::{Component, Path};

use serde::{Deserialize, Serialize};

/// How the host loads and runs an extension package.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ExtensionEntrypoint {
    /// Declarative skills/instructions under a pack root.
    InstructionPack { root: String },
    /// Declares an MCP module id; host enables/disables daemon MCP SoT on activate.
    McpBridge { module_id: String },
    /// Out-of-process host; crash-isolated JSON-RPC over stdio
    /// (`impetus_extension_sdk::host_protocol`).
    HostProcess {
        command: String,
        #[serde(default)]
        args: Vec<String>,
    },
}

/// Entrypoint field validation failure.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EntrypointError {
    #[error("entrypoint `{kind}` field `{field}` must be non-empty")]
    EmptyField {
        kind: &'static str,
        field: &'static str,
    },
    #[error(
        "instruction_pack root `{root}` must be a relative path without `..` (contained under the package directory)"
    )]
    UnsafeRoot { root: String },
}

impl ExtensionEntrypoint {
    /// Validate required non-empty fields for the selected kind.
    pub fn validate(&self) -> Result<(), EntrypointError> {
        match self {
            Self::InstructionPack { root } => {
                if root.trim().is_empty() {
                    return Err(EntrypointError::EmptyField {
                        kind: "instruction_pack",
                        field: "root",
                    });
                }
                if !is_safe_relative_root(root) {
                    return Err(EntrypointError::UnsafeRoot { root: root.clone() });
                }
            }
            Self::McpBridge { module_id } => {
                if module_id.trim().is_empty() {
                    return Err(EntrypointError::EmptyField {
                        kind: "mcp_bridge",
                        field: "module_id",
                    });
                }
            }
            Self::HostProcess { command, .. } => {
                if command.trim().is_empty() {
                    return Err(EntrypointError::EmptyField {
                        kind: "host_process",
                        field: "command",
                    });
                }
            }
        }
        Ok(())
    }
}

/// Reject absolute paths and `..` components in pack-relative roots.
pub fn is_safe_relative_root(root: &str) -> bool {
    let path = Path::new(root.trim());
    if path.is_absolute() {
        return false;
    }
    path.components().all(|c| matches!(c, Component::Normal(_)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_absolute_and_parent_roots() {
        assert!(!is_safe_relative_root("/tmp/escape"));
        assert!(!is_safe_relative_root("../escape"));
        assert!(!is_safe_relative_root("skills/../secret"));
        assert!(is_safe_relative_root("skills"));
        assert!(is_safe_relative_root("skills/nested"));
    }
}
