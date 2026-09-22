//! Mockable LLM prompt rewrite seam for Steer (#285).
//!
//! When Steer is accepted (active run required by [`crate::UserIntentRouter`]),
//! harness calls [`SteerRewrite`] with active-run context + steer text. Default
//! [`PassthroughSteerRewrite`] is deterministic and offline. When a
//! [`ModelProvider`] is wired, [`ProviderSteerRewrite`] performs a one-shot
//! rewrite (with passthrough fallback on provider failure).
//!
//! Rewritten fragments queue on [`SteerPendingQueue`] and drain into the active
//! [`crate::AgentLoop`] between turns. Durable Intent events keep the original
//! user steer text (origin / Policy unchanged).

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use thiserror::Error;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::provider_trait::{ModelProvider, StreamEvent};
use crate::{ProviderError, ProviderMessage};

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

/// In-memory queue of rewritten steer fragments keyed by session id.
///
/// Harness pushes after accept; active [`crate::AgentLoop`] drains between turns.
#[derive(Clone, Default)]
pub struct SteerPendingQueue {
    pending: Arc<Mutex<HashMap<Uuid, VecDeque<String>>>>,
}

impl SteerPendingQueue {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&self, session_id: Uuid, fragment: impl Into<String>) {
        let fragment = fragment.into();
        if fragment.is_empty() {
            return;
        }
        let mut guard = self
            .pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        guard.entry(session_id).or_default().push_back(fragment);
    }

    pub fn drain(&self, session_id: Uuid) -> Vec<String> {
        let mut guard = self
            .pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        guard.remove(&session_id).map(Vec::from).unwrap_or_default()
    }

    pub fn pending_count(&self, session_id: Uuid) -> usize {
        self.pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(&session_id)
            .map(|queue| queue.len())
            .unwrap_or(0)
    }
}

/// Live one-shot rewrite via [`ModelProvider`]; falls back to passthrough offline.
#[derive(Clone)]
pub struct ProviderSteerRewrite {
    provider: Arc<dyn ModelProvider>,
    fallback: PassthroughSteerRewrite,
}

impl ProviderSteerRewrite {
    pub fn new(provider: Arc<dyn ModelProvider>) -> Self {
        Self {
            provider,
            fallback: PassthroughSteerRewrite::new(),
        }
    }

    fn build_rewrite_messages(
        context: &SteerActiveContext,
        steer_text: &str,
    ) -> Vec<ProviderMessage> {
        let system = ProviderMessage::system(
            "Rewrite the user's steer instruction into a concise nudge for the active coding agent. \
             Output only the nudge text with no preamble.",
        );
        let user = match context.active_prompt.as_deref() {
            Some(prompt) if !prompt.is_empty() => ProviderMessage::user(format!(
                "Active task:\n{prompt}\n\nSteer instruction:\n{steer_text}"
            )),
            _ => ProviderMessage::user(format!("Steer instruction:\n{steer_text}")),
        };
        vec![system, user]
    }

    fn collect_provider_fragment(
        &self,
        context: &SteerActiveContext,
        steer_text: &str,
    ) -> Result<String, ProviderError> {
        let messages = Self::build_rewrite_messages(context, steer_text);
        let cancel = CancellationToken::new();
        let provider = self.provider.clone();
        block_on_steer(async move {
            let fragment = Arc::new(Mutex::new(String::new()));
            let capture = fragment.clone();
            provider
                .stream_messages(
                    &messages,
                    None,
                    None,
                    cancel,
                    crate::StreamOptions::default(),
                    Box::new(move |event| {
                        if let StreamEvent::TextDelta { delta } = event
                            && let Ok(mut out) = capture.lock()
                        {
                            out.push_str(&delta);
                        }
                        Ok(())
                    }),
                )
                .await?;
            Ok(fragment
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .trim()
                .to_string())
        })
    }
}

impl std::fmt::Debug for ProviderSteerRewrite {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProviderSteerRewrite")
            .field("provider_id", &self.provider.provider_id())
            .field("model_id", &self.provider.model_id())
            .finish()
    }
}

impl SteerRewrite for ProviderSteerRewrite {
    fn rewrite(
        &self,
        context: &SteerActiveContext,
        steer_text: &str,
    ) -> Result<SteerRewriteOutput, SteerRewriteError> {
        match self.collect_provider_fragment(context, steer_text) {
            Ok(fragment) if !fragment.is_empty() => Ok(SteerRewriteOutput::from_fragment(fragment)),
            _ => self.fallback.rewrite(context, steer_text),
        }
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

/// Shared default rewriter for harness construction (offline passthrough).
pub fn default_steer_rewrite() -> Arc<dyn SteerRewrite> {
    Arc::new(PassthroughSteerRewrite::new())
}

/// Build provider-backed rewriter when a live model is available.
pub fn provider_steer_rewrite(provider: Arc<dyn ModelProvider>) -> Arc<dyn SteerRewrite> {
    Arc::new(ProviderSteerRewrite::new(provider))
}

/// ponytail: sync trait forces block_on. Ceiling — nested runtime / worker thread.
fn block_on_steer<F, T>(fut: F) -> T
where
    F: std::future::Future<Output = T>,
{
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => tokio::task::block_in_place(|| handle.block_on(fut)),
        Err(_) => tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("steer rewrite runtime")
            .block_on(fut),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mock_provider::{MockProvider, MockStreamItem};
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

    #[test]
    fn pending_queue_drains_in_order() {
        let queue = SteerPendingQueue::new();
        let session = Uuid::new_v4();
        queue.push(session, "first");
        queue.push(session, "second");
        assert_eq!(queue.pending_count(session), 2);
        assert_eq!(queue.drain(session), vec!["first", "second"]);
        assert_eq!(queue.pending_count(session), 0);
    }

    #[test]
    fn provider_rewrite_uses_mock_provider_offline() {
        let provider = Arc::new(MockProvider::scripted(
            "steer-mock",
            "test-model",
            [vec![MockStreamItem::Chunk {
                chunk_id: 1,
                text: "Focus on unit tests.".into(),
            }]],
        ));
        let rewriter = ProviderSteerRewrite::new(provider);
        let out = rewriter
            .rewrite(&ctx(Some("implement feature")), "prefer tests")
            .expect("rewrite");
        assert_eq!(out.fragment, "Focus on unit tests.");
    }

    #[test]
    fn provider_rewrite_falls_back_when_stream_empty() {
        let provider = Arc::new(MockProvider::new("steer-mock", "test-model", []));
        let rewriter = ProviderSteerRewrite::new(provider);
        let out = rewriter
            .rewrite(&ctx(None), "go faster")
            .expect("fallback rewrite");
        assert_eq!(out.fragment, "[steer] go faster");
    }
}
