//! Bridge: extension manifest permissions → Policy / SandboxScope.
//!
//! Declared tokens do **not** bypass RiskGate or grant `origin=user`.
//! Hard-Deny against the current sandox scope blocks package activation;
//! Allow / NeedsApproval still go through normal Policy at action time.
//!
//! Extension-first (#336): `Lsp` / `Browser` permissions gate activation of
//! `LspIntegration` / `BrowserIntegration` packs. Core keeps coding-tools +
//! browser negotiate IPC contracts; CDP/WebDriver stay out of core.

use impetus_extension_sdk::ExtensionPermission;
use impetus_protocol::{ActionKind, PolicyDecision};

use crate::policy::SandboxScope;

/// Map a permission token to the ActionKind(s) it covers for Policy.
pub fn action_kinds_for_permission(permission: ExtensionPermission) -> &'static [ActionKind] {
    match permission {
        ExtensionPermission::FilesystemRead => &[ActionKind::ReadFile],
        ExtensionPermission::FilesystemWrite => &[ActionKind::WriteFile],
        ExtensionPermission::Network => &[
            ActionKind::NetworkConnect,
            ActionKind::SshConnect,
            ActionKind::SftpTransfer,
        ],
        ExtensionPermission::ProcessSpawn => &[ActionKind::SpawnProcess],
        ExtensionPermission::Pty => &[ActionKind::TmuxAttach],
        ExtensionPermission::Git => &[ActionKind::SpawnProcess],
        ExtensionPermission::Mcp => &[ActionKind::NetworkConnect],
        ExtensionPermission::Browser => &[
            ActionKind::WebBrowser,
            ActionKind::WebFetch,
            ActionKind::WebSearch,
        ],
        ExtensionPermission::Lsp => &[ActionKind::SpawnProcess],
        ExtensionPermission::Memory => &[],
        ExtensionPermission::SecretsProvider => &[],
    }
}

/// Fail-closed scope check used at package activate / auto-activate.
pub fn evaluate_permission_against_scope(
    permission: ExtensionPermission,
    scope: &SandboxScope,
) -> PolicyDecision {
    match permission {
        ExtensionPermission::Network | ExtensionPermission::Browser => {
            if !scope.allow_network {
                PolicyDecision::Deny {
                    reason: format!(
                        "extension permission `{}` requires network; sandbox has allow_network=false",
                        permission.as_str()
                    ),
                }
            } else {
                PolicyDecision::NeedsApproval {
                    reason: format!(
                        "extension declared `{}` (still subject to Policy at action time)",
                        permission.as_str()
                    ),
                }
            }
        }
        ExtensionPermission::Mcp
        | ExtensionPermission::FilesystemWrite
        | ExtensionPermission::ProcessSpawn
        | ExtensionPermission::Pty
        | ExtensionPermission::Git => PolicyDecision::NeedsApproval {
            reason: format!(
                "extension declared `{}` (still subject to Policy at action time)",
                permission.as_str()
            ),
        },
        ExtensionPermission::FilesystemRead
        | ExtensionPermission::Lsp
        | ExtensionPermission::Memory => PolicyDecision::Allow,
        ExtensionPermission::SecretsProvider => PolicyDecision::NeedsApproval {
            reason: "secrets_provider resolves Keychain references only; no raw token grant".into(),
        },
    }
}

/// Activate-time permission_eval: any hard-Deny against scope fails the pack.
pub fn permission_eval(
    permissions: &[ExtensionPermission],
    scope: &SandboxScope,
) -> Result<(), String> {
    for permission in permissions {
        if let PolicyDecision::Deny { reason } =
            evaluate_permission_against_scope(*permission, scope)
        {
            return Err(reason);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn scope(network: bool) -> SandboxScope {
        SandboxScope::local_workspace(PathBuf::from("/tmp/ws")).with_network(network)
    }

    #[test]
    fn filesystem_read_allows_activate() {
        assert!(permission_eval(&[ExtensionPermission::FilesystemRead], &scope(false)).is_ok());
    }

    #[test]
    fn network_denied_when_sandbox_blocks() {
        let err = permission_eval(&[ExtensionPermission::Network], &scope(false)).unwrap_err();
        assert!(err.contains("allow_network=false"));
    }

    #[test]
    fn network_ok_when_sandbox_allows() {
        assert!(permission_eval(&[ExtensionPermission::Network], &scope(true)).is_ok());
    }

    #[test]
    fn action_kinds_cover_read_write() {
        assert_eq!(
            action_kinds_for_permission(ExtensionPermission::FilesystemRead),
            &[ActionKind::ReadFile]
        );
        assert_eq!(
            action_kinds_for_permission(ExtensionPermission::FilesystemWrite),
            &[ActionKind::WriteFile]
        );
    }
}
