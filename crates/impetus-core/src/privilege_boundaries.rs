//! Privilege-boundary invariants for the Impetus trusted kernel.
//!
//! **Principle:** normal `impetus` / `impetusd` flows never require a user or
//! administrator password, `sudo`, root, or interactive privilege escalation.
//! Optional Keychain credential resolution must stay silent (no unlock UI).
//! Seatbelt / path sandbox stays userspace.

use crate::risk_gate::{RiskContext, RiskGate, RiskGateDecision};
use crate::{ActionOrigin, ExecutionMode, NormalizedEffect};

/// Returns true when a shell token list looks like privilege escalation.
pub fn command_requests_privilege_escalation(command: &str) -> bool {
    command
        .split_whitespace()
        .any(|token| matches!(token, "sudo" | "doas" | "su" | "pkexec"))
}

/// RiskGate must hard-deny agent sudo even in Bypass (fail-closed).
pub fn risk_gate_denies_sudo(gate: &dyn RiskGate) -> bool {
    let effect =
        NormalizedEffect::process_spawn(ActionOrigin::Agent, "sudo apt", "sudo apt update");
    let ctx = RiskContext {
        mode: ExecutionMode::Bypass,
        effect: &effect,
        argv: None,
    };
    matches!(gate.classify(&ctx), RiskGateDecision::Deny { .. })
}

/// Default PTY shell argv must not force a login shell (`-l` / `--login`).
/// Login shells re-source profile scripts that can trigger password prompts.
pub fn pty_argv_is_non_login(args: &[String]) -> bool {
    !args.iter().any(|a| a == "-l" || a == "--login")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::risk_gate::DeterministicRiskGate;

    #[test]
    fn sudo_tokens_detected() {
        assert!(command_requests_privilege_escalation("sudo apt update"));
        assert!(command_requests_privilege_escalation("doas pacman -Syu"));
        assert!(command_requests_privilege_escalation("su -"));
        assert!(!command_requests_privilege_escalation("echo hello"));
        assert!(!command_requests_privilege_escalation("cargo test"));
    }

    #[test]
    fn risk_gate_hard_denies_sudo_in_bypass() {
        assert!(risk_gate_denies_sudo(&DeterministicRiskGate));
    }

    #[test]
    fn default_pty_shell_has_no_login_flag() {
        // Mirror TUI default: `$SHELL` with empty args (see impetus-tui pty_passthrough).
        let args: Vec<String> = Vec::new();
        assert!(pty_argv_is_non_login(&args));
        assert!(!pty_argv_is_non_login(&["-l".into()]));
        assert!(!pty_argv_is_non_login(&["--login".into()]));
    }

    #[test]
    fn execution_pty_source_has_no_login_shell_flags() {
        let src = include_str!("execution/pty.rs");
        assert!(
            !src.contains("\"-l\"") && !src.contains("\"--login\""),
            "PTY spawn path must not hardcode login-shell flags"
        );
    }

    #[test]
    fn sandbox_source_uses_userspace_sandbox_exec_only() {
        let src = include_str!("execution/sandbox.rs");
        assert!(
            src.contains("/usr/bin/sandbox-exec"),
            "Seatbelt must wrap via userspace sandbox-exec"
        );
        for needle in [
            "sudo ",
            "osascript",
            "AuthorizationCreate",
            "SFAuthorization",
        ] {
            assert!(
                !src.contains(needle),
                "sandbox must not escalate privileges via `{needle}`"
            );
        }
    }
}
