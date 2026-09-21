//! Cheap hook match/filter **before** expensive process spawn (TODO P1 §9).
//!
//! Harness-side prefilter: typed pattern + action. First match wins; no match
//! means [`PrefilterDecision::AllowContinue`]. This is performance-first —
//! not a hook catalog, plugin ABI, or arbitrary script runner.
//!
//! **Security note:** security-critical hooks should prefer in-daemon / trusted
//! runtime evaluation, not arbitrary external processes by default. This module
//! only does in-process string match; spawning a helper to decide would defeat
//! the point.
//!
//! Out of scope: full hook runtime, overlapping large catalogs, perf suite,
//! wiring into live `ProcessExecution` (real OS spawn).

use thiserror::Error;

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

/// One typed match rule: exact label pattern + action.
///
/// Patterns are exact string equality on the command/tool label (no regex, no
/// glob) — cheapest possible match for the stub.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookRule {
    pub pattern: String,
    pub action: HookAction,
}

impl HookRule {
    pub fn new(pattern: impl Into<String>, action: HookAction) -> Self {
        Self {
            pattern: pattern.into(),
            action,
        }
    }
}

/// Result of [`HookPrefilter::prefilter`].
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
    pub fn prefilter(&self, label: &str) -> PrefilterDecision {
        for rule in &self.rules {
            if rule.pattern == label {
                return PrefilterDecision::from(rule.action);
            }
        }
        PrefilterDecision::AllowContinue
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
}

/// Call [`HookPrefilter::prefilter`] **before** any spawn stub work.
///
/// Never starts an OS process, touches the network, or handles secrets —
/// labels only.
pub fn spawn_stub(
    prefilter: &HookPrefilter,
    label: &str,
) -> Result<SpawnStubOutcome, SpawnStubError> {
    match prefilter.prefilter(label) {
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
            HookRule::new("rm-rf-workspace", HookAction::Deny),
            HookRule::new("echo-ok", HookAction::AllowContinue),
        ])
    }

    #[test]
    fn match_skip_spawn() {
        let pf = sample_prefilter();
        assert_eq!(pf.prefilter("expensive-lint"), PrefilterDecision::SkipSpawn);
        assert_eq!(
            spawn_stub(&pf, "expensive-lint").unwrap(),
            SpawnStubOutcome::Skipped
        );
    }

    #[test]
    fn match_deny() {
        let pf = sample_prefilter();
        assert_eq!(pf.prefilter("rm-rf-workspace"), PrefilterDecision::Deny);
        assert_eq!(
            spawn_stub(&pf, "rm-rf-workspace").unwrap_err(),
            SpawnStubError::Denied("rm-rf-workspace".into())
        );
    }

    #[test]
    fn no_match_continues() {
        let pf = sample_prefilter();
        assert_eq!(
            pf.prefilter("unknown-tool"),
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
        assert_eq!(pf.prefilter("echo-ok"), PrefilterDecision::AllowContinue);
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
        assert_eq!(pf.prefilter("tool"), PrefilterDecision::SkipSpawn);
    }

    #[test]
    fn empty_rules_always_continue() {
        let pf = HookPrefilter::new(vec![]);
        assert_eq!(pf.prefilter("anything"), PrefilterDecision::AllowContinue);
        assert_eq!(
            spawn_stub(&pf, "anything").unwrap(),
            SpawnStubOutcome::WouldSpawn
        );
    }

    #[test]
    fn spawn_stub_never_starts_process() {
        // Labels only — no Command::new / network / secrets in this path.
        let pf = sample_prefilter();
        let _ = spawn_stub(&pf, "expensive-lint");
        let _ = spawn_stub(&pf, "rm-rf-workspace");
        let _ = spawn_stub(&pf, "unknown-tool");
    }
}
