//! TUI application TEA loop.
//!
//! Logic lives in sibling modules; this file owns the event loop and re-exports
//! for unit tests.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, Result};
use crossterm::event::EventStream;
use futures_util::StreamExt;
use tokio::sync::mpsc;
use uuid::Uuid;

use crate::backend::UiBackend;
use crate::model::{AppState, RunOptions, drain_stream_frame};
use crate::render::render;
use crate::terminal::TerminalSession;

mod effects;
mod ingest;
mod input;
mod messages;

use effects::{Effect, TaskManager, execute_effect, run_pty_passthrough_effect};
use input::handle_terminal_event;
use messages::{AppMessage, apply_message};

pub(super) fn short(id: Uuid) -> String {
    id.to_string().chars().take(8).collect()
}

pub async fn run(backend: Arc<dyn UiBackend>, options: RunOptions) -> Result<()> {
    let connection = backend
        .connection_info()
        .await
        .context("negotiate TUI backend")?;
    let sessions = backend.list_sessions().await.context("list sessions")?;
    let mut app = AppState::new(connection);
    app.sessions = sessions;

    let mut terminal = TerminalSession::enter(&options)?;
    let (message_tx, mut message_rx) = mpsc::channel::<AppMessage>(128);
    let mut tasks = TaskManager::default();
    let mut terminal_events = EventStream::new();
    let mut ticker = tokio::time::interval(options.tick_rate);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    if let Some(initial) = app.sessions.first().map(|session| session.id) {
        execute_effect(
            Effect::ActivateSession(initial),
            &backend,
            &message_tx,
            &mut tasks,
            &mut app,
        );
    } else {
        let workspace = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        execute_effect(
            Effect::CreateSession { workspace },
            &backend,
            &message_tx,
            &mut tasks,
            &mut app,
        );
    }

    let theme = app.theme();
    terminal.draw(|frame| render(frame, &mut app, theme))?;
    app.dirty = false;
    let mut last_draw = Instant::now();

    while !app.should_quit {
        tokio::select! {
            maybe_event = terminal_events.next() => {
                match maybe_event {
                    Some(Ok(event)) => {
                        let effects = handle_terminal_event(&mut app, event);
                        for effect in effects {
                            if matches!(effect, Effect::EnterPtyPassthrough { .. }) {
                                run_pty_passthrough_effect(
                                    effect,
                                    &backend,
                                    &mut terminal,
                                    &mut app,
                                )
                                .await;
                            } else {
                                execute_effect(
                                    effect,
                                    &backend,
                                    &message_tx,
                                    &mut tasks,
                                    &mut app,
                                );
                            }
                        }
                    }
                    Some(Err(error)) => {
                        app.show_toast(format!("terminal input error: {error}"), true);
                    }
                    None => app.should_quit = true,
                }
            }
            Some(message) = message_rx.recv() => {
                let effects = apply_message(&mut app, message);
                for effect in effects {
                    if matches!(effect, Effect::EnterPtyPassthrough { .. }) {
                        run_pty_passthrough_effect(
                            effect,
                            &backend,
                            &mut terminal,
                            &mut app,
                        )
                        .await;
                    } else {
                        execute_effect(
                            effect,
                            &backend,
                            &message_tx,
                            &mut tasks,
                            &mut app,
                        );
                    }
                }
            }
            _ = ticker.tick() => {
                drain_stream_frame(&mut app);
                app.expire_transients();
            }
        }

        // High-frequency dirty flags coalesce into one paint per tick_rate
        // (stream chunk coalesce is separate — see chunks_coalesce_into_one_assistant_item).
        if crate::model::should_coalesce_redraw(app.dirty, last_draw.elapsed(), options.tick_rate)
            && !app.should_quit
        {
            let theme = app.theme();
            terminal.draw(|frame| render(frame, &mut app, theme))?;
            app.dirty = false;
            last_draw = Instant::now();
        }
    }

    tasks.abort_all();
    terminal.restore()?;
    Ok(())
}

#[cfg(test)]
#[allow(unused_imports)]
use crate::command::{self, CommandAction};
#[cfg(test)]
#[allow(unused_imports)]
use crate::hit::HitKind;
#[cfg(test)]
#[allow(unused_imports)]
use crate::model::{
    EXECUTION_MODE_ALL, ExecutionMode, FilesOverlayState, Focus, ItemKind, LARGE_PASTE_BYTES,
    MAX_PASTE_UPLOAD_BYTES, Overlay, RunState, SessionSummary, TextPromptKind, TimelineItem,
    UiEvent, UiEventKind, format_paste_placeholder, is_paste_placeholder, paste_line_count,
};
#[cfg(test)]
#[allow(unused_imports)]
use crate::render::filtered_sessions;
#[cfg(test)]
#[allow(unused_imports)]
use crossterm::event::{
    Event as TerminalEvent, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent,
    MouseEventKind,
};
#[cfg(test)]
#[allow(unused_imports)]
use ingest::{activity_step_count, ingest_event};
#[cfg(test)]
#[allow(unused_imports)]
use input::{
    apply_hit, cycle_execution_mode, execute_command, handle_key, handle_mouse, handle_overlay_key,
    open_branch_picker, open_files_overlay, open_pty_passthrough, open_session_picker,
    scroll_timeline_home, set_steer_intent,
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ApprovalCard, ApprovalDetailView, ConnectionInfo, UiEvent};

    #[test]
    fn chunks_coalesce_into_one_assistant_item() {
        let mut app = AppState::new(ConnectionInfo::default());
        let run_id = Uuid::new_v4();
        ingest_event(
            &mut app,
            UiEvent {
                sequence: 1,
                at_unix_ms: 1,
                kind: UiEventKind::AgentChunk {
                    run_id,
                    chunk_id: 1,
                    text: "hello ".to_owned(),
                },
            },
        );
        ingest_event(
            &mut app,
            UiEvent {
                sequence: 2,
                at_unix_ms: 2,
                kind: UiEventKind::AgentChunk {
                    run_id,
                    chunk_id: 2,
                    text: "world".to_owned(),
                },
            },
        );
        assert_eq!(app.timeline.len(), 1);
        assert_eq!(app.stream_run_id, Some(run_id));
        // Paced buffer holds text until tick/flush — flush paints full coalesce.
        app.flush_stream_to_timeline();
        assert_eq!(app.timeline[0].body, "hello world");
        assert!(app.timeline[0].streaming_key.is_none());
        assert!(app.stream_run_id.is_none());
    }

    #[test]
    fn cancel_flushes_stream_without_wedging() {
        let mut app = AppState::new(ConnectionInfo::default());
        let run_id = Uuid::new_v4();
        ingest_event(
            &mut app,
            UiEvent {
                sequence: 1,
                at_unix_ms: 1,
                kind: UiEventKind::AgentChunk {
                    run_id,
                    chunk_id: 1,
                    text: "partial".to_owned(),
                },
            },
        );
        assert!(app.stream_run_id.is_some());
        ingest_event(
            &mut app,
            UiEvent {
                sequence: 2,
                at_unix_ms: 2,
                kind: UiEventKind::RunCancelled { run_id },
            },
        );
        assert!(app.stream_run_id.is_none());
        assert!(app.stream_buffer.is_empty());
        assert_eq!(app.run_state, RunState::Idle);
        let assistant = app
            .timeline
            .iter()
            .find(|item| item.kind == ItemKind::Assistant)
            .expect("assistant card");
        assert_eq!(assistant.body, "partial");
        assert!(assistant.streaming_key.is_none());
    }

    #[test]
    fn agent_final_replaces_paced_body() {
        let mut app = AppState::new(ConnectionInfo::default());
        let run_id = Uuid::new_v4();
        ingest_event(
            &mut app,
            UiEvent {
                sequence: 1,
                at_unix_ms: 1,
                kind: UiEventKind::AgentChunk {
                    run_id,
                    chunk_id: 1,
                    text: "draft".to_owned(),
                },
            },
        );
        ingest_event(
            &mut app,
            UiEvent {
                sequence: 2,
                at_unix_ms: 2,
                kind: UiEventKind::AgentFinal {
                    run_id,
                    text: "final answer".to_owned(),
                },
            },
        );
        assert!(app.stream_run_id.is_none());
        assert_eq!(app.timeline.len(), 1);
        assert_eq!(app.timeline[0].body, "final answer");
        assert!(app.timeline[0].streaming_key.is_none());
    }

    #[test]
    fn duplicate_sequence_is_ignored() {
        let mut app = AppState::new(ConnectionInfo::default());
        let event = UiEvent {
            sequence: 1,
            at_unix_ms: 1,
            kind: UiEventKind::Notice {
                title: "once".to_owned(),
                message: "body".to_owned(),
                error: false,
                remediation: None,
            },
        };
        ingest_event(&mut app, event.clone());
        ingest_event(&mut app, event);
        assert_eq!(app.timeline.len(), 1);
    }

    #[test]
    fn large_paste_sets_compact_composer_placeholder() {
        let mut app = AppState::new(ConnectionInfo::default());
        let body = "x".repeat(LARGE_PASTE_BYTES + 1);
        let effects = handle_terminal_event(&mut app, TerminalEvent::Paste(body.clone()));
        assert!(effects.is_empty());
        assert_eq!(app.pending_large_paste.as_deref(), Some(body.as_str()));
        assert_eq!(
            app.composer.text(),
            format_paste_placeholder(body.len(), paste_line_count(&body))
        );
        assert!(matches!(app.overlay, Overlay::LargePaste));
        assert!(!app.composer.text().contains(&body));
    }

    #[test]
    fn oversized_paste_above_upload_cap_is_rejected() {
        let mut app = AppState::new(ConnectionInfo::default());
        let body = "y".repeat(MAX_PASTE_UPLOAD_BYTES + 1);
        let _ = handle_terminal_event(&mut app, TerminalEvent::Paste(body));
        assert!(app.pending_large_paste.is_none());
        assert!(app.composer.is_empty());
        assert!(app.toast.as_ref().is_some_and(|toast| toast.error));
    }

    #[test]
    fn confirm_large_paste_emits_upload_effect() {
        let mut app = AppState::new(ConnectionInfo::default());
        let body = "z".repeat(LARGE_PASTE_BYTES + 10);
        app.pending_large_paste = Some(body.clone());
        app.overlay = Overlay::LargePaste;
        let effects = handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE),
        );
        assert!(app.pending_large_paste.is_none());
        assert!(matches!(app.overlay, Overlay::None));
        assert!(matches!(
            effects.as_slice(),
            [Effect::SendLargePaste {
                label,
                body: effect_body,
                intent: _
            }] if effect_body == &body && is_paste_placeholder(label)
        ));
    }

    fn sample_approval(id: Uuid) -> ApprovalCard {
        ApprovalCard {
            id,
            action_kind: "WriteFile".to_owned(),
            summary: "touch workspace file".to_owned(),
            target: Some("crates/impetus-tui/src/app.rs".to_owned()),
            reason: "changes workspace files".to_owned(),
            fingerprint: "test:deadbeef".to_owned(),
            detail: None,
        }
    }

    #[test]
    fn approval_requested_then_approve_clears_queue_and_overlay() {
        let mut app = AppState::new(ConnectionInfo::default());
        let session_id = Uuid::from_u128(0xA1);
        app.active_session = Some(session_id);
        app.subscription_generation = 3;
        let approval_id = Uuid::from_u128(0xB2);

        ingest_event(
            &mut app,
            UiEvent {
                sequence: 1,
                at_unix_ms: 1,
                kind: UiEventKind::ApprovalRequested {
                    approval: sample_approval(approval_id),
                },
            },
        );
        assert_eq!(app.approval_queue.len(), 1);
        assert!(matches!(app.overlay, Overlay::Approval { selected: 0 }));
        assert_eq!(app.run_state, RunState::WaitingApproval);

        let effects = handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE),
        );
        assert!(matches!(
            effects.as_slice(),
            [Effect::ResolveApproval {
                approval_id: id,
                accepted: true
            }] if *id == approval_id
        ));

        let _ = apply_message(
            &mut app,
            AppMessage::ApprovalResolved {
                session_id,
                generation: 3,
                approval_id,
                accepted: true,
                result: Ok(()),
            },
        );
        assert!(app.approval_queue.is_empty());
        assert!(matches!(app.overlay, Overlay::None));
        assert_eq!(app.run_state, RunState::Working);
    }

    #[test]
    fn approval_deny_and_detail_paths() {
        let mut app = AppState::new(ConnectionInfo::default());
        let session_id = Uuid::from_u128(0xC3);
        app.active_session = Some(session_id);
        app.subscription_generation = 1;
        let approval_id = Uuid::from_u128(0xD4);

        ingest_event(
            &mut app,
            UiEvent {
                sequence: 1,
                at_unix_ms: 1,
                kind: UiEventKind::ApprovalRequested {
                    approval: sample_approval(approval_id),
                },
            },
        );

        let detail_effects = handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE),
        );
        assert!(matches!(
            detail_effects.as_slice(),
            [Effect::LoadApprovalDetail(id)] if *id == approval_id
        ));
        let _ = apply_message(
            &mut app,
            AppMessage::ApprovalDetail {
                session_id,
                generation: 1,
                approval_id,
                result: Ok(ApprovalDetailView {
                    diff_preview: Some("-old\n+new".to_owned()),
                    diff_observation: None,
                    affected_files: vec!["crates/impetus-tui/src/app.rs".to_owned()],
                    estimated_scope: Some("Lines(2)".to_owned()),
                    attachment_refs: vec![],
                    attachments: vec![],
                }),
            },
        );
        assert!(matches!(app.overlay, Overlay::ApprovalDetail));
        assert!(
            app.approval_queue
                .front()
                .and_then(|card| card.detail.as_ref())
                .is_some_and(|detail| detail.affected_files.len() == 1)
        );

        let deny_effects = handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE),
        );
        assert!(matches!(
            deny_effects.as_slice(),
            [Effect::ResolveApproval {
                approval_id: id,
                accepted: false
            }] if *id == approval_id
        ));
        let _ = apply_message(
            &mut app,
            AppMessage::ApprovalResolved {
                session_id,
                generation: 1,
                approval_id,
                accepted: false,
                result: Ok(()),
            },
        );
        assert!(app.approval_queue.is_empty());
        assert!(matches!(app.overlay, Overlay::None));
    }

    #[test]
    fn session_picker_filters_and_activates_selected() {
        let mut app = AppState::new(ConnectionInfo::default());
        let alpha = Uuid::from_u128(0xE5);
        let beta = Uuid::from_u128(0xF6);
        app.sessions = vec![
            SessionSummary::from_session_info(
                alpha,
                None,
                None,
                Some("router hardening".to_owned()),
                Some("saved".to_owned()),
                Some("~/code/one".to_owned()),
            ),
            SessionSummary::from_session_info(
                beta,
                Some(alpha),
                Some(2),
                None,
                None,
                Some("~/code/two".to_owned()),
            ),
        ];

        let _ = handle_key(&mut app, KeyEvent::new(KeyCode::F(2), KeyModifiers::NONE));
        assert!(matches!(
            &app.overlay,
            Overlay::Sessions {
                selected: 0,
                query
            } if query.is_empty()
        ));

        for ch in ['r', 'o', 'u'] {
            let _ = handle_key(
                &mut app,
                KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE),
            );
        }
        match &app.overlay {
            Overlay::Sessions { selected, query } => {
                assert_eq!(query, "rou");
                assert_eq!(*selected, 0);
                assert_eq!(filtered_sessions(&app, query).len(), 1);
                assert_eq!(filtered_sessions(&app, query)[0].id, alpha);
            }
            _ => panic!("expected sessions overlay"),
        }

        let effects = handle_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(
            effects.as_slice(),
            [Effect::ActivateSession(id)] if *id == alpha
        ));
    }

    #[test]
    fn command_palette_opens_filters_and_runs_selected() {
        let mut app = AppState::new(ConnectionInfo::default());

        let _ = handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL),
        );
        assert!(matches!(
            &app.overlay,
            Overlay::Commands {
                selected: 0,
                query
            } if query.is_empty()
        ));

        for ch in ['h', 'e', 'l'] {
            let _ = handle_key(
                &mut app,
                KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE),
            );
        }
        match &app.overlay {
            Overlay::Commands { selected, query } => {
                assert_eq!(query, "hel");
                assert_eq!(*selected, 0);
                let suggestions = command::suggestions(query);
                assert_eq!(suggestions.first().map(|item| item.name), Some("help"));
            }
            _ => panic!("expected commands overlay"),
        }

        let effects = handle_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(effects.is_empty());
        assert!(matches!(app.overlay, Overlay::Help));
    }

    #[test]
    fn command_palette_down_selects_and_runs_command() {
        let mut app = AppState::new(ConnectionInfo::default());

        let _ = handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL),
        );
        let _ = handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE),
        );

        let suggestions = command::suggestions("c");
        assert!(suggestions.len() >= 4);
        assert_eq!(suggestions[0].name, "checkpoint");
        assert_eq!(suggestions[1].name, "children");
        assert_eq!(suggestions[2].name, "clear");
        assert_eq!(suggestions[3].name, "cancel");

        let _ = handle_key(&mut app, KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        let _ = handle_key(&mut app, KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        let _ = handle_key(&mut app, KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        match &app.overlay {
            Overlay::Commands { selected, query } => {
                assert_eq!(query, "c");
                assert_eq!(*selected, 3);
            }
            _ => panic!("expected commands overlay"),
        }

        let effects = handle_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(effects.as_slice(), [Effect::Cancel]));
        assert!(matches!(app.overlay, Overlay::None));
    }

    #[test]
    fn page_keys_scroll_timeline_and_clamp_on_resize() {
        let mut app = AppState::new(ConnectionInfo::default());
        for seq in 1..=20 {
            app.push_item(TimelineItem::new(
                seq,
                seq,
                ItemKind::Notice,
                format!("event-{seq}"),
            ));
        }
        app.note_timeline_metrics(5, 40);
        let _ = handle_key(&mut app, KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE));
        assert!(!app.follow_tail);
        assert!(app.line_scroll_from_bottom >= 10);
        assert_eq!(app.focus, Focus::Timeline);

        app.line_scroll_from_bottom = 100;
        let _ = handle_terminal_event(&mut app, TerminalEvent::Resize(80, 20));
        assert_eq!(app.line_scroll_from_bottom, 35);
        assert!(app.dirty);
    }

    #[test]
    fn consecutive_notice_noise_coalesces_into_one_item() {
        let mut app = AppState::new(ConnectionInfo::default());
        for seq in 1..=5 {
            ingest_event(
                &mut app,
                UiEvent {
                    sequence: seq,
                    at_unix_ms: seq,
                    kind: UiEventKind::BudgetWarning {
                        message: format!("ctx rising · {seq}"),
                    },
                },
            );
        }
        assert_eq!(app.timeline.len(), 1);
        assert_eq!(app.timeline[0].title, "budget warning");
        assert!(app.timeline[0].body.contains("5"));
        assert_eq!(app.last_sequence, 5);
    }

    #[test]
    fn error_notice_shows_explicit_or_static_remediation() {
        let mut app = AppState::new(ConnectionInfo::default());
        ingest_event(
            &mut app,
            UiEvent {
                sequence: 1,
                at_unix_ms: 1,
                kind: UiEventKind::Notice {
                    title: "provider down".to_owned(),
                    message: "profile unreachable".to_owned(),
                    error: true,
                    remediation: Some("Re-select the provider profile in Keychain.".to_owned()),
                },
            },
        );
        assert_eq!(app.timeline.len(), 1);
        assert_eq!(app.timeline[0].kind, ItemKind::Error);
        assert!(
            app.timeline[0]
                .body
                .contains("Re-select the provider profile in Keychain.")
        );
        assert!(app.toast.as_ref().is_some_and(
            |toast| toast.error && toast.text.contains("Re-select the provider profile")
        ));

        ingest_event(
            &mut app,
            UiEvent {
                sequence: 2,
                at_unix_ms: 2,
                kind: UiEventKind::Notice {
                    title: "policy denied".to_owned(),
                    message: "write blocked".to_owned(),
                    error: true,
                    remediation: None,
                },
            },
        );
        assert_eq!(app.timeline.len(), 2);
        assert!(app.timeline[1].body.contains("→ "));
        assert!(app.timeline[1].body.to_ascii_lowercase().contains("policy"));
        assert!(app.timeline[1].details.starts_with("remediation:"));
    }

    #[test]
    fn question_mark_opens_help_when_composer_empty() {
        let mut app = AppState::new(ConnectionInfo::default());
        let _ = handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE),
        );
        assert!(matches!(app.overlay, Overlay::Help));

        app.overlay = Overlay::None;
        app.composer.insert_char('x');
        let _ = handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE),
        );
        assert!(matches!(app.overlay, Overlay::None));
        assert!(app.composer.text().contains('?'));
    }

    #[test]
    fn ctrl_o_opens_session_picker() {
        let mut app = AppState::new(ConnectionInfo::default());
        let _ = handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL),
        );
        assert!(matches!(app.overlay, Overlay::Sessions { .. }));
    }

    #[test]
    fn ctrl_b_loads_branches_via_harness() {
        let mut app = AppState::new(ConnectionInfo::default());
        app.connection.capabilities.insert("git".to_owned());
        app.active_session = Some(Uuid::new_v4());
        let effects = handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('b'), KeyModifiers::CONTROL),
        );
        assert!(matches!(effects.as_slice(), [Effect::LoadBranches]));
    }

    #[test]
    fn branch_picker_enter_switches_via_effect() {
        let mut app = AppState::new(ConnectionInfo::default());
        app.active_session = Some(Uuid::new_v4());
        app.overlay = Overlay::Branches {
            selected: 1,
            query: String::new(),
            branches: vec![
                impetus_client::protocol::GitBranchInfo {
                    name: "main".to_owned(),
                    current: true,
                    upstream: None,
                },
                impetus_client::protocol::GitBranchInfo {
                    name: "feature/x".to_owned(),
                    current: false,
                    upstream: None,
                },
            ],
        };
        let effects = handle_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(
            effects.as_slice(),
            [Effect::SwitchBranch { name }] if name == "feature/x"
        ));
    }

    #[test]
    fn ctrl_shift_p_cycles_prompt_intent() {
        let mut app = AppState::new(ConnectionInfo::default());
        assert_eq!(
            app.prompt_intent,
            impetus_client::protocol::UserPromptIntent::Prompt
        );
        let mods = KeyModifiers::CONTROL | KeyModifiers::SHIFT;
        let _ = handle_key(&mut app, KeyEvent::new(KeyCode::Char('p'), mods));
        assert_eq!(
            app.prompt_intent,
            impetus_client::protocol::UserPromptIntent::Steer
        );
        let _ = handle_key(&mut app, KeyEvent::new(KeyCode::Char('P'), mods));
        assert_eq!(
            app.prompt_intent,
            impetus_client::protocol::UserPromptIntent::FollowUp
        );
        let _ = handle_key(&mut app, KeyEvent::new(KeyCode::Char('p'), mods));
        assert_eq!(
            app.prompt_intent,
            impetus_client::protocol::UserPromptIntent::Prompt
        );
        assert!(matches!(app.overlay, Overlay::None));
    }

    #[test]
    fn ctrl_t_sets_steer_only_when_run_active() {
        let mut app = AppState::new(ConnectionInfo::default());
        let _ = handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL),
        );
        assert_eq!(
            app.prompt_intent,
            impetus_client::protocol::UserPromptIntent::Prompt
        );
        assert!(app.toast.as_ref().is_some_and(|toast| toast.error));

        app.run_state = RunState::Working;
        let _ = handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL),
        );
        assert_eq!(
            app.prompt_intent,
            impetus_client::protocol::UserPromptIntent::Steer
        );
    }

    #[test]
    fn session_picker_accepts_plain_n_and_ctrl_n_for_new() {
        let mut app = AppState::new(ConnectionInfo::default());
        open_session_picker(&mut app);
        let effects = handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE),
        );
        assert!(effects.is_empty());
        assert!(matches!(
            app.overlay,
            Overlay::TextPrompt {
                kind: TextPromptKind::WorkspaceRoot,
                ..
            }
        ));

        open_session_picker(&mut app);
        let effects = handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('n'), KeyModifiers::CONTROL),
        );
        assert!(effects.is_empty());
        assert!(matches!(
            app.overlay,
            Overlay::TextPrompt {
                kind: TextPromptKind::WorkspaceRoot,
                ..
            }
        ));
    }

    #[test]
    fn home_on_timeline_scrolls_to_top() {
        let mut app = AppState::new(ConnectionInfo::default());
        for seq in 1..=20 {
            app.push_item(TimelineItem::new(
                seq,
                seq,
                ItemKind::Notice,
                format!("event-{seq}"),
            ));
        }
        app.note_timeline_metrics(5, 40);
        app.focus = Focus::Timeline;
        app.follow_tail = true;
        let _ = handle_key(&mut app, KeyEvent::new(KeyCode::Home, KeyModifiers::NONE));
        assert!(!app.follow_tail);
        assert_eq!(app.line_scroll_from_bottom, 35);
        assert_eq!(app.focus, Focus::Timeline);
    }

    #[test]
    fn mouse_hit_selects_timeline_and_double_click_toggles() {
        use crate::hit::{HitTarget, RectHit};
        use crossterm::event::MouseButton;

        let mut app = AppState::new(ConnectionInfo::default());
        app.push_item(TimelineItem::new(1, 1, ItemKind::Notice, "one"));
        app.push_item(TimelineItem::new(2, 2, ItemKind::Notice, "two"));
        app.hit_targets = vec![HitTarget {
            rect: RectHit::new(2, 4, 20, 1),
            kind: HitKind::TimelineItem { index: 1 },
        }];

        let effects = handle_mouse(
            &mut app,
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 5,
                row: 4,
                modifiers: KeyModifiers::NONE,
            },
        );
        assert!(effects.is_empty());
        assert_eq!(app.selected_item, Some(1));
        assert_eq!(app.focus, Focus::Timeline);
        assert!(!app.timeline[1].collapsed);

        let effects = handle_mouse(
            &mut app,
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 5,
                row: 4,
                modifiers: KeyModifiers::NONE,
            },
        );
        assert!(effects.is_empty());
        assert!(app.timeline[1].collapsed);
    }

    #[test]
    fn mouse_hit_activates_session_picker_row() {
        use crate::hit::{HitTarget, RectHit};
        use crossterm::event::MouseButton;

        let mut app = AppState::new(ConnectionInfo::default());
        let id = Uuid::new_v4();
        app.sessions.push(SessionSummary {
            id,
            label: "alpha".to_owned(),
            status: "saved".to_owned(),
            workspace: None,
        });
        app.overlay = Overlay::Sessions {
            selected: 0,
            query: String::new(),
        };
        app.hit_targets = vec![HitTarget {
            rect: RectHit::new(10, 10, 30, 2),
            kind: HitKind::SessionPickerRow { index: 0 },
        }];

        let effects = handle_mouse(
            &mut app,
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 12,
                row: 11,
                modifiers: KeyModifiers::NONE,
            },
        );
        assert!(matches!(
            effects.as_slice(),
            [Effect::ActivateSession(session_id)] if *session_id == id
        ));
    }

    #[test]
    fn tab_cycles_focus_shift_tab_cycles_execution_mode() {
        let mut app = AppState::new(ConnectionInfo::default());
        app.active_session = Some(Uuid::new_v4());
        app.focus = Focus::Composer;
        app.mode = ExecutionMode::Plan;

        let effects = handle_key(&mut app, KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert!(effects.is_empty());
        assert_eq!(app.focus, Focus::Timeline);
        assert_eq!(app.mode, ExecutionMode::Plan);

        let effects = handle_key(
            &mut app,
            KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT),
        );
        assert!(matches!(
            effects.as_slice(),
            [Effect::SetExecutionMode {
                mode: ExecutionMode::Auto
            }]
        ));
        assert_eq!(app.mode, ExecutionMode::Plan);
    }

    #[test]
    fn slash_auto_requests_auto_mode_via_ipc() {
        let mut app = AppState::new(ConnectionInfo::default());
        app.active_session = Some(Uuid::new_v4());
        let effects = execute_command(&mut app, CommandAction::SetMode(ExecutionMode::Auto));
        assert!(matches!(
            effects.as_slice(),
            [Effect::SetExecutionMode {
                mode: ExecutionMode::Auto
            }]
        ));
        assert_eq!(app.mode, ExecutionMode::Ask);
    }

    #[test]
    fn bypass_requires_daemon_capability() {
        let mut app = AppState::new(ConnectionInfo::default());
        app.active_session = Some(Uuid::new_v4());
        let effects = execute_command(&mut app, CommandAction::SetMode(ExecutionMode::Bypass));
        assert!(effects.is_empty());
        assert_eq!(app.mode, ExecutionMode::Ask);
        assert!(app.toast.as_ref().is_some_and(|toast| toast.error));

        app.connection
            .capabilities
            .insert("approval_scope_full_auto".to_owned());
        let effects = execute_command(&mut app, CommandAction::SetMode(ExecutionMode::Bypass));
        assert!(matches!(
            effects.as_slice(),
            [Effect::SetExecutionMode {
                mode: ExecutionMode::Bypass
            }]
        ));
    }

    #[test]
    fn execution_mode_updates_only_after_successful_ipc() {
        let mut app = AppState::new(ConnectionInfo::default());
        let session_id = Uuid::new_v4();
        app.active_session = Some(session_id);
        app.subscription_generation = 1;

        let _ = apply_message(
            &mut app,
            AppMessage::ExecutionModeUpdated {
                session_id,
                generation: 1,
                result: Ok(ExecutionMode::Plan),
            },
        );
        assert_eq!(app.mode, ExecutionMode::Plan);
        assert!(app.toast.as_ref().is_some_and(|toast| !toast.error));
    }

    #[test]
    fn mouse_hit_focuses_composer() {
        use crate::hit::{HitTarget, RectHit};
        use crossterm::event::MouseButton;

        let mut app = AppState::new(ConnectionInfo::default());
        app.focus = Focus::Timeline;
        app.hit_targets = vec![HitTarget {
            rect: RectHit::new(0, 20, 80, 3),
            kind: HitKind::Composer,
        }];

        let _ = handle_mouse(
            &mut app,
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 4,
                row: 21,
                modifiers: KeyModifiers::NONE,
            },
        );
        assert_eq!(app.focus, Focus::Composer);
    }

    #[test]
    fn ctrl_f_opens_files_overlay_and_requests_root_list() {
        let mut app = AppState::new(ConnectionInfo::default());
        let session_id = Uuid::new_v4();
        app.active_session = Some(session_id);

        let effects = handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL),
        );
        assert!(matches!(app.overlay, Overlay::Files { .. }));
        assert!(matches!(
            effects.as_slice(),
            [Effect::FilesListDir { path }] if path == "."
        ));
    }

    #[test]
    fn ctrl_backslash_opens_pty_passthrough_when_capable() {
        let mut app = AppState::new(ConnectionInfo::default());
        app.active_session = Some(Uuid::new_v4());
        app.connection.capabilities.insert("pty".to_owned());

        let effects = handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('\\'), KeyModifiers::CONTROL),
        );
        assert!(matches!(
            effects.as_slice(),
            [Effect::EnterPtyPassthrough { command, args }]
                if !command.is_empty() && args.is_empty()
        ));
    }

    #[test]
    fn pty_command_parses_custom_argv() {
        assert_eq!(
            command::parse_command("/pty top -b"),
            Some(CommandAction::PtyPassthrough {
                command: Some("top".to_owned()),
                args: vec!["-b".to_owned()],
            })
        );
        let mut app = AppState::new(ConnectionInfo::default());
        app.active_session = Some(Uuid::new_v4());
        app.connection.capabilities.insert("pty".to_owned());
        let effects = execute_command(
            &mut app,
            CommandAction::PtyPassthrough {
                command: Some("cat".to_owned()),
                args: vec![],
            },
        );
        assert!(matches!(
            effects.as_slice(),
            [Effect::EnterPtyPassthrough { command, args }]
                if command == "cat" && args.is_empty()
        ));
    }

    #[test]
    fn files_overlay_enter_expands_dir_and_lists_children() {
        use impetus_client::protocol::WorkspaceDirEntry;

        let mut app = AppState::new(ConnectionInfo::default());
        app.active_session = Some(Uuid::new_v4());
        let mut state = FilesOverlayState::new();
        state.apply_listing(
            ".",
            vec![WorkspaceDirEntry {
                name: "src".into(),
                path: "src".into(),
                is_dir: true,
                is_symlink: false,
                is_file: false,
            }],
        );
        app.overlay = Overlay::Files { state };

        let effects =
            handle_overlay_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        match &app.overlay {
            Overlay::Files { state } => {
                assert!(state.expanded.contains("src"));
                assert!(state.loading_dirs.contains("src"));
            }
            other => panic!("expected Files overlay, got {other:?}"),
        }
        assert!(matches!(
            effects.as_slice(),
            [Effect::FilesListDir { path }] if path == "src"
        ));
    }

    #[test]
    fn files_dir_listed_message_applies_children() {
        use impetus_client::protocol::{WorkspaceDirEntry, WorkspaceDirListing};

        let mut app = AppState::new(ConnectionInfo::default());
        let session_id = Uuid::new_v4();
        app.active_session = Some(session_id);
        app.subscription_generation = 3;
        app.overlay = Overlay::Files {
            state: FilesOverlayState::new(),
        };

        let _ = apply_message(
            &mut app,
            AppMessage::FilesDirListed {
                session_id,
                generation: 3,
                path: ".".into(),
                result: Ok(WorkspaceDirListing {
                    path: ".".into(),
                    entries: vec![WorkspaceDirEntry {
                        name: "README.md".into(),
                        path: "README.md".into(),
                        is_dir: false,
                        is_symlink: false,
                        is_file: true,
                    }],
                }),
            },
        );
        match &app.overlay {
            Overlay::Files { state } => {
                assert_eq!(state.visible_rows().len(), 1);
                assert_eq!(state.visible_rows()[0].path, "README.md");
            }
            other => panic!("expected Files overlay, got {other:?}"),
        }
    }

    #[test]
    fn child_and_tools_fold_into_one_activity_tree() {
        let mut app = AppState::new(ConnectionInfo::default());
        ingest_event(
            &mut app,
            UiEvent {
                sequence: 1,
                at_unix_ms: 1,
                kind: UiEventKind::ChildStarted {
                    child_id: "c1".into(),
                    role: "Explore".into(),
                    parent_id: "p1".into(),
                },
            },
        );
        ingest_event(
            &mut app,
            UiEvent {
                sequence: 2,
                at_unix_ms: 2,
                kind: UiEventKind::ToolStarted {
                    name: "read_file".into(),
                },
            },
        );
        ingest_event(
            &mut app,
            UiEvent {
                sequence: 3,
                at_unix_ms: 3,
                kind: UiEventKind::ToolFinished {
                    name: "read_file".into(),
                    summary: "src/lib.rs".into(),
                },
            },
        );
        ingest_event(
            &mut app,
            UiEvent {
                sequence: 4,
                at_unix_ms: 4,
                kind: UiEventKind::ToolObserved {
                    call_id: "t2".into(),
                    name: "list_files".into(),
                    arguments: "{}".into(),
                    outcome: "Success".into(),
                    preview: "3 files".into(),
                    artifact: None,
                    error: None,
                },
            },
        );
        ingest_event(
            &mut app,
            UiEvent {
                sequence: 5,
                at_unix_ms: 5,
                kind: UiEventKind::ChildFinished {
                    child_id: "c1".into(),
                    status: "done".into(),
                    summary: Some("explored".into()),
                    error: None,
                },
            },
        );

        assert_eq!(app.timeline.len(), 1);
        let item = &app.timeline[0];
        assert_eq!(item.kind, ItemKind::Activity);
        assert!(item.title.starts_with("◆ Explore done"));
        assert!(item.collapsed);
        assert!(item.streaming_key.is_none());
        assert!(item.body.contains("├ "));
        assert!(item.body.contains("└ "));
        assert!(item.body.contains("read_file"));
        assert!(item.body.contains("list_files"));
        // Started+Finished coalesce to one step for the same tool.
        assert_eq!(activity_step_count(item), 2);
    }

    #[test]
    fn reasoning_summaries_coalesce_into_one_activity() {
        let mut app = AppState::new(ConnectionInfo::default());
        let run_id = Uuid::new_v4();
        ingest_event(
            &mut app,
            UiEvent {
                sequence: 1,
                at_unix_ms: 1,
                kind: UiEventKind::ReasoningSummary {
                    run_id,
                    text: "first".into(),
                },
            },
        );
        ingest_event(
            &mut app,
            UiEvent {
                sequence: 2,
                at_unix_ms: 2,
                kind: UiEventKind::ReasoningSummary {
                    run_id,
                    text: "second summary".into(),
                },
            },
        );
        assert_eq!(app.timeline.len(), 1);
        assert_eq!(app.timeline[0].kind, ItemKind::Activity);
        assert!(app.timeline[0].title.starts_with("◆ reasoning"));
        assert!(app.timeline[0].body.contains("second summary"));
        assert!(!app.timeline[0].body.contains("first"));
    }

    #[test]
    fn standalone_tools_fold_into_tools_activity() {
        let mut app = AppState::new(ConnectionInfo::default());
        ingest_event(
            &mut app,
            UiEvent {
                sequence: 1,
                at_unix_ms: 1,
                kind: UiEventKind::ToolFinished {
                    name: "search".into(),
                    summary: "TODO".into(),
                },
            },
        );
        ingest_event(
            &mut app,
            UiEvent {
                sequence: 2,
                at_unix_ms: 2,
                kind: UiEventKind::ToolFinished {
                    name: "read_file".into(),
                    summary: "a.rs".into(),
                },
            },
        );
        assert_eq!(app.timeline.len(), 1);
        assert_eq!(app.timeline[0].kind, ItemKind::Activity);
        assert!(app.timeline[0].title.starts_with("◆ tools"));
        assert_eq!(activity_step_count(&app.timeline[0]), 2);
    }

    fn connection_with_session_caps() -> ConnectionInfo {
        let mut connection = ConnectionInfo::default();
        connection.capabilities.insert("session_fork".to_owned());
        connection
            .capabilities
            .insert("session_checkpoint".to_owned());
        connection
    }

    #[test]
    fn fork_command_and_ctrl_shift_k_emit_fork_at_tip() {
        let mut app = AppState::new(connection_with_session_caps());
        let session_id = Uuid::from_u128(0xF0);
        app.active_session = Some(session_id);
        app.last_sequence = 9;

        let effects = execute_command(&mut app, CommandAction::Fork(None));
        assert!(matches!(
            effects.as_slice(),
            [Effect::ForkSession { up_to_sequence: 9 }]
        ));

        let effects = execute_command(&mut app, CommandAction::Fork(Some(4)));
        assert!(matches!(
            effects.as_slice(),
            [Effect::ForkSession { up_to_sequence: 4 }]
        ));

        let effects = handle_key(
            &mut app,
            KeyEvent::new(
                KeyCode::Char('k'),
                KeyModifiers::CONTROL | KeyModifiers::SHIFT,
            ),
        );
        assert!(matches!(
            effects.as_slice(),
            [Effect::ForkSession { up_to_sequence: 9 }]
        ));
    }

    #[test]
    fn checkpoint_commands_and_f7_overlay_restore() {
        let mut app = AppState::new(connection_with_session_caps());
        let session_id = Uuid::from_u128(0xC1);
        app.active_session = Some(session_id);

        let effects = execute_command(&mut app, CommandAction::Checkpoint(None));
        assert!(effects.is_empty());
        assert!(matches!(
            app.overlay,
            Overlay::TextPrompt {
                kind: TextPromptKind::CheckpointName,
                ..
            }
        ));

        let effects = execute_command(&mut app, CommandAction::Checkpoint(Some("stable".into())));
        assert!(matches!(
            effects.as_slice(),
            [Effect::CreateCheckpoint { name }] if name == "stable"
        ));

        app.overlay = Overlay::None;
        let effects = handle_key(&mut app, KeyEvent::new(KeyCode::F(7), KeyModifiers::NONE));
        assert!(matches!(effects.as_slice(), [Effect::LoadCheckpoints]));

        let checkpoint_id = Uuid::from_u128(0xC2);
        let generation = app.subscription_generation;
        let _ = apply_message(
            &mut app,
            AppMessage::CheckpointsLoaded {
                session_id,
                generation,
                result: Ok(vec![impetus_client::protocol::CheckpointInfo {
                    id: checkpoint_id,
                    session_id,
                    name: "stable".into(),
                    sequence: 3,
                    created_at_unix_ms: 1,
                }]),
            },
        );
        assert!(matches!(
            &app.overlay,
            Overlay::Checkpoints { selected: 0, checkpoints }
                if checkpoints.len() == 1 && checkpoints[0].id == checkpoint_id
        ));

        let effects = handle_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(
            effects.as_slice(),
            [Effect::RestoreCheckpoint { checkpoint_id: id }] if *id == checkpoint_id
        ));
    }

    #[test]
    fn new_command_opens_workspace_path_prompt() {
        let mut app = AppState::new(ConnectionInfo::default());
        let effects = execute_command(&mut app, CommandAction::NewSession);
        assert!(effects.is_empty());
        assert!(matches!(
            app.overlay,
            Overlay::TextPrompt {
                kind: TextPromptKind::WorkspaceRoot,
                ..
            }
        ));

        if let Overlay::TextPrompt { value, .. } = &mut app.overlay {
            *value = "/tmp/impetus-ws".into();
        }
        let effects = handle_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(
            effects.as_slice(),
            [Effect::CreateSession { workspace }]
                if workspace == std::path::Path::new("/tmp/impetus-ws")
        ));
    }

    #[test]
    fn session_forked_activates_new_branch() {
        let mut app = AppState::new(connection_with_session_caps());
        let forked = Uuid::from_u128(0xF00D);
        let effects = apply_message(&mut app, AppMessage::SessionForked(Ok(forked)));
        assert!(matches!(
            effects.as_slice(),
            [Effect::RefreshSessions, Effect::ActivateSession(id)] if *id == forked
        ));
    }

    fn connection_with_artifact_upload() -> ConnectionInfo {
        let mut connection = ConnectionInfo::default();
        connection.capabilities.insert("artifact_upload".to_owned());
        connection
    }

    #[test]
    fn attach_command_opens_path_overlay_or_uploads() {
        let mut app = AppState::new(connection_with_artifact_upload());
        app.active_session = Some(Uuid::from_u128(0xA77));

        let effects = execute_command(&mut app, CommandAction::Attach { path: None });
        assert!(effects.is_empty());
        assert!(matches!(app.overlay, Overlay::AttachPath { .. }));

        let effects = execute_command(
            &mut app,
            CommandAction::Attach {
                path: Some("/tmp/notes.txt".into()),
            },
        );
        assert!(matches!(
            effects.as_slice(),
            [Effect::UploadArtifact { path }] if path == "/tmp/notes.txt"
        ));
    }

    #[test]
    fn attach_path_overlay_enter_emits_upload() {
        let mut app = AppState::new(connection_with_artifact_upload());
        app.active_session = Some(Uuid::from_u128(0xA78));
        app.overlay = Overlay::AttachPath {
            path: "/tmp/hello.rs".into(),
        };
        let effects = handle_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(app.overlay, Overlay::None));
        assert!(matches!(
            effects.as_slice(),
            [Effect::UploadArtifact { path }] if path == "/tmp/hello.rs"
        ));
    }

    #[test]
    fn artifact_uploaded_sets_composer_placeholder_and_pending() {
        let mut app = AppState::new(connection_with_artifact_upload());
        let session_id = Uuid::from_u128(0xA79);
        app.active_session = Some(session_id);
        app.subscription_generation = 2;

        let pending = crate::model::PendingArtifact {
            artifact: impetus_client::protocol::DurableArtifactRef {
                id: "art-1".into(),
                byte_count: 2048,
            },
            path: "/tmp/notes.txt".into(),
            file_name: "notes.txt".into(),
            content_type: Some("text/plain".into()),
            label: crate::model::format_attach_placeholder("notes.txt", 2048, Some("text/plain")),
        };
        let _ = apply_message(
            &mut app,
            AppMessage::ArtifactUploaded {
                session_id,
                generation: 2,
                path: "/tmp/notes.txt".into(),
                result: Ok(pending.clone()),
            },
        );
        assert_eq!(app.composer.text(), pending.label);
        assert_eq!(
            app.pending_artifact
                .as_ref()
                .map(|p| p.artifact.id.as_str()),
            Some("art-1")
        );

        app.composer.clear();
        app.composer.insert_str(&pending.label);
        let effects = handle_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(app.pending_artifact.is_none());
        assert!(matches!(
            effects.as_slice(),
            [Effect::SendMessage {
                text,
                artifact: Some(artifact),
                ..
            }] if text == &pending.label && artifact.id == "art-1"
        ));
    }

    #[test]
    fn user_input_with_artifact_shows_ref_in_timeline() {
        let mut app = AppState::new(ConnectionInfo::default());
        ingest_event(
            &mut app,
            UiEvent {
                sequence: 1,
                at_unix_ms: 1,
                kind: UiEventKind::UserInput {
                    text: "[Attached · notes.txt · 2 KB · text/plain]".into(),
                    artifact: Some(impetus_client::protocol::DurableArtifactRef {
                        id: "art-xyz".into(),
                        byte_count: 2048,
                    }),
                },
            },
        );
        assert_eq!(app.timeline.len(), 1);
        assert!(app.timeline[0].body.contains("artifact art-xyz · 2 KB"));
        assert!(app.timeline[0].body.contains("[Attached · notes.txt"));
    }

    fn connection_with_model_caps() -> ConnectionInfo {
        let mut connection = ConnectionInfo::default();
        connection.capabilities.insert("list_providers".to_owned());
        connection.capabilities.insert("session_model".to_owned());
        connection
    }

    #[test]
    fn f8_and_model_command_open_catalog_picker_effect() {
        let mut app = AppState::new(connection_with_model_caps());
        app.active_session = Some(Uuid::from_u128(0x337));

        let effects = handle_key(&mut app, KeyEvent::new(KeyCode::F(8), KeyModifiers::NONE));
        assert!(matches!(effects.as_slice(), [Effect::OpenModelPicker]));

        let effects = execute_command(&mut app, CommandAction::ModelPicker);
        assert!(matches!(effects.as_slice(), [Effect::OpenModelPicker]));
    }

    #[test]
    fn session_model_restored_opens_picker_and_shows_selection() {
        let mut app = AppState::new(connection_with_model_caps());
        let session_id = Uuid::from_u128(0x337);
        app.active_session = Some(session_id);
        app.subscription_generation = 1;

        let mut healthy = impetus_client::protocol::ModelProviderStatus::basic(
            "mock",
            "mock-fast",
            impetus_client::protocol::ModelProviderHealthLabel::Healthy,
            true,
        );
        healthy.reasoning_efforts = vec!["low".into(), "high".into()];
        let mut down = impetus_client::protocol::ModelProviderStatus::basic(
            "offline",
            "offline-model",
            impetus_client::protocol::ModelProviderHealthLabel::Unavailable {
                last_error_redacted: "down".into(),
            },
            false,
        );
        down.availability = impetus_client::protocol::ModelAvailability::Unavailable;

        let selection = impetus_client::protocol::SessionModelSelection {
            provider_id: "mock".into(),
            model_id: "mock-fast".into(),
            reasoning_effort: Some("high".into()),
        };

        let _ = apply_message(
            &mut app,
            AppMessage::SessionModelRestored {
                session_id,
                generation: 1,
                result: Ok((vec![healthy, down], selection.clone())),
                open_picker: true,
            },
        );

        assert_eq!(app.session_model.as_ref(), Some(&selection));
        assert_eq!(app.provider_catalog.len(), 2);
        assert!(matches!(app.overlay, Overlay::ModelPicker { .. }));
        assert!(app.session_model_label().contains("mock/mock-fast"));
        assert!(!crate::catalog::row_is_selectable(&app.provider_catalog[1]));
    }

    #[test]
    fn model_picker_rejects_unavailable_provider_on_enter() {
        let mut app = AppState::new(connection_with_model_caps());
        let mut down = impetus_client::protocol::ModelProviderStatus::basic(
            "offline",
            "offline-model",
            impetus_client::protocol::ModelProviderHealthLabel::Unavailable {
                last_error_redacted: "down".into(),
            },
            false,
        );
        down.availability = impetus_client::protocol::ModelAvailability::Unavailable;
        app.provider_catalog = vec![down];
        app.overlay = Overlay::ModelPicker {
            state: crate::catalog::ModelPickerState::default(),
        };

        let effects =
            handle_overlay_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(effects.is_empty());
        assert!(matches!(app.overlay, Overlay::ModelPicker { .. }));
        assert!(app.toast.as_ref().is_some_and(|t| t.error));
    }

    #[test]
    fn model_picker_commit_emits_set_session_model() {
        let mut app = AppState::new(connection_with_model_caps());
        app.active_session = Some(Uuid::from_u128(0x337));
        let mut row = impetus_client::protocol::ModelProviderStatus::basic(
            "mock",
            "mock-fast",
            impetus_client::protocol::ModelProviderHealthLabel::Healthy,
            true,
        );
        row.reasoning_efforts = vec!["medium".into()];
        app.provider_catalog = vec![row];
        let state = crate::catalog::ModelPickerState {
            draft_provider_id: Some("mock".into()),
            draft_model_id: Some("mock-fast".into()),
            draft_reasoning: Some("medium".into()),
            step: crate::catalog::ModelPickerStep::Reasoning,
            visited_reasoning: true,
            selected: 0,
            ..Default::default()
        };
        app.overlay = Overlay::ModelPicker { state };

        let effects =
            handle_overlay_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(
            effects.as_slice(),
            [Effect::SetSessionModel {
                provider_id,
                model_id,
                reasoning_effort: Some(effort),
                options: None,
            }] if provider_id == "mock" && model_id == "mock-fast" && effort == "medium"
        ));
    }
}
