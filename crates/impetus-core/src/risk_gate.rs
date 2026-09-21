//! Deterministic, argv/effects-aware risk gate after hard Policy + execution mode.
//!
//! Separate from [`crate::hook_prefilter`] (performance prefilter only). No cloud
//! classifier — fail-closed heuristics for opaque shell and privilege escalation.

use crate::{Action, ActionKind, ActionOrigin, EffectCapability, ExecutionMode, NormalizedEffect};
use std::sync::Arc;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RiskGateDecision {
    Allow,
    NeedsHumanApproval { reason: String },
    Deny { reason: String },
}

#[derive(Debug, Clone)]
pub struct RiskContext<'a> {
    pub mode: ExecutionMode,
    pub effect: &'a NormalizedEffect,
    /// Full argv when the caller has it (process spawn path).
    pub argv: Option<&'a [String]>,
}

pub trait RiskGate: Send + Sync {
    fn classify(&self, ctx: &RiskContext<'_>) -> RiskGateDecision;
}

/// Default in-process risk gate: deterministic heuristics, fail-closed on opaque shell.
#[derive(Debug, Clone, Copy, Default)]
pub struct DeterministicRiskGate;

impl RiskGate for DeterministicRiskGate {
    fn classify(&self, ctx: &RiskContext<'_>) -> RiskGateDecision {
        let effect = ctx.effect;
        let command = command_text(ctx.argv, &effect.action);

        if contains_privilege_escalation(&command) {
            return RiskGateDecision::Deny {
                reason: "privilege escalation command is never auto-allowed".into(),
            };
        }
        if contains_destructive_git(&command) {
            return RiskGateDecision::Deny {
                reason: "destructive git command is never auto-allowed".into(),
            };
        }
        if path_escape_in_target(effect) {
            return RiskGateDecision::Deny {
                reason: "path escape attempt outside workspace scope".into(),
            };
        }

        if effect.action.origin == ActionOrigin::User {
            return RiskGateDecision::Allow;
        }

        if effect.action.kind == ActionKind::SpawnProcess && is_opaque_shell(ctx.argv, &command) {
            return RiskGateDecision::NeedsHumanApproval {
                reason: "opaque shell invocation requires human review".into(),
            };
        }

        match effect.action.kind {
            ActionKind::ReadFile => RiskGateDecision::Allow,
            ActionKind::WriteFile => classify_workspace_write(ctx.mode),
            ActionKind::SpawnProcess => classify_process_spawn(ctx.mode, &command),
            ActionKind::WebSearch | ActionKind::WebFetch => RiskGateDecision::Allow,
            ActionKind::WebDownload
            | ActionKind::WebBrowser
            | ActionKind::WebSubmit
            | ActionKind::WebUpload => classify_outbound_web(ctx.mode),
            ActionKind::NetworkConnect
            | ActionKind::SshConnect
            | ActionKind::SftpTransfer
            | ActionKind::TmuxAttach => classify_network_like(ctx.mode),
        }
    }
}

fn classify_workspace_write(mode: ExecutionMode) -> RiskGateDecision {
    match mode {
        ExecutionMode::Plan => RiskGateDecision::Deny {
            reason: "PLAN mode denies workspace writes".into(),
        },
        ExecutionMode::Ask => RiskGateDecision::NeedsHumanApproval {
            reason: "workspace write requires human approval in ASK mode".into(),
        },
        ExecutionMode::AcceptEdits | ExecutionMode::Auto | ExecutionMode::Bypass => {
            RiskGateDecision::Allow
        }
    }
}

fn classify_process_spawn(mode: ExecutionMode, command: &str) -> RiskGateDecision {
    if matches!(mode, ExecutionMode::Plan) {
        return RiskGateDecision::Deny {
            reason: "PLAN mode denies process spawn".into(),
        };
    }
    if is_safe_readonly_command(command) {
        return RiskGateDecision::Allow;
    }
    match mode {
        ExecutionMode::Ask => RiskGateDecision::NeedsHumanApproval {
            reason: "process spawn requires human approval in ASK mode".into(),
        },
        ExecutionMode::AcceptEdits | ExecutionMode::Auto => RiskGateDecision::NeedsHumanApproval {
            reason: "mutating or non-readonly process requires human approval".into(),
        },
        ExecutionMode::Bypass => RiskGateDecision::Allow,
        ExecutionMode::Plan => unreachable!("plan handled above"),
    }
}

fn classify_outbound_web(mode: ExecutionMode) -> RiskGateDecision {
    match mode {
        ExecutionMode::Plan => RiskGateDecision::Deny {
            reason: "PLAN mode denies outbound web actions".into(),
        },
        ExecutionMode::Ask => RiskGateDecision::NeedsHumanApproval {
            reason: "outbound web requires human approval in ASK mode".into(),
        },
        ExecutionMode::AcceptEdits | ExecutionMode::Auto | ExecutionMode::Bypass => {
            RiskGateDecision::NeedsHumanApproval {
                reason: "outbound web requires human approval".into(),
            }
        }
    }
}

fn classify_network_like(mode: ExecutionMode) -> RiskGateDecision {
    match mode {
        ExecutionMode::Plan => RiskGateDecision::Deny {
            reason: "PLAN mode denies network actions".into(),
        },
        ExecutionMode::Ask | ExecutionMode::AcceptEdits | ExecutionMode::Auto => {
            RiskGateDecision::NeedsHumanApproval {
                reason: "network action requires human approval".into(),
            }
        }
        ExecutionMode::Bypass => RiskGateDecision::Allow,
    }
}

pub fn command_text(argv: Option<&[String]>, action: &Action) -> String {
    if let Some(argv) = argv {
        if argv.is_empty() {
            return action.summary.clone();
        }
        return argv.join(" ");
    }
    match action.target.as_deref() {
        Some(target) => format!("{} {}", action.summary, target),
        None => action.summary.clone(),
    }
}

pub fn is_opaque_shell(argv: Option<&[String]>, command: &str) -> bool {
    if let Some(argv) = argv
        && argv.len() >= 2
    {
        let exe = argv[0].rsplit('/').next().unwrap_or(&argv[0]);
        if matches!(exe, "sh" | "bash" | "zsh" | "dash" | "ksh")
            && matches!(argv[1].as_str(), "-c" | "-lc" | "-ic")
        {
            return true;
        }
    }
    command.contains('|')
        || command.contains("&&")
        || command.contains("||")
        || command.contains(';')
        || command.contains("$(")
        || command.contains('`')
}

pub fn is_safe_readonly_command(command: &str) -> bool {
    let trimmed = command.trim();
    let lower = trimmed.to_ascii_lowercase();
    if lower.starts_with("git status") || lower.starts_with("git diff") {
        return true;
    }
    if lower.starts_with("rg ")
        || lower.starts_with("grep ")
        || lower.starts_with("cargo check")
        || lower.starts_with("cargo test --no-run")
        || lower.starts_with("cargo fmt --check")
        || lower.starts_with("cargo clippy")
    {
        return true;
    }
    matches!(
        lower.as_str(),
        "git status" | "git diff" | "pwd" | "ls" | "cargo check"
    )
}

fn contains_privilege_escalation(command: &str) -> bool {
    let lower = command.to_ascii_lowercase();
    lower
        .split_whitespace()
        .any(|token| token == "sudo" || token == "doas")
}

fn contains_destructive_git(command: &str) -> bool {
    let lower = command.to_ascii_lowercase();
    lower.contains("git push") && (lower.contains("--force") || lower.contains("-f"))
        || lower.contains("git reset --hard")
        || lower.contains("git clean -f")
}

fn path_escape_in_target(effect: &NormalizedEffect) -> bool {
    let Some(target) = effect.action.target.as_deref() else {
        return false;
    };
    target.contains("/../")
        || target.contains("\\..\\")
        || target.starts_with("../")
        || target.starts_with("..\\")
}

pub fn default_risk_gate() -> Arc<dyn RiskGate> {
    Arc::new(DeterministicRiskGate)
}

pub fn is_mutating_effect(effect: &NormalizedEffect) -> bool {
    match effect.capability {
        EffectCapability::WorkspaceWrite | EffectCapability::ProcessSpawn => true,
        EffectCapability::NetworkConnect => !matches!(
            effect.action.kind,
            ActionKind::WebSearch | ActionKind::WebFetch
        ),
        EffectCapability::WorkspaceRead => false,
    }
}

pub fn is_read_only_effect(effect: &NormalizedEffect) -> bool {
    matches!(
        effect.action.kind,
        ActionKind::ReadFile | ActionKind::WebSearch | ActionKind::WebFetch
    ) || (effect.action.kind == ActionKind::SpawnProcess
        && is_safe_readonly_command(&command_text(None, &effect.action)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ActionOrigin, CapabilityVersion};

    fn ctx<'a>(
        mode: ExecutionMode,
        effect: &'a NormalizedEffect,
        argv: Option<&'a [String]>,
    ) -> RiskContext<'a> {
        RiskContext { mode, effect, argv }
    }

    fn read_effect() -> NormalizedEffect {
        NormalizedEffect::workspace_read(ActionOrigin::Agent, "read note", "note.txt")
    }

    fn write_effect() -> NormalizedEffect {
        NormalizedEffect::workspace_write(ActionOrigin::Agent, "write note", "note.txt")
    }

    fn spawn_effect(summary: &str, target: &str) -> NormalizedEffect {
        NormalizedEffect::process_spawn(ActionOrigin::Agent, summary, target)
    }

    #[test]
    fn opaque_shell_detects_sh_c_pipes_and_subshell() {
        assert!(is_opaque_shell(
            Some(&["sh".into(), "-c".into(), "echo hi".into()]),
            "ignored"
        ));
        assert!(is_opaque_shell(None, "cargo fmt && cargo test"));
        assert!(is_opaque_shell(None, "echo $(whoami)"));
        assert!(is_opaque_shell(None, "a | b"));
        assert!(!is_opaque_shell(None, "git status"));
    }

    #[test]
    fn benign_names_containing_rm_substring_are_not_denied() {
        let gate = DeterministicRiskGate;
        let effect = spawn_effect("run transform", "cargo run --package rm-helper");
        assert!(!matches!(
            gate.classify(&ctx(ExecutionMode::Auto, &effect, None)),
            RiskGateDecision::Deny { ref reason } if reason.contains("rm")
        ));
    }

    #[test]
    fn sudo_and_force_push_fail_closed_even_in_bypass() {
        let gate = DeterministicRiskGate;
        let sudo = spawn_effect("sudo apt", "sudo apt update");
        assert!(matches!(
            gate.classify(&ctx(ExecutionMode::Bypass, &sudo, None)),
            RiskGateDecision::Deny { .. }
        ));
        let force = spawn_effect("force push", "git push --force origin main");
        assert!(matches!(
            gate.classify(&ctx(ExecutionMode::Bypass, &force, None)),
            RiskGateDecision::Deny { .. }
        ));
    }

    #[test]
    fn path_traversal_in_write_target_is_denied() {
        let gate = DeterministicRiskGate;
        let effect = NormalizedEffect::workspace_write(
            ActionOrigin::Agent,
            "escape",
            "../outside/secret.txt",
        );
        assert!(matches!(
            gate.classify(&ctx(ExecutionMode::Bypass, &effect, None)),
            RiskGateDecision::Deny { .. }
        ));
    }

    #[test]
    fn auto_allows_safe_read_and_scoped_write() {
        let gate = DeterministicRiskGate;
        assert_eq!(
            gate.classify(&ctx(ExecutionMode::Auto, &read_effect(), None)),
            RiskGateDecision::Allow
        );
        assert_eq!(
            gate.classify(&ctx(ExecutionMode::Auto, &write_effect(), None)),
            RiskGateDecision::Allow
        );
        let git_status = spawn_effect("git status", "git status");
        assert_eq!(
            gate.classify(&ctx(ExecutionMode::Auto, &git_status, None)),
            RiskGateDecision::Allow
        );
    }

    #[test]
    fn ask_keeps_write_on_needs_human_approval() {
        let gate = DeterministicRiskGate;
        assert!(matches!(
            gate.classify(&ctx(ExecutionMode::Ask, &write_effect(), None)),
            RiskGateDecision::NeedsHumanApproval { .. }
        ));
    }

    #[test]
    fn plan_classifier_denies_mutations() {
        let gate = DeterministicRiskGate;
        assert!(matches!(
            gate.classify(&ctx(ExecutionMode::Plan, &write_effect(), None)),
            RiskGateDecision::Deny { .. }
        ));
        let spawn = spawn_effect("run", "cargo build");
        assert!(matches!(
            gate.classify(&ctx(ExecutionMode::Plan, &spawn, None)),
            RiskGateDecision::Deny { .. }
        ));
    }

    #[test]
    fn user_origin_still_denies_sudo() {
        let gate = DeterministicRiskGate;
        let mut effect = spawn_effect("sudo apt", "sudo apt update");
        effect.action.origin = ActionOrigin::User;
        effect.origin = ActionOrigin::User;
        assert!(matches!(
            gate.classify(&ctx(ExecutionMode::Ask, &effect, None)),
            RiskGateDecision::Deny { .. }
        ));
    }

    #[test]
    fn sh_lc_argv_triggers_needs_human_approval() {
        let gate = DeterministicRiskGate;
        let effect = spawn_effect("shell", ".");
        let argv = vec!["/bin/sh".into(), "-lc".into(), "echo hi".into()];
        assert!(matches!(
            gate.classify(&ctx(ExecutionMode::Auto, &effect, Some(&argv))),
            RiskGateDecision::NeedsHumanApproval { .. }
        ));
    }

    #[test]
    fn normalized_effect_roundtrip_preserves_version() {
        let effect = read_effect();
        assert_eq!(effect.version, CapabilityVersion::V1);
    }
}
