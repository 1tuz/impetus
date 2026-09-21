//! Typed user prompt intents: Prompt | Steer | FollowUp (TODO P1 §8).
//!
//! # Semantics
//!
//! - [`UserPromptIntent::Prompt`] — normal baseline user message for a session.
//! - [`UserPromptIntent::Steer`] — nudge the *active* run; rejected when no
//!   active run exists for that session.
//! - [`UserPromptIntent::FollowUp`] — enqueue text to run after the current
//!   turn completes; accepted when the session exists (even with no active run).
//!
//! # Policy / origin
//!
//! Neither Steer nor FollowUp changes [`ActionOrigin`] or bypasses Policy.
//! Clients still carry `origin=user|agent` separately; the model cannot
//! self-grant `origin=user` or approval. This module only validates routing
//! state (session / active run / queue) — Policy evaluation stays upstream.
//!
//! Out of scope for this slice: full TUI/IPC wiring, LLM prompt rewriting,
//! WorkflowEngine cancel/replace races (session_id may be shared later).

use std::collections::{HashMap, VecDeque};

use thiserror::Error;
use uuid::Uuid;

use crate::policy::ActionOrigin;

/// Distinct client intents for user-authored text (not collapsed into Prompt).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UserPromptIntent {
    /// Normal baseline user message.
    Prompt,
    /// Nudge / steer an active run.
    Steer,
    /// Enqueue after the current run finishes.
    FollowUp,
}

/// One typed submission. `origin` is preserved for Policy — never rewritten here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserIntentSubmission {
    pub session_id: Uuid,
    pub intent: UserPromptIntent,
    pub text: String,
    pub origin: ActionOrigin,
}

/// Successful accept of a typed intent (in-memory stub outcome).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserIntentAccepted {
    pub session_id: Uuid,
    pub intent: UserPromptIntent,
    pub text: String,
    /// Origin as submitted — unchanged by Steer/FollowUp.
    pub origin: ActionOrigin,
    pub active_run_id: Option<Uuid>,
    /// Position in the follow-up queue (0-based) when intent is FollowUp.
    pub follow_up_position: Option<usize>,
}

/// Queued follow-up entry (labels/text only; no secrets).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueuedFollowUp {
    pub text: String,
    pub origin: ActionOrigin,
}

/// Validation / routing errors for the in-memory stub.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum UserIntentError {
    #[error("unknown session {0}")]
    UnknownSession(Uuid),
    #[error("steer rejected: session {0} has no active run")]
    NoActiveRun(Uuid),
}

#[derive(Debug, Clone, Default)]
struct SessionIntentState {
    active_run_id: Option<Uuid>,
    follow_ups: VecDeque<QueuedFollowUp>,
}

/// Boring in-memory router for typed Prompt / Steer / FollowUp.
///
/// ponytail: single-process HashMap only; durable IPC / WorkflowEngine coupling
/// stays for a later slice.
#[derive(Debug, Default, Clone)]
pub struct UserIntentRouter {
    sessions: HashMap<Uuid, SessionIntentState>,
}

impl UserIntentRouter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Open (or re-open) a session slot. Idempotent.
    pub fn open_session(&mut self, session_id: Uuid) {
        self.sessions.entry(session_id).or_default();
    }

    pub fn has_session(&self, session_id: Uuid) -> bool {
        self.sessions.contains_key(&session_id)
    }

    /// Mark or clear the active run for a known session.
    pub fn set_active_run(
        &mut self,
        session_id: Uuid,
        run_id: Option<Uuid>,
    ) -> Result<(), UserIntentError> {
        let state = self
            .sessions
            .get_mut(&session_id)
            .ok_or(UserIntentError::UnknownSession(session_id))?;
        state.active_run_id = run_id;
        Ok(())
    }

    pub fn active_run_id(&self, session_id: Uuid) -> Result<Option<Uuid>, UserIntentError> {
        Ok(self
            .sessions
            .get(&session_id)
            .ok_or(UserIntentError::UnknownSession(session_id))?
            .active_run_id)
    }

    pub fn follow_up_len(&self, session_id: Uuid) -> Result<usize, UserIntentError> {
        Ok(self
            .sessions
            .get(&session_id)
            .ok_or(UserIntentError::UnknownSession(session_id))?
            .follow_ups
            .len())
    }

    pub fn follow_ups(
        &self,
        session_id: Uuid,
    ) -> Result<&VecDeque<QueuedFollowUp>, UserIntentError> {
        Ok(&self
            .sessions
            .get(&session_id)
            .ok_or(UserIntentError::UnknownSession(session_id))?
            .follow_ups)
    }

    /// Pop the next queued follow-up, if any.
    pub fn pop_follow_up(
        &mut self,
        session_id: Uuid,
    ) -> Result<Option<QueuedFollowUp>, UserIntentError> {
        Ok(self
            .sessions
            .get_mut(&session_id)
            .ok_or(UserIntentError::UnknownSession(session_id))?
            .follow_ups
            .pop_front())
    }

    /// Validate and apply a typed intent.
    ///
    /// - Prompt: accepted when session exists (baseline).
    /// - Steer: accepted only when an active run is set.
    /// - FollowUp: enqueued when session exists (active run optional).
    ///
    /// Origin is never rewritten. This stub does not call PolicyEngine.
    pub fn submit(
        &mut self,
        submission: UserIntentSubmission,
    ) -> Result<UserIntentAccepted, UserIntentError> {
        let state = self
            .sessions
            .get_mut(&submission.session_id)
            .ok_or(UserIntentError::UnknownSession(submission.session_id))?;

        match submission.intent {
            UserPromptIntent::Prompt => Ok(UserIntentAccepted {
                session_id: submission.session_id,
                intent: UserPromptIntent::Prompt,
                text: submission.text,
                origin: submission.origin,
                active_run_id: state.active_run_id,
                follow_up_position: None,
            }),
            UserPromptIntent::Steer => {
                let Some(run_id) = state.active_run_id else {
                    return Err(UserIntentError::NoActiveRun(submission.session_id));
                };
                Ok(UserIntentAccepted {
                    session_id: submission.session_id,
                    intent: UserPromptIntent::Steer,
                    text: submission.text,
                    origin: submission.origin,
                    active_run_id: Some(run_id),
                    follow_up_position: None,
                })
            }
            UserPromptIntent::FollowUp => {
                let position = state.follow_ups.len();
                state.follow_ups.push_back(QueuedFollowUp {
                    text: submission.text.clone(),
                    origin: submission.origin,
                });
                Ok(UserIntentAccepted {
                    session_id: submission.session_id,
                    intent: UserPromptIntent::FollowUp,
                    text: submission.text,
                    origin: submission.origin,
                    active_run_id: state.active_run_id,
                    follow_up_position: Some(position),
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session_router() -> (UserIntentRouter, Uuid) {
        let mut router = UserIntentRouter::new();
        let sid = Uuid::new_v4();
        router.open_session(sid);
        (router, sid)
    }

    #[test]
    fn prompt_baseline_ok_without_active_run() {
        let (mut router, sid) = session_router();
        let accepted = router
            .submit(UserIntentSubmission {
                session_id: sid,
                intent: UserPromptIntent::Prompt,
                text: "hello".into(),
                origin: ActionOrigin::User,
            })
            .expect("prompt");
        assert_eq!(accepted.intent, UserPromptIntent::Prompt);
        assert_eq!(accepted.active_run_id, None);
        assert_eq!(accepted.origin, ActionOrigin::User);
        assert!(accepted.follow_up_position.is_none());
    }

    #[test]
    fn steer_rejected_without_active_run() {
        let (mut router, sid) = session_router();
        let err = router
            .submit(UserIntentSubmission {
                session_id: sid,
                intent: UserPromptIntent::Steer,
                text: "nudge".into(),
                origin: ActionOrigin::User,
            })
            .unwrap_err();
        assert_eq!(err, UserIntentError::NoActiveRun(sid));
    }

    #[test]
    fn steer_ok_with_active_run() {
        let (mut router, sid) = session_router();
        let run = Uuid::new_v4();
        router.set_active_run(sid, Some(run)).unwrap();
        let accepted = router
            .submit(UserIntentSubmission {
                session_id: sid,
                intent: UserPromptIntent::Steer,
                text: "prefer tests first".into(),
                origin: ActionOrigin::User,
            })
            .expect("steer");
        assert_eq!(accepted.intent, UserPromptIntent::Steer);
        assert_eq!(accepted.active_run_id, Some(run));
    }

    #[test]
    fn follow_up_enqueues_when_session_exists() {
        let (mut router, sid) = session_router();
        // No active run — still OK.
        let a = router
            .submit(UserIntentSubmission {
                session_id: sid,
                intent: UserPromptIntent::FollowUp,
                text: "then add docs".into(),
                origin: ActionOrigin::User,
            })
            .expect("follow-up");
        assert_eq!(a.follow_up_position, Some(0));
        assert_eq!(router.follow_up_len(sid).unwrap(), 1);

        let b = router
            .submit(UserIntentSubmission {
                session_id: sid,
                intent: UserPromptIntent::FollowUp,
                text: "then open PR".into(),
                origin: ActionOrigin::User,
            })
            .expect("second follow-up");
        assert_eq!(b.follow_up_position, Some(1));
        assert_eq!(router.follow_up_len(sid).unwrap(), 2);

        let first = router.pop_follow_up(sid).unwrap().unwrap();
        assert_eq!(first.text, "then add docs");
        assert_eq!(first.origin, ActionOrigin::User);
    }

    #[test]
    fn unknown_session_rejected_for_all_intents() {
        let mut router = UserIntentRouter::new();
        let sid = Uuid::new_v4();
        for intent in [
            UserPromptIntent::Prompt,
            UserPromptIntent::Steer,
            UserPromptIntent::FollowUp,
        ] {
            let err = router
                .submit(UserIntentSubmission {
                    session_id: sid,
                    intent,
                    text: "x".into(),
                    origin: ActionOrigin::User,
                })
                .unwrap_err();
            assert_eq!(err, UserIntentError::UnknownSession(sid));
        }
    }

    #[test]
    fn steer_and_follow_up_preserve_origin_no_bypass() {
        // Agent-origin steer/follow-up stay agent; stub never elevates to User.
        let (mut router, sid) = session_router();
        let run = Uuid::new_v4();
        router.set_active_run(sid, Some(run)).unwrap();

        let steer = router
            .submit(UserIntentSubmission {
                session_id: sid,
                intent: UserPromptIntent::Steer,
                text: "agent nudge".into(),
                origin: ActionOrigin::Agent,
            })
            .expect("steer");
        assert_eq!(steer.origin, ActionOrigin::Agent);

        let follow = router
            .submit(UserIntentSubmission {
                session_id: sid,
                intent: UserPromptIntent::FollowUp,
                text: "agent later".into(),
                origin: ActionOrigin::Agent,
            })
            .expect("follow-up");
        assert_eq!(follow.origin, ActionOrigin::Agent);
        assert_eq!(
            router.follow_ups(sid).unwrap().front().unwrap().origin,
            ActionOrigin::Agent
        );
    }
}
