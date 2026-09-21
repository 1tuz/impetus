//! Cheap hook match/filter **before** expensive process spawn (TODO P1 §9).
//!
//! Harness-side prefilter: typed pattern + action + trust. First match wins; no
//! match means [`PrefilterDecision::AllowContinue`]. This is performance-first
//! — not a hook catalog, plugin ABI, or arbitrary script runner.
//!
//! **Trust:** each rule is [`HookTrustLevel::InDaemon`] (in-process) or
//! [`HookTrustLevel::External`] (would consult/spawn outside the daemon).
//! Security-critical rules (policy deny, sandbox-escape checks) **require**
//! `InDaemon`. An `External` matcher that hits a security-critical rule is
//! refused with a clear error — never treated as AllowContinue.
//!
//! Out of scope: full hook/plugin ABI, arbitrary script runner, perf suite,
//! wiring into live `ProcessExecution` (real OS spawn).

use thiserror::Error;

/// Where a hook rule is evaluated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookTrustLevel {
    /// In-process inside the daemon — allowed for security-critical rules.
    InDaemon,
    /// External process/script/matcher — forbidden for security-critical rules.
    External,
}

/// Action taken when a rule's pattern matches a command/tool label.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookAction {
    /// Match is informative only — continue toward spawn.
    AllowContinue,
    /// Skip the expensive spawn (cheap no-op path).
    SkipSpawn,
    /// Hard deny — do not spawn.
    Deny,
}

/// One typed match rule: exact label pattern + action + trust.
///
/// Patterns are exact string equality on the command/tool label (no regex, no
/// glob) — cheapest possible match for the stub.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookRule {
    pub pattern: String,
    pub action: HookAction,
    pub trust: HookTrustLevel,
    /// Policy deny / sandbox-escape style rules — External trust refused.
    pub security_critical: bool,
}

impl HookRule {
    /// Ordinary in-daemon rule (not security-critical).
    pub fn new(pattern: impl Into<String>, action: HookAction) -> Self {
        Self {
            pattern: pattern.into(),
            action,
            trust: HookTrustLevel::InDaemon,
            security_critical: false,
        }
    }

    /// Security-critical in-daemon rule (policy gate, dangerous-label deny).
    pub fn security_critical(pattern: impl Into<String>, action: HookAction) -> Self {
        Self {
            pattern: pattern.into(),
            action,
            trust: HookTrustLevel::InDaemon,
            security_critical: true,
        }
    }

    /// Ordinary external matcher (not security-critical).
    pub fn external(pattern: impl Into<String>, action: HookAction) -> Self {
        Self {
            pattern: pattern.into(),
            action,
            trust: HookTrustLevel::External,
            security_critical: false,
        }
    }

    /// Override trust (e.g. test External + security_critical refusal path).
    pub fn with_trust(mut self, trust: HookTrustLevel) -> Self {
        self.trust = trust;
        self
    }
}

/// Result of [`HookPrefilter::prefilter`] on success.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrefilterDecision {
    AllowContinue,
    SkipSpawn,
    Deny,
}

impl From<HookAction> for PrefilterDecision {
    fn from(action: HookAction) -> Self {
        match action {
            HookAction::AllowContinue => Self::AllowContinue,
            HookAction::SkipSpawn => Self::SkipSpawn,
            HookAction::Deny => Self::Deny,
        }
    }
}

/// Trust / policy failures from prefilter (distinct from action Deny).
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum PrefilterError {
    /// External matcher hit a security-critical rule — refused.
    #[error(
        "security-critical hook for label `{label}` requires InDaemon trust; External matcher refused"
    )]
    ExternalForCritical { label: String },
}

/// In-memory rule set for cheap pre-spawn filtering.
#[derive(Debug, Clone, Default)]
pub struct HookPrefilter {
    rules: Vec<HookRule>,
}

impl HookPrefilter {
    pub fn new(rules: Vec<HookRule>) -> Self {
        Self { rules }
    }

    pub fn rules(&self) -> &[HookRule] {
        &self.rules
    }

    /// Match `label` against rules in order. First hit wins; no hit → continue.
    ///
    /// Security-critical + [`HookTrustLevel::External`] →
    /// [`PrefilterError::ExternalForCritical`] (clear Deny/error path).
    pub fn prefilter(&self, label: &str) -> Result<PrefilterDecision, PrefilterError> {
        for rule in &self.rules {
            if rule.pattern == label {
                if rule.security_critical && rule.trust == HookTrustLevel::External {
                    return Err(PrefilterError::ExternalForCritical {
                        label: label.to_string(),
                    });
                }
                return Ok(PrefilterDecision::from(rule.action));
            }
        }
        Ok(PrefilterDecision::AllowContinue)
    }
}

/// Outcome of the spawn **stub** (never starts a real process).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpawnStubOutcome {
    /// Prefilter allowed continue — stub would spawn (not performed here).
    WouldSpawn,
    /// Prefilter skipped spawn.
    Skipped,
}

/// Failures from the spawn stub path.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum SpawnStubError {
    #[error("hook prefilter denied spawn for label {0}")]
    Denied(String),
    #[error(
        "security-critical hook for label `{label}` requires InDaemon trust; External matcher refused"
    )]
    ExternalForCritical { label: String },
}

impl From<PrefilterError> for SpawnStubError {
    fn from(err: PrefilterError) -> Self {
        match err {
            PrefilterError::ExternalForCritical { label } => Self::ExternalForCritical { label },
        }
    }
}

/// Call [`HookPrefilter::prefilter`] **before** any spawn stub work.
///
/// Never starts an OS process, touches the network, or handles secrets —
/// labels only.
pub fn spawn_stub(
    prefilter: &HookPrefilter,
    label: &str,
) -> Result<SpawnStubOutcome, SpawnStubError> {
    match prefilter.prefilter(label)? {
        PrefilterDecision::AllowContinue => Ok(SpawnStubOutcome::WouldSpawn),
        PrefilterDecision::SkipSpawn => Ok(SpawnStubOutcome::Skipped),
        PrefilterDecision::Deny => Err(SpawnStubError::Denied(label.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_prefilter() -> HookPrefilter {
        HookPrefilter::new(vec![
            HookRule::new("expensive-lint", HookAction::SkipSpawn),
            HookRule::security_critical("rm-rf-workspace", HookAction::Deny),
            HookRule::new("echo-ok", HookAction::AllowContinue),
            HookRule::external("notify-slack", HookAction::SkipSpawn),
        ])
    }

    #[test]
    fn match_skip_spawn() {
        let pf = sample_prefilter();
        assert_eq!(
            pf.prefilter("expensive-lint").unwrap(),
            PrefilterDecision::SkipSpawn
        );
        assert_eq!(
            spawn_stub(&pf, "expensive-lint").unwrap(),
            SpawnStubOutcome::Skipped
        );
    }

    #[test]
    fn match_deny() {
        let pf = sample_prefilter();
        assert_eq!(
            pf.prefilter("rm-rf-workspace").unwrap(),
            PrefilterDecision::Deny
        );
        assert_eq!(
            spawn_stub(&pf, "rm-rf-workspace").unwrap_err(),
            SpawnStubError::Denied("rm-rf-workspace".into())
        );
    }

    #[test]
    fn no_match_continues() {
        let pf = sample_prefilter();
        assert_eq!(
            pf.prefilter("unknown-tool").unwrap(),
            PrefilterDecision::AllowContinue
        );
        assert_eq!(
            spawn_stub(&pf, "unknown-tool").unwrap(),
            SpawnStubOutcome::WouldSpawn
        );
    }

    #[test]
    fn allow_continue_rule_still_continues() {
        let pf = sample_prefilter();
        assert_eq!(
            pf.prefilter("echo-ok").unwrap(),
            PrefilterDecision::AllowContinue
        );
        assert_eq!(
            spawn_stub(&pf, "echo-ok").unwrap(),
            SpawnStubOutcome::WouldSpawn
        );
    }

    #[test]
    fn first_match_wins() {
        let pf = HookPrefilter::new(vec![
            HookRule::new("tool", HookAction::SkipSpawn),
            HookRule::new("tool", HookAction::Deny),
        ]);
        assert_eq!(pf.prefilter("tool").unwrap(), PrefilterDecision::SkipSpawn);
    }

    #[test]
    fn empty_rules_always_continue() {
        let pf = HookPrefilter::new(vec![]);
        assert_eq!(
            pf.prefilter("anything").unwrap(),
            PrefilterDecision::AllowContinue
        );
        assert_eq!(
            spawn_stub(&pf, "anything").unwrap(),
            SpawnStubOutcome::WouldSpawn
        );
    }

    #[test]
    fn ordinary_external_matcher_allowed() {
        let pf = sample_prefilter();
        assert_eq!(
            pf.prefilter("notify-slack").unwrap(),
            PrefilterDecision::SkipSpawn
        );
        assert_eq!(
            spawn_stub(&pf, "notify-slack").unwrap(),
            SpawnStubOutcome::Skipped
        );
    }

    #[test]
    fn security_critical_in_daemon_deny_ok() {
        let pf = HookPrefilter::new(vec![HookRule::security_critical(
            "policy-gate",
            HookAction::Deny,
        )]);
        let rule = &pf.rules()[0];
        assert_eq!(rule.trust, HookTrustLevel::InDaemon);
        assert!(rule.security_critical);
        assert_eq!(
            pf.prefilter("policy-gate").unwrap(),
            PrefilterDecision::Deny
        );
    }

    #[test]
    fn external_matcher_for_critical_action_refused() {
        let pf = HookPrefilter::new(vec![
            HookRule::security_critical("sandbox-escape-check", HookAction::Deny)
                .with_trust(HookTrustLevel::External),
        ]);
        assert_eq!(
            pf.prefilter("sandbox-escape-check").unwrap_err(),
            PrefilterError::ExternalForCritical {
                label: "sandbox-escape-check".into(),
            }
        );
        assert_eq!(
            spawn_stub(&pf, "sandbox-escape-check").unwrap_err(),
            SpawnStubError::ExternalForCritical {
                label: "sandbox-escape-check".into(),
            }
        );
        // Clear Deny/error — must not look like AllowContinue / WouldSpawn.
        let msg = spawn_stub(&pf, "sandbox-escape-check")
            .unwrap_err()
            .to_string();
        assert!(msg.contains("InDaemon"));
        assert!(msg.contains("External"));
    }

    #[test]
    fn spawn_stub_never_starts_process() {
        // Labels only — no Command::new / network / secrets in this path.
        let pf = sample_prefilter();
        let _ = spawn_stub(&pf, "expensive-lint");
        let _ = spawn_stub(&pf, "rm-rf-workspace");
        let _ = spawn_stub(&pf, "unknown-tool");
        let _ = spawn_stub(
            &HookPrefilter::new(vec![
                HookRule::security_critical("x", HookAction::Deny)
                    .with_trust(HookTrustLevel::External),
            ]),
            "x",
        );
    }
}
