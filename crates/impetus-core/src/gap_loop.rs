//! Bounded Gap Loop over [`CompletionGate`](crate::CompletionGate).
//!
//! When the gate returns `Insufficient`, attempt a limited number of honest
//! gap-fill corrections, re-check, then terminate fail-closed. Never maps
//! unknown / truncated / exhausted outcomes to `Completed`.
//!
//! `Rejected` is terminal (no retry). Gap-fill that cannot proceed returns
//! `false` and the loop exhausts immediately — no fabricated success.

use crate::CompletionVerdict;
use serde::{Deserialize, Serialize};

/// Hard bound on gap-fill attempts after `Insufficient`.
pub const GAP_LOOP_MAX_ITERATIONS: u32 = 3;

/// Result of driving CompletionGate through a bounded gap-fill loop.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum GapLoopOutcome {
    Accepted,
    Rejected {
        reason: String,
    },
    /// Bound hit (or no fill possible) while still Insufficient — fail-closed.
    Exhausted {
        missing: Vec<String>,
        iterations: u32,
    },
    /// Evaluator or gap-fill error — fail-closed, never Completed.
    Failed {
        reason: String,
    },
}

impl GapLoopOutcome {
    /// Reason string for `RunEvent::Failed`, if this outcome is not Accepted.
    pub fn fail_reason(&self) -> Option<String> {
        match self {
            Self::Accepted => None,
            Self::Rejected { reason } => Some(format!("completion gate rejected: {reason}")),
            Self::Exhausted {
                missing,
                iterations,
            } => Some(format!(
                "GapLoopExhausted after {iterations}: NeedsEvidence: missing {}",
                missing.join(", ")
            )),
            Self::Failed { reason } => Some(reason.clone()),
        }
    }

    pub fn is_accepted(&self) -> bool {
        matches!(self, Self::Accepted)
    }
}

/// Drive CompletionGate with a bounded gap-fill callback.
///
/// `evaluate` — fresh gate check against current durable evidence.
/// `attempt_gap_fill` — honest correction for `missing`; return `Ok(true)` if
/// an attempt was made that might change evidence, `Ok(false)` if no fill is
/// possible (loop exhausts immediately — still not Completed).
pub fn run_gap_loop<EvalErr, FillErr>(
    mut evaluate: impl FnMut() -> Result<CompletionVerdict, EvalErr>,
    mut attempt_gap_fill: impl FnMut(&[String]) -> Result<bool, FillErr>,
) -> GapLoopOutcome
where
    EvalErr: std::fmt::Display,
    FillErr: std::fmt::Display,
{
    let mut iterations = 0u32;
    loop {
        let verdict = match evaluate() {
            Ok(v) => v,
            Err(error) => {
                return GapLoopOutcome::Failed {
                    reason: format!("completion gate error: {error}"),
                };
            }
        };

        match verdict {
            CompletionVerdict::Accepted => return GapLoopOutcome::Accepted,
            CompletionVerdict::Rejected { reason } => {
                return GapLoopOutcome::Rejected { reason };
            }
            CompletionVerdict::Insufficient { missing } => {
                if iterations >= GAP_LOOP_MAX_ITERATIONS {
                    return GapLoopOutcome::Exhausted {
                        missing,
                        iterations,
                    };
                }
                iterations += 1;
                match attempt_gap_fill(&missing) {
                    Ok(true) => {
                        // Re-evaluate after an honest fill attempt.
                        continue;
                    }
                    Ok(false) => {
                        // Cannot fill cheaply — fail-closed, no fake Completed.
                        return GapLoopOutcome::Exhausted {
                            missing,
                            iterations,
                        };
                    }
                    Err(error) => {
                        return GapLoopOutcome::Failed {
                            reason: format!("gap fill error: {error}"),
                        };
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CompletionClaim, CompletionGate, Evidence};
    use std::cell::Cell;

    fn evidence(kind: &str, id: &str) -> Evidence {
        Evidence {
            kind: kind.into(),
            id: id.into(),
            summary: format!("{kind}:{id}"),
            artifact_or_path: None,
            created_sequence: Some(1),
        }
    }

    #[test]
    fn success_after_gap_when_evidence_appears() {
        let bag: Cell<Vec<Evidence>> = Cell::new(Vec::new());
        let fills = Cell::new(0u32);

        let outcome = run_gap_loop(
            || {
                let claim = CompletionClaim {
                    goal: "edit".into(),
                    side_effects_occurred: true,
                };
                let items = bag.take();
                let verdict = CompletionGate::evaluate(&claim, &items);
                bag.set(items);
                Ok::<_, &str>(verdict)
            },
            |_missing| {
                fills.set(fills.get() + 1);
                bag.set(vec![evidence("tool_observation", "call-1")]);
                Ok::<_, &str>(true)
            },
        );

        assert_eq!(outcome, GapLoopOutcome::Accepted);
        assert_eq!(fills.get(), 1);
        assert!(outcome.is_accepted());
        assert!(outcome.fail_reason().is_none());
    }

    #[test]
    fn terminate_on_bound_when_fill_never_helps() {
        let fills = Cell::new(0u32);

        let outcome = run_gap_loop(
            || {
                Ok::<_, &str>(CompletionVerdict::Insufficient {
                    missing: vec!["tool_observation".into()],
                })
            },
            |_missing| {
                fills.set(fills.get() + 1);
                Ok::<_, &str>(true)
            },
        );

        assert_eq!(fills.get(), GAP_LOOP_MAX_ITERATIONS);
        assert_eq!(
            outcome,
            GapLoopOutcome::Exhausted {
                missing: vec!["tool_observation".into()],
                iterations: GAP_LOOP_MAX_ITERATIONS,
            }
        );
        assert!(!outcome.is_accepted());
    }

    #[test]
    fn never_completes_on_exhaust_when_no_fill_possible() {
        let outcome = run_gap_loop(
            || {
                Ok::<_, &str>(CompletionVerdict::Insufficient {
                    missing: vec!["tool_observation".into()],
                })
            },
            |_missing| Ok::<_, &str>(false),
        );

        match &outcome {
            GapLoopOutcome::Exhausted {
                missing,
                iterations,
            } => {
                assert_eq!(missing, &vec!["tool_observation".to_string()]);
                assert_eq!(*iterations, 1);
            }
            other => panic!("expected Exhausted, got {other:?}"),
        }
        assert!(!matches!(outcome, GapLoopOutcome::Accepted));
        let reason = outcome.fail_reason().expect("fail reason");
        assert!(reason.contains("GapLoopExhausted"));
        assert!(reason.contains("NeedsEvidence"));
    }

    #[test]
    fn rejected_is_terminal_without_gap_fill() {
        let fills = Cell::new(0u32);
        let outcome = run_gap_loop(
            || {
                Ok::<_, &str>(CompletionVerdict::Rejected {
                    reason: "no successful tool observation".into(),
                })
            },
            |_missing| {
                fills.set(fills.get() + 1);
                Ok::<_, &str>(true)
            },
        );
        assert_eq!(fills.get(), 0);
        assert!(matches!(outcome, GapLoopOutcome::Rejected { .. }));
        assert!(!outcome.is_accepted());
    }

    #[test]
    fn accepted_on_first_evaluate_skips_gap_fill() {
        let fills = Cell::new(0u32);
        let outcome = run_gap_loop(
            || Ok::<_, &str>(CompletionVerdict::Accepted),
            |_missing| {
                fills.set(fills.get() + 1);
                Ok::<_, &str>(true)
            },
        );
        assert_eq!(outcome, GapLoopOutcome::Accepted);
        assert_eq!(fills.get(), 0);
    }

    #[test]
    fn evaluate_error_fail_closed_never_completed() {
        let outcome = run_gap_loop(|| Err("store unavailable"), |_missing| Ok::<_, &str>(true));
        assert!(matches!(outcome, GapLoopOutcome::Failed { .. }));
        assert!(!outcome.is_accepted());
        let reason = outcome.fail_reason().expect("fail");
        assert!(reason.contains("completion gate error"));
    }
}
