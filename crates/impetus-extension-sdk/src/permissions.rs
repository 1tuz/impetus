//! Explicit extension permission tokens (default deny).

use std::collections::HashSet;
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Permission category declared in an extension package manifest.
///
/// Maps into host Policy → Approval → Sandbox. Extensions cannot grant
/// themselves `origin=user` or bypass RiskGate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExtensionPermission {
    FilesystemRead,
    FilesystemWrite,
    Network,
    ProcessSpawn,
    Pty,
    Git,
    Mcp,
    Browser,
    Lsp,
    Memory,
    SecretsProvider,
}

impl ExtensionPermission {
    /// Snake_case wire token.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::FilesystemRead => "filesystem_read",
            Self::FilesystemWrite => "filesystem_write",
            Self::Network => "network",
            Self::ProcessSpawn => "process_spawn",
            Self::Pty => "pty",
            Self::Git => "git",
            Self::Mcp => "mcp",
            Self::Browser => "browser",
            Self::Lsp => "lsp",
            Self::Memory => "memory",
            Self::SecretsProvider => "secrets_provider",
        }
    }
}

impl fmt::Display for ExtensionPermission {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for ExtensionPermission {
    type Err = PermissionsError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "filesystem_read" => Ok(Self::FilesystemRead),
            "filesystem_write" => Ok(Self::FilesystemWrite),
            "network" => Ok(Self::Network),
            "process_spawn" => Ok(Self::ProcessSpawn),
            "pty" => Ok(Self::Pty),
            "git" => Ok(Self::Git),
            "mcp" => Ok(Self::Mcp),
            "browser" => Ok(Self::Browser),
            "lsp" => Ok(Self::Lsp),
            "memory" => Ok(Self::Memory),
            "secrets_provider" => Ok(Self::SecretsProvider),
            other => Err(PermissionsError::Unknown {
                token: other.to_string(),
            }),
        }
    }
}

/// Permission list parse / uniqueness errors.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum PermissionsError {
    #[error("unknown permission token `{token}`")]
    Unknown { token: String },
    #[error("duplicate permission `{token}`")]
    Duplicate { token: String },
}

/// Reject duplicate permission tokens in a declared list.
pub fn validate_permissions(permissions: &[ExtensionPermission]) -> Result<(), PermissionsError> {
    let mut seen = HashSet::with_capacity(permissions.len());
    for permission in permissions {
        if !seen.insert(*permission) {
            return Err(PermissionsError::Duplicate {
                token: permission.as_str().to_string(),
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serde_snake_case() {
        let json = serde_json::to_string(&ExtensionPermission::FilesystemRead).unwrap();
        assert_eq!(json, "\"filesystem_read\"");
        let back: ExtensionPermission = serde_json::from_str(&json).unwrap();
        assert_eq!(back, ExtensionPermission::FilesystemRead);
    }

    #[test]
    fn uniqueness() {
        assert!(validate_permissions(&[ExtensionPermission::Network]).is_ok());
        let err = validate_permissions(&[ExtensionPermission::Git, ExtensionPermission::Git])
            .unwrap_err();
        assert!(matches!(err, PermissionsError::Duplicate { .. }));
    }

    #[test]
    fn parse_known() {
        assert_eq!(
            "secrets_provider".parse::<ExtensionPermission>().unwrap(),
            ExtensionPermission::SecretsProvider
        );
        assert!("nope".parse::<ExtensionPermission>().is_err());
    }
}
