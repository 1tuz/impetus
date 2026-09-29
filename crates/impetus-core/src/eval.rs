//! Offline harness eval + immutable [`ExperimentCapsule`] (#408 / parent #397).
//!
//! Measures fixture pass/fail against mock providers — no live keys, no network.
//! Role separation: proposer / executor / analyzer / promotion. Promotion is
//! **never** automatic: explicit opt-in still only stages a human request.
//!
//! Expensive full eval suites stay off PR Fast; this module is unit-cheap.

use crate::mock_provider::{MockProvider, MockStreamItem};
use crate::{FinishReason, ModelProvider, ProviderMessage, StreamEvent};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::sync::{Arc, Mutex};
use thiserror::Error;
use tokio_util::sync::CancellationToken;

/// Eval pipeline roles. A proposer must not execute, analyze, or promote
/// in the same experiment; promotion never self-applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvalRole {
    Proposer,
    Executor,
    Analyzer,
    Promotion,
}

impl EvalRole {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Proposer => "proposer",
            Self::Executor => "executor",
            Self::Analyzer => "analyzer",
            Self::Promotion => "promotion",
        }
    }
}

/// Immutable config for one offline eval run. Digest covers fixture ids,
/// harness revision, and policy digest **labels** only — never secrets.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExperimentCapsule {
    /// Short stable id derived from [`Self::digest`] (not a secret).
    pub capsule_id: String,
    pub fixture_ids: Vec<String>,
    /// Git SHA, `GITHUB_SHA`, or `impetus-core@<crate version>`.
    pub harness_revision: String,
    /// Policy identity labels (version/path tokens) — never tokens/keys.
    pub policy_digest_labels: Vec<String>,
    /// `sha256:` + hex over canonical capsule fields.
    pub digest: String,
}

impl ExperimentCapsule {
    /// Build an immutable capsule and compute its digest.
    pub fn new(
        mut fixture_ids: Vec<String>,
        harness_revision: impl Into<String>,
        mut policy_digest_labels: Vec<String>,
    ) -> Self {
        fixture_ids.sort();
        fixture_ids.dedup();
        policy_digest_labels.sort();
        policy_digest_labels.dedup();
        let harness_revision = harness_revision.into();
        let digest = compute_capsule_digest(&fixture_ids, &harness_revision, &policy_digest_labels);
        let capsule_id = format!(
            "eval:{}",
            &digest["sha256:".len()..].get(..12).unwrap_or("")
        );
        Self {
            capsule_id,
            fixture_ids,
            harness_revision,
            policy_digest_labels,
            digest,
        }
    }
}

fn compute_capsule_digest(
    fixture_ids: &[String],
    harness_revision: &str,
    policy_digest_labels: &[String],
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"impetus.experiment_capsule.v1\n");
    hasher.update(b"fixtures:");
    hasher.update(fixture_ids.join(",").as_bytes());
    hasher.update(b"\nharness:");
    hasher.update(harness_revision.as_bytes());
    hasher.update(b"\npolicy:");
    hasher.update(policy_digest_labels.join(",").as_bytes());
    hasher.update(b"\n");
    let hex: String = hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    format!("sha256:{hex}")
}

/// Default harness revision label for offline eval (no git required).
pub fn default_harness_revision() -> String {
    std::env::var("IMPETUS_EVAL_HARNESS_REVISION")
        .or_else(|_| std::env::var("GITHUB_SHA"))
        .unwrap_or_else(|_| format!("impetus-core@{}", env!("CARGO_PKG_VERSION")))
}

/// Default policy digest labels (version tokens only).
pub fn default_policy_digest_labels() -> Vec<String> {
    vec![format!("policy_config:{}", crate::POLICY_CONFIG_VERSION)]
}

/// One offline mock fixture (no credentials).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvalFixture {
    pub id: String,
    /// User prompt label (audit only).
    pub prompt: String,
    /// Expected concatenated assistant text from the mock stream.
    pub expected_text: String,
    /// When false, fixture expects the mock to fail (error / mismatch).
    pub expect_pass: bool,
}

/// Built-in offline set — runs without live keys.
pub fn builtin_offline_fixtures() -> Vec<EvalFixture> {
    vec![
        EvalFixture {
            id: "mock_hello_pass".into(),
            prompt: "say hello".into(),
            expected_text: "hello from mock".into(),
            expect_pass: true,
        },
        EvalFixture {
            id: "mock_mismatch_fail".into(),
            prompt: "force mismatch".into(),
            expected_text: "this text will not match".into(),
            expect_pass: false,
        },
    ]
}

/// Per-fixture outcome bound to a capsule id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvalFixtureResult {
    pub fixture_id: String,
    pub passed: bool,
    pub detail: String,
    pub capsule_id: String,
}

/// Full offline eval report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvalRunReport {
    pub capsule: ExperimentCapsule,
    pub results: Vec<EvalFixtureResult>,
    pub all_passed: bool,
    /// Role that produced this report (executor).
    pub executed_by: EvalRole,
}

#[derive(Debug, Error)]
pub enum EvalError {
    #[error("provider error: {0}")]
    Provider(String),
    #[error("empty fixture set")]
    EmptyFixtures,
}

/// Run offline mock fixtures and record pass/fail + capsule id.
///
/// Uses [`MockProvider`] only — never live credentials.
pub async fn run_offline_eval(
    fixtures: &[EvalFixture],
    harness_revision: impl Into<String>,
    policy_digest_labels: Vec<String>,
) -> Result<EvalRunReport, EvalError> {
    if fixtures.is_empty() {
        return Err(EvalError::EmptyFixtures);
    }
    let fixture_ids: Vec<String> = fixtures.iter().map(|f| f.id.clone()).collect();
    let capsule = ExperimentCapsule::new(fixture_ids, harness_revision, policy_digest_labels);

    let mut results = Vec::with_capacity(fixtures.len());
    for fixture in fixtures {
        let result = run_one_mock_fixture(fixture, &capsule.capsule_id).await?;
        results.push(result);
    }

    let all_passed = results.iter().all(|r| r.passed);

    Ok(EvalRunReport {
        capsule,
        results,
        all_passed,
        executed_by: EvalRole::Executor,
    })
}

async fn run_one_mock_fixture(
    fixture: &EvalFixture,
    capsule_id: &str,
) -> Result<EvalFixtureResult, EvalError> {
    // Happy-path mock always emits expected_text for pass fixtures.
    // Fail fixtures intentionally emit different text so expect_pass=false → passed=true.
    let mock_text = if fixture.expect_pass {
        fixture.expected_text.clone()
    } else {
        format!("offline-mismatch:{}", fixture.id)
    };

    let provider = MockProvider::new(
        "eval-mock",
        "eval-mock-model",
        [
            MockStreamItem::Chunk {
                chunk_id: 1,
                text: mock_text.clone(),
            },
            MockStreamItem::Finish {
                reason: FinishReason::Stop,
            },
        ],
    );

    let collected = Arc::new(Mutex::new(String::new()));
    let sink = Arc::clone(&collected);
    provider
        .stream_messages(
            &[ProviderMessage::user(fixture.prompt.clone())],
            None,
            None,
            CancellationToken::new(),
            Default::default(),
            Box::new(move |event| {
                if let StreamEvent::TextDelta { delta } = event {
                    if let Ok(mut buf) = sink.lock() {
                        buf.push_str(&delta);
                    }
                }
                Ok(())
            }),
        )
        .await
        .map_err(|e| EvalError::Provider(e.to_string()))?;

    let got = collected.lock().map(|s| s.clone()).unwrap_or_default();
    let matches = got == fixture.expected_text;
    let passed = matches == fixture.expect_pass;
    let detail = if passed {
        if fixture.expect_pass {
            format!("mock text matched ({got})")
        } else {
            format!("expected mismatch observed (got {got})")
        }
    } else {
        format!(
            "unexpected outcome: got={got:?} expect_pass={}",
            fixture.expect_pass
        )
    };

    Ok(EvalFixtureResult {
        fixture_id: fixture.id.clone(),
        passed,
        detail,
        capsule_id: capsule_id.to_string(),
    })
}

/// Analyzer stub: reads a report only (no mutation / no promote).
pub fn analyze_report(report: &EvalRunReport) -> EvalAnalysis {
    EvalAnalysis {
        capsule_id: report.capsule.capsule_id.clone(),
        capsule_digest: report.capsule.digest.clone(),
        fixture_count: report.results.len(),
        pass_count: report.results.iter().filter(|r| r.passed).count(),
        all_passed: report.all_passed,
        role: EvalRole::Analyzer,
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvalAnalysis {
    pub capsule_id: String,
    pub capsule_digest: String,
    pub fixture_count: usize,
    pub pass_count: usize,
    pub all_passed: bool,
    pub role: EvalRole,
}

/// Explicit promotion request. Default is refuse.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromotionRequest {
    pub capsule_digest: String,
    /// CLI/API opt-in flag (default false).
    pub opt_in: bool,
    /// Separate human confirmation — required even when `opt_in`.
    pub human_confirmed: bool,
}

/// Promotion never auto-applies to production / main.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum PromotionOutcome {
    Refused {
        reason: String,
    },
    /// Staged for a human; never means applied.
    StagedForHuman {
        capsule_digest: String,
    },
}

/// Promotion gate: refuse unless opt-in **and** human confirmation.
/// Even then only stages — never auto-merges or mutates harness.
pub fn request_promotion(req: &PromotionRequest) -> PromotionOutcome {
    if !req.opt_in {
        return PromotionOutcome::Refused {
            reason: "promotion opt-in flag is off (default)".into(),
        };
    }
    if !req.human_confirmed {
        return PromotionOutcome::Refused {
            reason: "human confirmation required; promotion is never automatic".into(),
        };
    }
    PromotionOutcome::StagedForHuman {
        capsule_digest: req.capsule_digest.clone(),
    }
}

/// Proposer stub — may suggest a candidate label only; cannot promote.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvalProposal {
    pub summary: String,
    pub role: EvalRole,
}

pub fn propose_candidate(summary: impl Into<String>) -> EvalProposal {
    EvalProposal {
        summary: summary.into(),
        role: EvalRole::Proposer,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capsule_digest_stable_and_order_independent() {
        let a = ExperimentCapsule::new(
            vec!["b".into(), "a".into()],
            "rev1",
            vec!["pol:z".into(), "pol:a".into()],
        );
        let b = ExperimentCapsule::new(
            vec!["a".into(), "b".into()],
            "rev1",
            vec!["pol:a".into(), "pol:z".into()],
        );
        assert_eq!(a.digest, b.digest);
        assert_eq!(a.capsule_id, b.capsule_id);
        assert!(a.digest.starts_with("sha256:"));
        assert!(a.capsule_id.starts_with("eval:"));
    }

    #[test]
    fn capsule_digest_changes_with_revision() {
        let a = ExperimentCapsule::new(vec!["f1".into()], "rev-a", vec!["p:1".into()]);
        let b = ExperimentCapsule::new(vec!["f1".into()], "rev-b", vec!["p:1".into()]);
        assert_ne!(a.digest, b.digest);
    }

    #[tokio::test]
    async fn offline_builtin_fixtures_pass_with_mock_provider() {
        let fixtures = builtin_offline_fixtures();
        let report = run_offline_eval(&fixtures, "test-rev", vec!["policy_config:test".into()])
            .await
            .expect("offline eval");
        assert_eq!(report.executed_by, EvalRole::Executor);
        assert!(report.all_passed, "{:?}", report.results);
        assert_eq!(report.results.len(), 2);
        for r in &report.results {
            assert_eq!(r.capsule_id, report.capsule.capsule_id);
            assert!(r.passed);
        }
        let analysis = analyze_report(&report);
        assert_eq!(analysis.role, EvalRole::Analyzer);
        assert_eq!(analysis.pass_count, 2);
    }

    #[test]
    fn promotion_never_automatic() {
        let digest = "sha256:deadbeef".to_string();
        assert!(matches!(
            request_promotion(&PromotionRequest {
                capsule_digest: digest.clone(),
                opt_in: false,
                human_confirmed: false,
            }),
            PromotionOutcome::Refused { .. }
        ));
        assert!(matches!(
            request_promotion(&PromotionRequest {
                capsule_digest: digest.clone(),
                opt_in: true,
                human_confirmed: false,
            }),
            PromotionOutcome::Refused { .. }
        ));
        assert_eq!(
            request_promotion(&PromotionRequest {
                capsule_digest: digest.clone(),
                opt_in: true,
                human_confirmed: true,
            }),
            PromotionOutcome::StagedForHuman {
                capsule_digest: digest
            }
        );
    }

    #[test]
    fn proposer_cannot_look_like_promotion() {
        let p = propose_candidate("tweak prompt");
        assert_eq!(p.role, EvalRole::Proposer);
        assert_ne!(p.role, EvalRole::Promotion);
    }
}
