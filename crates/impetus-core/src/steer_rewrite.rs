//! Mockable LLM prompt rewrite seam for Steer (#285).
//!
//! When Steer is accepted (active run required by [`crate::UserIntentRouter`]),
//! harness calls [`SteerRewrite`] with active-run context + steer text. Default
//! [`PassthroughSteerRewrite`] is deterministic and offline — **no live provider
//! wire** yet (out of scope). Origin / Policy stay unchanged.
//!
//! Tests inject [`MockSteerRewrite`]. Live model rewrite remains deferred.

use std::sync::{Arc, Mutex};

use thiserror::Error;
use uuid::Uuid;

use crate::provider::ProviderMessage;

/// Active-run context supplied to a rewriter (labels/text only; no secrets).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SteerActiveContext {
    pub session_id: Uuid,
    pub active_run_id: Uuid,
    /// Latest durable user/prompt intent text for the session, if any.
    pub active_prompt: Option<String>,
}

/// Rewritten nudge for the active run (fragment and/or provider messages).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SteerRewriteOutput {
    /// Prompt fragment to nudge the active run.
    pub fragment: String,
    /// Optional message list for a future provider wire.
    pub messages: Vec<ProviderMessage>,
}

impl SteerRewriteOutput {
    pub fn from_fragment(fragment: impl Into<String>) -> Self {
        let fragment = fragment.into();
        Self {
            messages: vec![ProviderMessage::user(fragment.clone())],
            fragment,
        }
    }
}

/// Errors from a [`SteerRewrite`] implementation (never secrets).
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum SteerRewriteError {
    #[error("steer rewrite refused: {0}")]
    Refused(String),
}

/// Replaceable Steer prompt rewriter: (active context + steer text) → fragment/messages.
pub trait SteerRewrite: Send + Sync {
    fn rewrite(
        &self,
        context: &SteerActiveContext,
        steer_text: &str,
    ) -> Result<SteerRewriteOutput, SteerRewriteError>;
}

/// Default offline rewriter — deterministic nudge fragment, no network.
///
/// ponytail: live LLM rewrite deferred; this seam proves the harness hook.
#[derive(Debug, Default, Clone, Copy)]
pub struct PassthroughSteerRewrite;

impl PassthroughSteerRewrite {
    pub fn new() -> Self {
        Self
    }
}

impl SteerRewrite for PassthroughSteerRewrite {
    fn rewrite(
        &self,
        context: &SteerActiveContext,
        steer_text: &str,
    ) -> Result<SteerRewriteOutput, SteerRewriteError> {
        let fragment = match context.active_prompt.as_deref() {
            Some(prompt) if !prompt.is_empty() => {
                format!("{prompt}\n\n[steer] {steer_text}")
            }
            _ => format!("[steer] {steer_text}"),
        };
        Ok(SteerRewriteOutput::from_fragment(fragment))
    }
}

/// Test double: fixed fragment and recorded calls (no network/secrets).
#[derive(Debug, Default)]
pub struct MockSteerRewrite {
    fixed_fragment: Option<String>,
    refuse: Option<String>,
    calls: Mutex<Vec<(SteerActiveContext, String)>>,
}

impl MockSteerRewrite {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_fixed_fragment(fragment: impl Into<String>) -> Self {
        Self {
            fixed_fragment: Some(fragment.into()),
            ..Self::default()
        }
    }

    pub fn refusing(reason: impl Into<String>) -> Self {
        Self {
            refuse: Some(reason.into()),
            ..Self::default()
        }
    }

    pub fn call_count(&self) -> usize {
        self.calls
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .len()
    }

    pub fn calls(&self) -> Vec<(SteerActiveContext, String)> {
        self.calls
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }
}

impl SteerRewrite for MockSteerRewrite {
    fn rewrite(
        &self,
        context: &SteerActiveContext,
        steer_text: &str,
    ) -> Result<SteerRewriteOutput, SteerRewriteError> {
        self.calls
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push((context.clone(), steer_text.to_string()));
        if let Some(reason) = &self.refuse {
            return Err(SteerRewriteError::Refused(reason.clone()));
        }
        let fragment = self
            .fixed_fragment
            .clone()
            .unwrap_or_else(|| format!("[mock-steer] {steer_text}"));
        Ok(SteerRewriteOutput::from_fragment(fragment))
    }
}

/// Shared default rewriter for harness construction.
pub fn default_steer_rewrite() -> Arc<dyn SteerRewrite> {
    Arc::new(PassthroughSteerRewrite::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(prompt: Option<&str>) -> SteerActiveContext {
        SteerActiveContext {
            session_id: Uuid::new_v4(),
            active_run_id: Uuid::new_v4(),
            active_prompt: prompt.map(str::to_string),
        }
    }

    #[test]
    fn passthrough_prefixes_steer_without_active_prompt() {
        let out = PassthroughSteerRewrite
            .rewrite(&ctx(None), "prefer tests")
            .expect("rewrite");
        assert_eq!(out.fragment, "[steer] prefer tests");
        assert_eq!(out.messages.len(), 1);
        assert_eq!(out.messages[0].role(), "user");
        assert_eq!(out.messages[0].content(), "[steer] prefer tests");
    }

    #[test]
    fn passthrough_appends_steer_to_active_prompt() {
        let out = PassthroughSteerRewrite
            .rewrite(&ctx(Some("do work")), "prefer tests")
            .expect("rewrite");
        assert_eq!(out.fragment, "do work\n\n[steer] prefer tests");
    }

    #[test]
    fn mock_records_calls_and_returns_fixed_fragment() {
        let mock = MockSteerRewrite::with_fixed_fragment("nudge-fragment");
        let context = ctx(Some("baseline"));
        let out = mock.rewrite(&context, "go left").expect("rewrite");
        assert_eq!(out.fragment, "nudge-fragment");
        assert_eq!(mock.call_count(), 1);
        let calls = mock.calls();
        assert_eq!(calls[0].0.active_run_id, context.active_run_id);
        assert_eq!(calls[0].1, "go left");
    }

    #[test]
    fn mock_can_refuse_without_network() {
        let mock = MockSteerRewrite::refusing("test refuse");
        let err = mock.rewrite(&ctx(None), "x").unwrap_err();
        assert_eq!(err, SteerRewriteError::Refused("test refuse".into()));
        assert_eq!(mock.call_count(), 1);
    }

    #[test]
    fn rewrite_does_not_touch_origin_or_policy_types() {
        // Compile-time seam check: API takes only context + text — no ActionOrigin.
        let _: fn(&dyn SteerRewrite, &SteerActiveContext, &str) =
            |r, c, t| r.rewrite(c, t).map(|_| ()).unwrap_or(());
    }
}
