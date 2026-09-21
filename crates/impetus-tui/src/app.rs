use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use crossterm::event::{
    Event as TerminalEvent, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEvent,
    MouseEventKind,
};
use futures_util::StreamExt;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use uuid::Uuid;

use crate::backend::UiBackend;
use crate::command::{self, CommandAction};
use crate::hit::{HitKind, PointerClick, cycle_prompt_intent, is_double_click, resolve_hit};
use crate::model::{
    AppState, EXECUTION_MODE_ALL, ExecutionMode, Focus, ItemKind, LARGE_PASTE_BYTES,
    MAX_PASTE_UPLOAD_BYTES, Overlay, RunOptions, RunState, SessionSummary, TimelineItem, UiEvent,
    UiEventKind, bounded, drain_stream_frame, execution_mode_is_available,
    format_paste_placeholder, ingest_stream_chunk, is_paste_placeholder, max_scroll_from_bottom,
    normalize_paste, paste_line_count,
};
use crate::render::{filtered_sessions, render};
use crate::terminal::TerminalSession;
use crate::theme::{self, THEME_CATALOG};

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
        execute_effect(
            Effect::CreateSession,
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
                            execute_effect(
                                effect,
                                &backend,
                                &message_tx,
                                &mut tasks,
                                &mut app,
                            );
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
                    execute_effect(
                        effect,
                        &backend,
                        &message_tx,
                        &mut tasks,
                        &mut app,
                    );
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

#[derive(Default)]
struct TaskManager {
    subscription: Option<JoinHandle<()>>,
}

fn spawn_detached(future: impl std::future::Future<Output = ()> + Send + 'static) {
    drop(tokio::spawn(future));
}

impl TaskManager {
    fn replace_subscription(&mut self, handle: JoinHandle<()>) {
        if let Some(previous) = self.subscription.replace(handle) {
            previous.abort();
        }
    }

    fn abort_all(&mut self) {
        if let Some(handle) = self.subscription.take() {
            handle.abort();
        }
    }
}

#[derive(Debug)]
enum AppMessage {
    SessionsLoaded(Result<Vec<SessionSummary>, String>),
    SessionCreated(Result<Uuid, String>),
    SessionActivated {
        session_id: Uuid,
        generation: u64,
        result: Result<String, String>,
    },
    EventBatch {
        session_id: Uuid,
        generation: u64,
        result: Result<Vec<UiEvent>, String>,
    },
    PromptSent {
        session_id: Uuid,
        generation: u64,
        result: Result<String, String>,
    },
    Cancelled {
        session_id: Uuid,
        generation: u64,
        result: Result<String, String>,
    },
    ApprovalResolved {
        session_id: Uuid,
        generation: u64,
        approval_id: Uuid,
        accepted: bool,
        result: Result<(), String>,
    },
    ApprovalDetail {
        session_id: Uuid,
        generation: u64,
        approval_id: Uuid,
        result: Result<crate::model::ApprovalDetailView, String>,
    },
    Diagnostics(Result<String, String>),
    ExecutionModeUpdated {
        session_id: Uuid,
        generation: u64,
        result: Result<ExecutionMode, String>,
    },
}

#[derive(Debug)]
enum Effect {
    RefreshSessions,
    CreateSession,
    ActivateSession(Uuid),
    SendMessage {
        text: String,
        intent: impetus_client::protocol::UserPromptIntent,
    },
    SendLargePaste {
        label: String,
        body: String,
        intent: impetus_client::protocol::UserPromptIntent,
    },
    Cancel,
    ResolveApproval {
        approval_id: Uuid,
        accepted: bool,
    },
    LoadApprovalDetail(Uuid),
    Diagnostics,
    ListChildren,
    SetExecutionMode {
        mode: ExecutionMode,
    },
}

fn execute_effect(
    effect: Effect,
    backend: &Arc<dyn UiBackend>,
    tx: &mpsc::Sender<AppMessage>,
    tasks: &mut TaskManager,
    app: &mut AppState,
) {
    match effect {
        Effect::RefreshSessions => {
            let backend = backend.clone();
            let tx = tx.clone();
            spawn_detached(async move {
                let result = backend
                    .list_sessions()
                    .await
                    .map_err(|error| error.to_string());
                let _ = tx.send(AppMessage::SessionsLoaded(result)).await;
            });
        }
        Effect::CreateSession => {
            app.status_message = "creating session".to_owned();
            app.dirty = true;
            let backend = backend.clone();
            let tx = tx.clone();
            spawn_detached(async move {
                let result = std::env::current_dir()
                    .context("resolve current workspace")
                    .and_then(|path| {
                        path.canonicalize()
                            .context("canonicalize current workspace")
                    });
                let result = match result {
                    Ok(workspace) => backend
                        .create_session(workspace)
                        .await
                        .map_err(|error| error.to_string()),
                    Err(error) => Err(error.to_string()),
                };
                let _ = tx.send(AppMessage::SessionCreated(result)).await;
            });
        }
        Effect::ActivateSession(session_id) => {
            app.subscription_generation = app.subscription_generation.wrapping_add(1);
            let generation = app.subscription_generation;
            app.active_session = Some(session_id);
            app.clear_stream();
            app.timeline.clear();
            app.selected_item = None;
            app.approval_queue.clear();
            app.last_sequence = 0;
            app.follow_tail = true;
            app.line_scroll_from_bottom = 0;
            app.status_message = "attaching".to_owned();
            app.run_state = RunState::Idle;
            app.overlay = Overlay::None;
            app.dirty = true;

            let backend = backend.clone();
            let tx = tx.clone();
            let handle = tokio::spawn(async move {
                let resumed = backend
                    .resume_session(session_id)
                    .await
                    .map_err(|error| error.to_string());
                let resume_ok = resumed.is_ok();
                if tx
                    .send(AppMessage::SessionActivated {
                        session_id,
                        generation,
                        result: resumed,
                    })
                    .await
                    .is_err()
                {
                    return;
                }
                if !resume_ok {
                    return;
                }

                let mode_result = backend
                    .get_execution_mode(session_id)
                    .await
                    .map_err(|error| error.to_string());
                if tx
                    .send(AppMessage::ExecutionModeUpdated {
                        session_id,
                        generation,
                        result: mode_result,
                    })
                    .await
                    .is_err()
                {
                    return;
                }

                let mut after_sequence = 0u64;
                let mut backoff = Duration::from_millis(250);
                loop {
                    let stream = backend.subscribe(session_id, after_sequence).await;
                    let mut stream = match stream {
                        Ok(stream) => {
                            backoff = Duration::from_millis(250);
                            stream
                        }
                        Err(error) => {
                            if tx
                                .send(AppMessage::EventBatch {
                                    session_id,
                                    generation,
                                    result: Err(format!("event subscription: {error}")),
                                })
                                .await
                                .is_err()
                            {
                                return;
                            }
                            tokio::time::sleep(backoff).await;
                            backoff = backoff.saturating_mul(2).min(Duration::from_secs(5));
                            continue;
                        }
                    };

                    loop {
                        match stream.next_batch().await {
                            Ok(events) => {
                                if let Some(max_sequence) =
                                    events.iter().map(|event| event.sequence).max()
                                {
                                    after_sequence = after_sequence.max(max_sequence);
                                }
                                if tx
                                    .send(AppMessage::EventBatch {
                                        session_id,
                                        generation,
                                        result: Ok(events),
                                    })
                                    .await
                                    .is_err()
                                {
                                    return;
                                }
                            }
                            Err(error) => {
                                if tx
                                    .send(AppMessage::EventBatch {
                                        session_id,
                                        generation,
                                        result: Err(format!("event stream disconnected: {error}")),
                                    })
                                    .await
                                    .is_err()
                                {
                                    return;
                                }
                                break;
                            }
                        }
                    }
                    tokio::time::sleep(backoff).await;
                    backoff = backoff.saturating_mul(2).min(Duration::from_secs(5));
                }
            });
            tasks.replace_subscription(handle);
        }
        Effect::SendMessage { text, intent } => {
            let Some(session_id) = app.active_session else {
                app.show_toast("No active session. Create or resume one first.", true);
                return;
            };
            app.run_state = RunState::Working;
            app.status_message = format!("submitting · {}", intent.label());
            app.dirty = true;
            let generation = app.subscription_generation;
            let backend = backend.clone();
            let tx = tx.clone();
            spawn_detached(async move {
                let result = backend
                    .send_message(session_id, text, intent)
                    .await
                    .map_err(|error| error.to_string());
                let _ = tx
                    .send(AppMessage::PromptSent {
                        session_id,
                        generation,
                        result,
                    })
                    .await;
            });
        }
        Effect::SendLargePaste {
            label,
            body,
            intent,
        } => {
            let Some(session_id) = app.active_session else {
                app.show_toast("No active session. Create or resume one first.", true);
                return;
            };
            if !app.connection.capabilities.contains("artifact_upload") {
                app.show_toast(
                    "Large paste upload requires a daemon with artifact_upload capability.",
                    true,
                );
                app.run_state = RunState::Failed;
                return;
            }
            app.run_state = RunState::Working;
            app.status_message = format!("uploading paste · {}", intent.label());
            app.dirty = true;
            let generation = app.subscription_generation;
            let backend = backend.clone();
            let tx = tx.clone();
            spawn_detached(async move {
                let result = backend
                    .send_large_paste(session_id, label, body.into_bytes(), intent)
                    .await
                    .map_err(|error| error.to_string());
                let _ = tx
                    .send(AppMessage::PromptSent {
                        session_id,
                        generation,
                        result,
                    })
                    .await;
            });
        }
        Effect::Cancel => {
            let Some(session_id) = app.active_session else {
                app.show_toast("No active session to cancel.", true);
                return;
            };
            app.run_state = RunState::Cancelling;
            app.status_message = "cancellation requested".to_owned();
            app.dirty = true;
            let generation = app.subscription_generation;
            let backend = backend.clone();
            let tx = tx.clone();
            spawn_detached(async move {
                let result = backend
                    .cancel(session_id)
                    .await
                    .map_err(|error| error.to_string());
                let _ = tx
                    .send(AppMessage::Cancelled {
                        session_id,
                        generation,
                        result,
                    })
                    .await;
            });
        }
        Effect::ResolveApproval {
            approval_id,
            accepted,
        } => {
            let Some(session_id) = app.active_session else {
                app.show_toast("No active session owns this approval.", true);
                return;
            };
            app.status_message = if accepted {
                "approving exact action".to_owned()
            } else {
                "rejecting action".to_owned()
            };
            app.dirty = true;
            let generation = app.subscription_generation;
            let backend = backend.clone();
            let tx = tx.clone();
            spawn_detached(async move {
                let result = backend
                    .resolve_approval(session_id, approval_id, accepted)
                    .await
                    .map_err(|error| error.to_string());
                let _ = tx
                    .send(AppMessage::ApprovalResolved {
                        session_id,
                        generation,
                        approval_id,
                        accepted,
                        result,
                    })
                    .await;
            });
        }
        Effect::LoadApprovalDetail(approval_id) => {
            let Some(session_id) = app.active_session else {
                app.show_toast("No active session owns this approval.", true);
                return;
            };
            let generation = app.subscription_generation;
            let backend = backend.clone();
            let tx = tx.clone();
            spawn_detached(async move {
                let result = backend
                    .approval_detail(session_id, approval_id)
                    .await
                    .map_err(|error| error.to_string());
                let _ = tx
                    .send(AppMessage::ApprovalDetail {
                        session_id,
                        generation,
                        approval_id,
                        result,
                    })
                    .await;
            });
        }
        Effect::Diagnostics => {
            app.overlay = Overlay::Diagnostics {
                text: "Loading redacted diagnostics…".to_owned(),
            };
            app.dirty = true;
            let backend = backend.clone();
            let tx = tx.clone();
            spawn_detached(async move {
                let result = backend
                    .diagnostics()
                    .await
                    .map_err(|error| error.to_string());
                let _ = tx.send(AppMessage::Diagnostics(result)).await;
            });
        }
        Effect::ListChildren => {
            let Some(session_id) = app.active_session else {
                app.show_toast("No active session. Create or resume one first.", true);
                return;
            };
            app.overlay = Overlay::Diagnostics {
                text: "Loading child runs…".to_owned(),
            };
            app.dirty = true;
            let backend = backend.clone();
            let tx = tx.clone();
            spawn_detached(async move {
                let result = backend
                    .list_child_runs(session_id)
                    .await
                    .map_err(|error| error.to_string());
                let _ = tx.send(AppMessage::Diagnostics(result)).await;
            });
        }
        Effect::SetExecutionMode { mode } => {
            let Some(session_id) = app.active_session else {
                app.show_toast("No active session. Create or resume one first.", true);
                return;
            };
            if !execution_mode_is_available(mode, &app.connection.capabilities) {
                app.show_toast(
                    format!(
                        "{} is not supported by the current daemon contract.",
                        mode.label()
                    ),
                    true,
                );
                return;
            }
            app.status_message = format!("setting mode · {}", mode.label());
            app.dirty = true;
            let generation = app.subscription_generation;
            let backend = backend.clone();
            let tx = tx.clone();
            spawn_detached(async move {
                let result = backend
                    .set_execution_mode(session_id, mode)
                    .await
                    .map_err(|error| error.to_string());
                let _ = tx
                    .send(AppMessage::ExecutionModeUpdated {
                        session_id,
                        generation,
                        result,
                    })
                    .await;
            });
        }
    }
}

fn apply_message(app: &mut AppState, message: AppMessage) -> Vec<Effect> {
    match message {
        AppMessage::SessionsLoaded(result) => match result {
            Ok(sessions) => {
                app.sessions = sessions;
                app.dirty = true;
            }
            Err(error) => app.show_toast(format!("session refresh failed: {error}"), true),
        },
        AppMessage::SessionCreated(result) => match result {
            Ok(session_id) => {
                app.show_toast(format!("Created session {}", short(session_id)), false);
                return vec![Effect::RefreshSessions, Effect::ActivateSession(session_id)];
            }
            Err(error) => app.show_toast(format!("create session failed: {error}"), true),
        },
        AppMessage::SessionActivated {
            session_id,
            generation,
            result,
        } => {
            if !is_current_operation(app, session_id, generation) {
                return vec![];
            }
            match result {
                Ok(status) => {
                    app.status_message = format!("attached · {status}");
                    if let Some(session) = app
                        .sessions
                        .iter_mut()
                        .find(|session| session.id == session_id)
                    {
                        session.status = status;
                    }
                }
                Err(error) => {
                    app.status_message = "attach failed".to_owned();
                    app.show_toast(format!("attach failed: {error}"), true);
                }
            }
            app.dirty = true;
        }
        AppMessage::EventBatch {
            session_id,
            generation,
            result,
        } => {
            if !is_current_operation(app, session_id, generation) {
                return vec![];
            }
            match result {
                Ok(events) => {
                    app.status_message = "live".to_owned();
                    for event in events {
                        ingest_event(app, event);
                    }
                }
                Err(error) => {
                    // Do not leave paced backlog or streaming card wedged on disconnect.
                    app.flush_stream_to_timeline();
                    app.status_message = "reconnecting".to_owned();
                    app.show_toast(error, true);
                }
            }
            app.dirty = true;
        }
        AppMessage::PromptSent {
            session_id,
            generation,
            result,
        } => {
            if !is_current_operation(app, session_id, generation) {
                return vec![];
            }
            match result {
                Ok(status) => {
                    app.status_message = format!("run · {status}");
                    app.show_toast("Task accepted by impetusd.", false);
                }
                Err(error) => {
                    app.run_state = RunState::Failed;
                    app.status_message = "submit failed".to_owned();
                    app.show_toast(format!("submit failed: {error}"), true);
                }
            }
        }
        AppMessage::Cancelled {
            session_id,
            generation,
            result,
        } => {
            if !is_current_operation(app, session_id, generation) {
                return vec![];
            }
            match result {
                Ok(status) => {
                    app.flush_stream_to_timeline();
                    app.status_message = format!("cancel · {status}");
                    app.show_toast("Cancellation forwarded to the daemon.", false);
                }
                Err(error) => {
                    app.flush_stream_to_timeline();
                    app.run_state = RunState::Unknown;
                    app.show_toast(format!("cancel failed: {error}"), true);
                }
            }
        }
        AppMessage::ApprovalResolved {
            session_id,
            generation,
            approval_id,
            accepted,
            result,
        } => {
            if !is_current_operation(app, session_id, generation) {
                return vec![];
            }
            match result {
                Ok(()) => {
                    app.approval_queue
                        .retain(|approval| approval.id != approval_id);
                    app.overlay = if app.approval_queue.is_empty() {
                        Overlay::None
                    } else {
                        Overlay::Approval { selected: 0 }
                    };
                    app.run_state = if app.approval_queue.is_empty() {
                        RunState::Working
                    } else {
                        RunState::WaitingApproval
                    };
                    app.show_toast(
                        if accepted {
                            "Approved this exact action once."
                        } else {
                            "Action rejected."
                        },
                        false,
                    );
                }
                Err(error) => {
                    app.show_toast(format!("approval response failed: {error}"), true);
                }
            }
        }
        AppMessage::ApprovalDetail {
            session_id,
            generation,
            approval_id,
            result,
        } => {
            if !is_current_operation(app, session_id, generation) {
                return vec![];
            }
            match result {
                Ok(detail) => {
                    if let Some(approval) = app
                        .approval_queue
                        .iter_mut()
                        .find(|approval| approval.id == approval_id)
                    {
                        approval.detail = Some(detail);
                    }
                    app.overlay = Overlay::ApprovalDetail;
                    app.dirty = true;
                }
                Err(error) => {
                    app.overlay = Overlay::Approval { selected: 0 };
                    app.show_toast(format!("approval detail failed: {error}"), true);
                }
            }
        }
        AppMessage::ExecutionModeUpdated {
            session_id,
            generation,
            result,
        } => {
            if !is_current_operation(app, session_id, generation) {
                return vec![];
            }
            match result {
                Ok(mode) => {
                    app.mode = mode;
                    app.status_message = format!("mode · {}", mode.label());
                    app.show_toast(format!("Execution mode: {}", mode.label()), false);
                }
                Err(error) => {
                    app.show_toast(format!("mode change failed: {error}"), true);
                }
            }
            app.dirty = true;
        }
        AppMessage::Diagnostics(result) => {
            app.overlay = match result {
                Ok(text) => Overlay::Diagnostics { text },
                Err(error) => Overlay::Message {
                    title: " diagnostics failed ".to_owned(),
                    body: error,
                    error: true,
                },
            };
            app.dirty = true;
        }
    }
    vec![]
}

fn is_current_operation(app: &AppState, session_id: Uuid, generation: u64) -> bool {
    app.active_session == Some(session_id) && app.subscription_generation == generation
}

fn handle_terminal_event(app: &mut AppState, event: TerminalEvent) -> Vec<Effect> {
    match event {
        TerminalEvent::Key(key) if key.kind == KeyEventKind::Press => handle_key(app, key),
        TerminalEvent::Paste(text) => {
            let text = normalize_paste(&text);
            if text.len() > MAX_PASTE_UPLOAD_BYTES {
                app.show_toast(
                    format!(
                        "Paste is too large ({} bytes). Maximum upload size is {} bytes.",
                        text.len(),
                        MAX_PASTE_UPLOAD_BYTES
                    ),
                    true,
                );
                app.dirty = true;
                return vec![];
            }
            if text.len() > LARGE_PASTE_BYTES {
                let placeholder = format_paste_placeholder(text.len(), paste_line_count(&text));
                app.pending_large_paste = Some(text);
                app.composer.clear();
                app.composer.insert_str(&placeholder);
                app.overlay = Overlay::LargePaste;
            } else {
                app.composer.insert_str(&text);
            }
            app.dirty = true;
            vec![]
        }
        TerminalEvent::Mouse(mouse) => handle_mouse(app, mouse),
        TerminalEvent::Resize(_, _) | TerminalEvent::FocusGained | TerminalEvent::FocusLost => {
            app.clamp_timeline_scroll();
            app.dirty = true;
            vec![]
        }
        _ => vec![],
    }
}

fn handle_mouse(app: &mut AppState, mouse: MouseEvent) -> Vec<Effect> {
    match mouse.kind {
        MouseEventKind::ScrollUp => {
            scroll_up(app, 4);
            vec![]
        }
        MouseEventKind::ScrollDown => {
            scroll_down(app, 4);
            vec![]
        }
        MouseEventKind::Down(_) => {
            let column = mouse.column;
            let row = mouse.row;
            let Some(kind) = resolve_hit(&app.hit_targets, column, row) else {
                return vec![];
            };
            let now = Instant::now();
            let double = is_double_click(app.last_pointer.as_ref(), column, row, kind, now);
            app.last_pointer = Some(PointerClick {
                at: now,
                column,
                row,
                kind,
            });
            let effects = apply_hit(app, kind, double);
            app.dirty = true;
            effects
        }
        _ => vec![],
    }
}

fn apply_hit(app: &mut AppState, kind: HitKind, double: bool) -> Vec<Effect> {
    match kind {
        HitKind::Composer => {
            if matches!(app.overlay, Overlay::None) {
                app.focus = Focus::Composer;
            }
            vec![]
        }
        HitKind::TimelineItem { index } => {
            if !matches!(app.overlay, Overlay::None) {
                return vec![];
            }
            if index < app.timeline.len() {
                app.selected_item = Some(index);
                app.focus = Focus::Timeline;
                if double {
                    toggle_selected_item(app);
                }
            }
            vec![]
        }
        HitKind::SessionPickerRow { index } => {
            let Overlay::Sessions { query, .. } = &app.overlay else {
                return vec![];
            };
            let query = query.clone();
            let chosen = filtered_sessions(app, &query)
                .get(index)
                .map(|session| session.id);
            app.overlay = Overlay::Sessions {
                selected: index,
                query,
            };
            if let Some(session_id) = chosen {
                return vec![Effect::ActivateSession(session_id)];
            }
            vec![]
        }
        HitKind::SessionPanelRow { index } => {
            if !matches!(app.overlay, Overlay::None) {
                return vec![];
            }
            if let Some(session) = app.sessions.get(index) {
                return vec![Effect::ActivateSession(session.id)];
            }
            vec![]
        }
        HitKind::ApprovalAccept => app
            .approval_queue
            .front()
            .map(|approval| Effect::ResolveApproval {
                approval_id: approval.id,
                accepted: true,
            })
            .into_iter()
            .collect(),
        HitKind::ApprovalReject => app
            .approval_queue
            .front()
            .map(|approval| Effect::ResolveApproval {
                approval_id: approval.id,
                accepted: false,
            })
            .into_iter()
            .collect(),
        HitKind::ApprovalInspect => {
            if let Some(approval) = app.approval_queue.front() {
                app.overlay = Overlay::ApprovalDetail;
                vec![Effect::LoadApprovalDetail(approval.id)]
            } else {
                vec![]
            }
        }
    }
}

fn handle_key(app: &mut AppState, key: KeyEvent) -> Vec<Effect> {
    if key.modifiers.contains(KeyModifiers::CONTROL) && matches!(key.code, KeyCode::Char('q')) {
        app.should_quit = true;
        return vec![];
    }

    if !matches!(app.overlay, Overlay::None) {
        return handle_overlay_key(app, key);
    }

    match key.code {
        KeyCode::F(1) => app.overlay = Overlay::Help,
        KeyCode::Char('?') if app.composer.is_empty() => app.overlay = Overlay::Help,
        KeyCode::F(2) => open_session_picker(app),
        KeyCode::F(3) => app.show_inspector = !app.show_inspector,
        KeyCode::F(4) => {
            let selected = EXECUTION_MODE_ALL
                .iter()
                .position(|mode| *mode == app.mode)
                .unwrap_or(0);
            app.overlay = Overlay::Modes { selected };
        }
        KeyCode::F(5) => open_theme_picker(app),
        KeyCode::PageUp => scroll_up(app, 10),
        KeyCode::PageDown => scroll_down(app, 10),
        KeyCode::Home if app.focus == Focus::Timeline => scroll_timeline_home(app),
        KeyCode::End if app.focus == Focus::Timeline => {
            app.follow_tail = true;
            app.line_scroll_from_bottom = 0;
        }
        KeyCode::Up if key.modifiers.contains(KeyModifiers::ALT) => select_previous_item(app),
        KeyCode::Down if key.modifiers.contains(KeyModifiers::ALT) => select_next_item(app),
        KeyCode::BackTab => return cycle_execution_mode(app),
        KeyCode::Tab if key.modifiers.contains(KeyModifiers::SHIFT) => {
            return cycle_execution_mode(app);
        }
        KeyCode::Tab => cycle_focus(app),
        KeyCode::Enter if app.focus == Focus::Timeline => toggle_selected_item(app),
        KeyCode::Esc => {
            if !app.composer.is_empty() || app.pending_large_paste.is_some() {
                app.composer.clear();
                app.pending_large_paste = None;
            } else {
                app.focus = Focus::Composer;
            }
        }
        _ => return handle_composer_key(app, key),
    }
    app.dirty = true;
    vec![]
}

fn open_session_picker(app: &mut AppState) {
    let selected = app
        .active_session
        .and_then(|active| app.sessions.iter().position(|session| session.id == active))
        .unwrap_or(0);
    app.overlay = Overlay::Sessions {
        selected,
        query: String::new(),
    };
}

fn open_theme_picker(app: &mut AppState) {
    app.overlay = Overlay::Themes {
        selected: theme::theme_index(&app.theme_id),
    };
}

fn apply_theme(app: &mut AppState, id: &str) {
    app.set_theme_id(id);
    let meta = theme::theme_meta(&app.theme_id);
    let label = meta.map(|m| m.label).unwrap_or(app.theme_id.as_str());
    let blurb = meta.map(|m| m.blurb).unwrap_or("");
    app.show_toast(
        format!("Theme: {label} — {blurb} (`{}`)", app.theme_id),
        false,
    );
    app.overlay = Overlay::None;
}

fn scroll_timeline_home(app: &mut AppState) {
    app.follow_tail = false;
    app.line_scroll_from_bottom =
        max_scroll_from_bottom(app.timeline_line_count, app.timeline_viewport_rows);
    app.focus = Focus::Timeline;
}

fn handle_overlay_key(app: &mut AppState, key: KeyEvent) -> Vec<Effect> {
    if key.code == KeyCode::Esc {
        let return_to_approval = matches!(app.overlay, Overlay::ApprovalDetail);
        app.overlay = if return_to_approval {
            Overlay::Approval { selected: 0 }
        } else {
            Overlay::None
        };
        app.dirty = true;
        return vec![];
    }

    let overlay = std::mem::take(&mut app.overlay);
    let (new_overlay, effects) = match overlay {
        Overlay::Help | Overlay::Diagnostics { .. } | Overlay::Message { .. }
            if matches!(key.code, KeyCode::Enter | KeyCode::Char(' ')) =>
        {
            (Overlay::None, vec![])
        }
        Overlay::Sessions {
            mut selected,
            mut query,
        } => {
            let filtered_len = filtered_sessions(app, &query).len();
            match key.code {
                KeyCode::Up => selected = selected.saturating_sub(1),
                KeyCode::Down => {
                    selected = (selected + 1).min(filtered_len.saturating_sub(1));
                }
                KeyCode::Backspace => {
                    let _ = query.pop();
                    selected = 0;
                }
                KeyCode::Char('n') | KeyCode::Char('N')
                    if key.modifiers.contains(KeyModifiers::CONTROL)
                        || (query.is_empty()
                            && (key.modifiers.is_empty()
                                || key.modifiers == KeyModifiers::SHIFT)) =>
                {
                    return vec![Effect::CreateSession];
                }
                KeyCode::Char(ch)
                    if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT =>
                {
                    query.push(ch);
                    selected = 0;
                }
                KeyCode::Enter => {
                    let chosen = filtered_sessions(app, &query)
                        .get(selected)
                        .map(|session| session.id);
                    if let Some(session_id) = chosen {
                        return vec![Effect::ActivateSession(session_id)];
                    }
                }
                _ => {}
            }
            (Overlay::Sessions { selected, query }, vec![])
        }
        Overlay::Commands {
            mut selected,
            mut query,
        } => {
            let suggestions = command::suggestions(&query);
            match key.code {
                KeyCode::Up => selected = selected.saturating_sub(1),
                KeyCode::Down => {
                    selected = (selected + 1).min(suggestions.len().saturating_sub(1));
                }
                KeyCode::Backspace => {
                    let _ = query.pop();
                    selected = 0;
                }
                KeyCode::Char(ch)
                    if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT =>
                {
                    query.push(ch);
                    selected = 0;
                }
                KeyCode::Enter => {
                    let action = suggestions
                        .get(selected)
                        .and_then(|spec| command::parse_command(&format!("/{}", spec.name)));
                    app.overlay = Overlay::None;
                    app.dirty = true;
                    return action
                        .map(|action| execute_command(app, action))
                        .unwrap_or_default();
                }
                _ => {}
            }
            (Overlay::Commands { selected, query }, vec![])
        }
        Overlay::Modes { mut selected } => match key.code {
            KeyCode::Up => {
                selected = selected.saturating_sub(1);
                (Overlay::Modes { selected }, vec![])
            }
            KeyCode::Down => {
                selected = (selected + 1).min(EXECUTION_MODE_ALL.len() - 1);
                (Overlay::Modes { selected }, vec![])
            }
            KeyCode::Enter => {
                let mode = EXECUTION_MODE_ALL[selected];
                app.overlay = Overlay::None;
                app.dirty = true;
                return set_execution_mode_effects(app, mode);
            }
            _ => (Overlay::Modes { selected }, vec![]),
        },
        Overlay::Themes { mut selected } => match key.code {
            KeyCode::Up => {
                selected = selected.saturating_sub(1);
                (Overlay::Themes { selected }, vec![])
            }
            KeyCode::Down => {
                selected = (selected + 1).min(THEME_CATALOG.len().saturating_sub(1));
                (Overlay::Themes { selected }, vec![])
            }
            KeyCode::Enter => {
                let id = THEME_CATALOG[selected.min(THEME_CATALOG.len() - 1)].id;
                apply_theme(app, id);
                (Overlay::None, vec![])
            }
            _ => (Overlay::Themes { selected }, vec![]),
        },
        Overlay::Approval { mut selected } => match key.code {
            KeyCode::Up => {
                selected = selected.saturating_sub(1);
                (Overlay::Approval { selected }, vec![])
            }
            KeyCode::Down => {
                selected = (selected + 1).min(2);
                (Overlay::Approval { selected }, vec![])
            }
            KeyCode::Char('y') | KeyCode::Char('Y') => {
                let effect = app
                    .approval_queue
                    .front()
                    .map(|approval| Effect::ResolveApproval {
                        approval_id: approval.id,
                        accepted: true,
                    });
                (Overlay::Approval { selected }, effect.into_iter().collect())
            }
            KeyCode::Char('n') | KeyCode::Char('N') => {
                let effect = app
                    .approval_queue
                    .front()
                    .map(|approval| Effect::ResolveApproval {
                        approval_id: approval.id,
                        accepted: false,
                    });
                (Overlay::Approval { selected }, effect.into_iter().collect())
            }
            KeyCode::Char('d') | KeyCode::Char('D') => {
                let effect = app
                    .approval_queue
                    .front()
                    .map(|approval| Effect::LoadApprovalDetail(approval.id));
                (Overlay::ApprovalDetail, effect.into_iter().collect())
            }
            KeyCode::Enter => {
                let effect = app.approval_queue.front().map(|approval| match selected {
                    0 => Effect::ResolveApproval {
                        approval_id: approval.id,
                        accepted: true,
                    },
                    1 => Effect::ResolveApproval {
                        approval_id: approval.id,
                        accepted: false,
                    },
                    _ => Effect::LoadApprovalDetail(approval.id),
                });
                let next_overlay = if selected == 2 {
                    Overlay::ApprovalDetail
                } else {
                    Overlay::Approval { selected }
                };
                (next_overlay, effect.into_iter().collect())
            }
            _ => (Overlay::Approval { selected }, vec![]),
        },
        Overlay::ApprovalDetail => match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') => {
                let effect = app
                    .approval_queue
                    .front()
                    .map(|approval| Effect::ResolveApproval {
                        approval_id: approval.id,
                        accepted: true,
                    });
                (Overlay::ApprovalDetail, effect.into_iter().collect())
            }
            KeyCode::Char('n') | KeyCode::Char('N') => {
                let effect = app
                    .approval_queue
                    .front()
                    .map(|approval| Effect::ResolveApproval {
                        approval_id: approval.id,
                        accepted: false,
                    });
                (Overlay::ApprovalDetail, effect.into_iter().collect())
            }
            _ => (Overlay::ApprovalDetail, vec![]),
        },
        Overlay::LargePaste => match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') => match app.pending_large_paste.take() {
                None => (Overlay::None, vec![]),
                Some(body) if body.len() > MAX_PASTE_UPLOAD_BYTES => {
                    app.pending_large_paste = Some(body);
                    app.show_toast(
                        format!(
                            "Paste exceeds the {} byte upload limit.",
                            MAX_PASTE_UPLOAD_BYTES
                        ),
                        true,
                    );
                    (Overlay::LargePaste, vec![])
                }
                Some(body) => {
                    let label = format_paste_placeholder(body.len(), paste_line_count(&body));
                    app.composer.clear();
                    (
                        Overlay::None,
                        vec![send_large_paste_effect(app.prompt_intent, label, body)],
                    )
                }
            },
            KeyCode::Char('i') | KeyCode::Char('I') => {
                if let Some(text) = app.pending_large_paste.take() {
                    app.composer.clear();
                    app.composer.insert_str(&text);
                    app.show_toast("Large paste inserted into the composer for editing.", false);
                }
                (Overlay::None, vec![])
            }
            KeyCode::Char('n') | KeyCode::Char('N') => {
                app.pending_large_paste = None;
                app.composer.clear();
                app.show_toast("Large paste cancelled.", false);
                (Overlay::None, vec![])
            }
            _ => (Overlay::LargePaste, vec![]),
        },
        other => (other, vec![]),
    };

    app.overlay = new_overlay;
    app.dirty = true;
    effects
}

fn handle_composer_key(app: &mut AppState, key: KeyEvent) -> Vec<Effect> {
    if key.modifiers.contains(KeyModifiers::ALT) && key.code == KeyCode::Char('m') {
        app.composer.toggle_layout_mode();
        let mode = app.composer.layout_mode().label();
        app.show_toast(format!("Composer mode: {mode}"), false);
        app.focus = Focus::Composer;
        app.dirty = true;
        return vec![];
    }

    if key.modifiers.contains(KeyModifiers::CONTROL) {
        match key.code {
            KeyCode::Char('p') | KeyCode::Char('P')
                if key.modifiers.contains(KeyModifiers::SHIFT) =>
            {
                app.prompt_intent = cycle_prompt_intent(app.prompt_intent);
                app.show_toast(
                    format!(
                        "Composer intent: {} (Ctrl+Shift+P cycles · Ctrl+T steers)",
                        app.prompt_intent.label()
                    ),
                    false,
                );
            }
            KeyCode::Char('t') | KeyCode::Char('T')
                if key.modifiers.contains(KeyModifiers::SHIFT) =>
            {
                return execute_command(app, CommandAction::CycleTheme);
            }
            KeyCode::Char('p') => {
                app.overlay = Overlay::Commands {
                    selected: 0,
                    query: String::new(),
                };
            }
            KeyCode::Char('o') => open_session_picker(app),
            KeyCode::Char('t') => set_steer_intent(app),
            KeyCode::Char('c') => {
                if !app.composer.is_empty() || app.pending_large_paste.is_some() {
                    app.composer.clear();
                    app.pending_large_paste = None;
                } else if matches!(
                    app.run_state,
                    RunState::Working | RunState::WaitingApproval | RunState::Cancelling
                ) {
                    app.dirty = true;
                    return vec![Effect::Cancel];
                }
            }
            KeyCode::Char('l') => {
                app.clear_stream();
                app.timeline.clear();
                app.selected_item = None;
                app.show_toast(
                    "Local viewport cleared. Durable events remain in impetusd.",
                    false,
                );
            }
            KeyCode::Char('a') => app.composer.move_home(),
            KeyCode::Char('e') => app.composer.move_end(),
            KeyCode::Char('w') => app.composer.delete_previous_word(),
            KeyCode::Char('u') => app.composer.kill_to_line_start(),
            KeyCode::Char('k') => app.composer.kill_to_line_end(),
            KeyCode::Char('j') => app.composer.newline(),
            KeyCode::Char('d') => return show_selected_detail(app),
            _ => {}
        }
        app.dirty = true;
        return vec![];
    }

    match key.code {
        KeyCode::Enter
            if key.modifiers.contains(KeyModifiers::ALT)
                || key.modifiers.contains(KeyModifiers::SHIFT) =>
        {
            app.composer.newline();
        }
        KeyCode::Enter => {
            if app.composer.try_backslash_continuation() {
                app.focus = Focus::Composer;
                app.dirty = true;
                return vec![];
            }
            let Some(text) = app.composer.take_for_submit() else {
                return vec![];
            };
            if let Some(action) = command::parse_command(&text) {
                app.pending_large_paste = None;
                return execute_command(app, action);
            }
            if let Some(body) = app.pending_large_paste.take() {
                let label = if is_paste_placeholder(&text) {
                    format_paste_placeholder(body.len(), paste_line_count(&body))
                } else {
                    text
                };
                return vec![send_large_paste_effect(app.prompt_intent, label, body)];
            }
            if text.len() > MAX_PASTE_UPLOAD_BYTES {
                app.show_toast(
                    format!(
                        "Message is too large ({} bytes). Maximum upload size is {} bytes.",
                        text.len(),
                        MAX_PASTE_UPLOAD_BYTES
                    ),
                    true,
                );
                app.composer.insert_str(&text);
                app.dirty = true;
                return vec![];
            }
            if text.len() > LARGE_PASTE_BYTES {
                let label = format_paste_placeholder(text.len(), paste_line_count(&text));
                return vec![send_large_paste_effect(app.prompt_intent, label, text)];
            }
            return vec![send_effect(app.prompt_intent, text)];
        }
        KeyCode::Backspace => app.composer.backspace(),
        KeyCode::Delete => app.composer.delete(),
        KeyCode::Left if key.modifiers.contains(KeyModifiers::ALT) => app.composer.move_word_left(),
        KeyCode::Right if key.modifiers.contains(KeyModifiers::ALT) => {
            app.composer.move_word_right()
        }
        KeyCode::Left => app.composer.move_left(),
        KeyCode::Right => app.composer.move_right(),
        KeyCode::Home => app.composer.move_home(),
        KeyCode::End => app.composer.move_end(),
        KeyCode::Up => app.composer.history_previous(),
        KeyCode::Down => app.composer.history_next(),
        KeyCode::Char(ch) => app.composer.insert_char(ch),
        _ => {}
    }
    app.focus = Focus::Composer;
    app.dirty = true;
    vec![]
}

fn execute_command(app: &mut AppState, action: CommandAction) -> Vec<Effect> {
    match action {
        CommandAction::NewSession => vec![Effect::CreateSession],
        CommandAction::Resume(Some(session_id)) => vec![Effect::ActivateSession(session_id)],
        CommandAction::Resume(None) | CommandAction::Sessions => {
            open_session_picker(app);
            vec![]
        }
        CommandAction::ModePicker => {
            let selected = EXECUTION_MODE_ALL
                .iter()
                .position(|mode| *mode == app.mode)
                .unwrap_or(0);
            app.overlay = Overlay::Modes { selected };
            vec![]
        }
        CommandAction::SetMode(mode) => set_execution_mode_effects(app, mode),
        CommandAction::SetPromptIntent(intent) => {
            app.prompt_intent = intent;
            app.show_toast(
                format!(
                    "Composer intent: {} (/prompt · /steer · /follow-up)",
                    intent.label()
                ),
                false,
            );
            vec![]
        }
        CommandAction::ShowDiff => show_selected_detail(app),
        CommandAction::ToggleInspector => {
            app.show_inspector = !app.show_inspector;
            vec![]
        }
        CommandAction::Status => {
            app.overlay = Overlay::Message {
                title: " status ".to_owned(),
                body: status_body(app),
                error: false,
            };
            vec![]
        }
        CommandAction::Diagnostics => vec![Effect::Diagnostics],
        CommandAction::ListChildren => vec![Effect::ListChildren],
        CommandAction::Cancel => vec![Effect::Cancel],
        CommandAction::ClearViewport => {
            app.clear_stream();
            app.timeline.clear();
            app.selected_item = None;
            app.show_toast(
                "Local viewport cleared. Durable events remain in impetusd.",
                false,
            );
            vec![]
        }
        CommandAction::Help => {
            app.overlay = Overlay::Help;
            vec![]
        }
        CommandAction::ThemePicker => {
            open_theme_picker(app);
            vec![]
        }
        CommandAction::SetTheme(id) => {
            apply_theme(app, &id);
            vec![]
        }
        CommandAction::CycleTheme => {
            app.cycle_theme();
            let meta = theme::theme_meta(&app.theme_id);
            let label = meta.map(|m| m.label).unwrap_or(app.theme_id.as_str());
            app.show_toast(
                format!(
                    "Theme: {label} (`{}`) · /theme or Ctrl+Shift+T",
                    app.theme_id
                ),
                false,
            );
            vec![]
        }
        CommandAction::Quit => {
            app.should_quit = true;
            vec![]
        }
        CommandAction::Unknown(message) => {
            app.show_toast(message, true);
            vec![]
        }
    }
}

fn send_effect(intent: impetus_client::protocol::UserPromptIntent, text: String) -> Effect {
    Effect::SendMessage { text, intent }
}

fn send_large_paste_effect(
    intent: impetus_client::protocol::UserPromptIntent,
    label: String,
    body: String,
) -> Effect {
    Effect::SendLargePaste {
        label,
        body,
        intent,
    }
}

fn set_execution_mode_effects(app: &mut AppState, mode: ExecutionMode) -> Vec<Effect> {
    if app.active_session.is_none() {
        app.show_toast("No active session. Create or resume one first.", true);
        return vec![];
    }
    if !execution_mode_is_available(mode, &app.connection.capabilities) {
        app.show_toast(
            format!(
                "{} is not supported by the current daemon contract.",
                mode.label()
            ),
            true,
        );
        return vec![];
    }
    vec![Effect::SetExecutionMode { mode }]
}

fn cycle_execution_mode(app: &mut AppState) -> Vec<Effect> {
    app.dirty = true;
    if app.active_session.is_none() {
        app.show_toast("No active session. Create or resume one first.", true);
        return vec![];
    }
    set_execution_mode_effects(app, app.mode.cycle_next())
}

fn show_selected_detail(app: &mut AppState) -> Vec<Effect> {
    if let Some(approval) = app.approval_queue.front() {
        app.overlay = Overlay::ApprovalDetail;
        return vec![Effect::LoadApprovalDetail(approval.id)];
    }
    if let Some(item) = app.selected_item.and_then(|index| app.timeline.get(index)) {
        let body = if crate::diff::looks_like_diff(&item.body) || item.details.is_empty() {
            item.body.clone()
        } else {
            item.details.clone()
        };
        let title = if crate::diff::looks_like_diff(&body) {
            format!(" diff · {} · event {} ", item.title, item.sequence)
        } else {
            format!(" {} · event {} ", item.title, item.sequence)
        };
        app.overlay = Overlay::Message {
            title,
            body,
            error: item.kind == ItemKind::Error,
        };
    } else {
        app.show_toast("Select an event with Alt+Up/Down first.", true);
    }
    vec![]
}

fn status_body(app: &AppState) -> String {
    format!(
        "# Session status\n\n- **Backend:** {}\n- **IPC:** v{}\n- **Session:** {}\n- **Run:** {}\n- **Mode:** {}\n- **Events rendered:** {}\n- **Last sequence:** {}\n- **Tokens used:** {}\n- **Context:** {}%\n- **Turns:** {}\n- **Compactions:** {}\n\nThe client owns only this projection. Durable history, policy and execution remain in `impetusd`.",
        app.connection.label,
        app.connection.protocol_version,
        app.active_session
            .map(|id| id.to_string())
            .unwrap_or_else(|| "none".to_owned()),
        app.run_state.label(),
        app.mode.label(),
        app.timeline.len(),
        app.last_sequence,
        app.budget.tokens_used,
        app.budget.context_used_percent,
        app.budget.turns_used,
        app.budget.compactions,
    )
}

fn ingest_event(app: &mut AppState, event: UiEvent) {
    if event.sequence <= app.last_sequence {
        return;
    }
    let sequence = event.sequence;
    let at = event.at_unix_ms;
    match event.kind {
        UiEventKind::SessionCreated => app.push_item(
            TimelineItem::new(sequence, at, ItemKind::Notice, "session created")
                .with_body("Durable session created by impetusd."),
        ),
        UiEventKind::SessionWorkspace { workspace } => {
            if let Some(active) = app.active_session
                && let Some(session) = app.sessions.iter_mut().find(|session| session.id == active)
            {
                session.workspace = Some(workspace.clone());
            }
            app.push_item(
                TimelineItem::new(sequence, at, ItemKind::Notice, "workspace")
                    .with_body(workspace.clone())
                    .with_details(format!("workspace_root: {workspace}")),
            );
        }
        UiEventKind::SessionAttached => {
            app.status_message = "attached".to_owned();
            app.last_sequence = sequence;
        }
        UiEventKind::ExecutionModeChanged { mode } => {
            app.mode = mode;
            app.push_item(
                TimelineItem::new(sequence, at, ItemKind::Notice, "execution mode")
                    .with_body(format!("daemon mode set to {}", mode.label())),
            );
        }
        UiEventKind::UserInput { text } => app.push_item(
            TimelineItem::new(sequence, at, ItemKind::User, "you")
                .with_body(strip_mode_prefix(&text)),
        ),
        UiEventKind::Plan { summary } => app
            .push_item(TimelineItem::new(sequence, at, ItemKind::Plan, "plan").with_body(summary)),
        UiEventKind::RunStarted { run_id } => {
            app.run_state = RunState::Working;
            set_active_session_status(app, "working");
            app.status_message = format!("run {}", short(run_id));
            app.push_item(
                TimelineItem::new(sequence, at, ItemKind::Notice, "run started")
                    .with_body(format!("run_id: {run_id}")),
            );
        }
        UiEventKind::RunCompleted { run_id } => {
            app.flush_stream_to_timeline();
            app.run_state = RunState::Idle;
            set_active_session_status(app, "ready");
            app.status_message = "complete".to_owned();
            app.push_item(
                TimelineItem::new(sequence, at, ItemKind::Notice, "run completed")
                    .with_body(format!("run_id: {run_id}")),
            );
        }
        UiEventKind::RunFailed { run_id, reason } => {
            app.flush_stream_to_timeline();
            app.run_state = RunState::Failed;
            set_active_session_status(app, "failed");
            app.status_message = "failed".to_owned();
            let hint = crate::model::remediation_hint("run failed", None);
            push_or_coalesce_noise(
                app,
                TimelineItem::new(sequence, at, ItemKind::Error, "run failed")
                    .with_body(format!("{reason}\n→ {hint}"))
                    .with_details(format!("run_id: {run_id}\nremediation: {hint}")),
            );
            app.show_toast(format!("run failed → {hint}"), true);
        }
        UiEventKind::RunCancelled { run_id } => {
            app.flush_stream_to_timeline();
            app.run_state = RunState::Idle;
            set_active_session_status(app, "cancelled");
            app.status_message = "cancelled".to_owned();
            app.push_item(
                TimelineItem::new(sequence, at, ItemKind::Notice, "run cancelled")
                    .with_body(format!("run_id: {run_id}")),
            );
        }
        UiEventKind::RunUnknown { run_id } => {
            app.flush_stream_to_timeline();
            app.run_state = RunState::Unknown;
            set_active_session_status(app, "unknown");
            app.status_message = "unknown outcome".to_owned();
            let hint = crate::model::remediation_hint("unknown outcome", None);
            push_or_coalesce_noise(
                app,
                TimelineItem::new(sequence, at, ItemKind::Error, "unknown outcome")
                    .with_body(format!(
                        "The client disconnected before the daemon could prove completion. Do not retry non-replayable work automatically.\n→ {hint}"
                    ))
                    .with_details(format!("run_id: {run_id}\nremediation: {hint}")),
            );
            app.show_toast(format!("unknown outcome → {hint}"), true);
        }
        UiEventKind::AgentChunk {
            run_id,
            chunk_id,
            text,
        } => {
            ingest_stream_chunk(app, run_id, sequence, at, chunk_id, text);
        }
        UiEventKind::AgentFinal { run_id, text } => {
            let key = run_id.to_string();
            if app.stream_run_id == Some(run_id) {
                let pending = app.stream_buffer.flush();
                crate::model::append_stream_body(app, &key, &pending);
                app.stream_run_id = None;
            }
            if let Some(item) = app
                .timeline
                .iter_mut()
                .rev()
                .find(|item| item.streaming_key.as_deref() == Some(key.as_str()))
            {
                item.body = bounded(text, crate::model::MAX_BODY_CHARS);
                item.streaming_key = None;
                item.sequence = sequence;
                item.at_unix_ms = at;
                item.details = format!("run_id: {run_id}\nfinal: true");
                app.last_sequence = sequence;
                app.dirty = true;
            } else {
                app.push_item(
                    TimelineItem::new(sequence, at, ItemKind::Assistant, "assistant")
                        .with_body(text)
                        .with_details(format!("run_id: {run_id}\nfinal: true")),
                );
            }
        }
        UiEventKind::ToolStarted { name } => app.push_item(
            TimelineItem::new(sequence, at, ItemKind::Tool, format!("tool · {name}"))
                .with_body("running…")
                .collapsed(),
        ),
        UiEventKind::ToolFinished { name, summary } => app.push_item(
            TimelineItem::new(sequence, at, ItemKind::Tool, format!("tool · {name}"))
                .with_body(summary)
                .collapsed(),
        ),
        UiEventKind::ToolObserved {
            call_id,
            name,
            arguments,
            outcome,
            preview,
            artifact,
            error,
        } => {
            let kind = if error.is_some() || outcome.to_ascii_lowercase().contains("error") {
                ItemKind::Error
            } else {
                ItemKind::Tool
            };
            let mut details =
                format!("call_id: {call_id}\noutcome: {outcome}\narguments:\n{arguments}");
            if let Some(artifact) = artifact {
                details.push_str(&format!("\nartifact: {artifact}"));
            }
            if let Some(error) = error {
                details.push_str(&format!("\nerror: {error}"));
            }
            let mut item = TimelineItem::new(sequence, at, kind, format!("tool · {name}"))
                .with_body(preview)
                .with_details(details);
            if kind == ItemKind::Tool && !crate::diff::looks_like_diff(&item.body) {
                item = item.collapsed();
            }
            app.push_item(item);
        }
        UiEventKind::ToolDeferred {
            approval_id,
            call_id,
            name,
            arguments,
        } => app.push_item(
            TimelineItem::new(
                sequence,
                at,
                ItemKind::Approval,
                format!("deferred · {name}"),
            )
            .with_body("Waiting for an exact user approval.")
            .with_details(format!(
                "approval_id: {approval_id}\ncall_id: {call_id}\narguments:\n{arguments}"
            )),
        ),
        UiEventKind::ApprovalRequested { approval } => {
            app.run_state = RunState::WaitingApproval;
            app.push_item(
                TimelineItem::new(sequence, at, ItemKind::Approval, "approval requested")
                    .with_body(format!(
                        "{}\n{}",
                        approval.summary,
                        approval.target.clone().unwrap_or_default()
                    ))
                    .with_details(format!(
                        "id: {}\nkind: {}\nreason: {}\nfingerprint: {}",
                        approval.id, approval.action_kind, approval.reason, approval.fingerprint
                    )),
            );
            if !app
                .approval_queue
                .iter()
                .any(|pending| pending.id == approval.id)
            {
                app.approval_queue.push_back(approval);
            }
            if matches!(app.overlay, Overlay::None) {
                app.overlay = Overlay::Approval { selected: 0 };
            }
        }
        UiEventKind::ApprovalResolved {
            approval_id,
            accepted,
        } => {
            app.approval_queue
                .retain(|approval| approval.id != approval_id);
            app.push_item(
                TimelineItem::new(sequence, at, ItemKind::Approval, "approval resolved")
                    .with_body(if accepted {
                        "approved once"
                    } else {
                        "rejected"
                    })
                    .with_details(format!("approval_id: {approval_id}")),
            );
            if app.approval_queue.is_empty() {
                app.run_state = RunState::Working;
            }
        }
        UiEventKind::Backend {
            title,
            detail,
            healthy,
        } => {
            let kind = if healthy {
                ItemKind::Notice
            } else {
                ItemKind::Error
            };
            if healthy {
                push_or_coalesce_noise(
                    app,
                    TimelineItem::new(sequence, at, kind, title).with_body(detail),
                );
            } else {
                let hint = crate::model::remediation_hint(&title, None);
                push_or_coalesce_noise(
                    app,
                    TimelineItem::new(sequence, at, kind, title.clone())
                        .with_body(format!("{detail}\n→ {hint}"))
                        .with_details(format!("remediation: {hint}")),
                );
                app.show_toast(format!("{title} → {hint}"), true);
            }
        }
        UiEventKind::BudgetUpdated(budget) => {
            app.budget = budget;
            app.last_sequence = sequence;
            app.dirty = true;
        }
        UiEventKind::BudgetWarning { message } => {
            app.budget.warning = Some(message.clone());
            push_or_coalesce_noise(
                app,
                TimelineItem::new(sequence, at, ItemKind::Budget, "budget warning")
                    .with_body(message),
            );
        }
        UiEventKind::Notice {
            title,
            message,
            error,
            remediation,
        } => {
            let kind = if error {
                ItemKind::Error
            } else {
                ItemKind::Notice
            };
            let mut item = TimelineItem::new(sequence, at, kind, title.clone()).with_body(message);
            if error {
                let hint = crate::model::remediation_hint(&title, remediation.as_deref());
                item.body = format!("{}\n→ {hint}", item.body);
                item.details = format!("remediation: {hint}");
                app.show_toast(format!("{title} → {hint}"), true);
            }
            push_or_coalesce_noise(app, item);
        }
        UiEventKind::Retry {
            title,
            message,
            failed,
        } => {
            let kind = if failed {
                ItemKind::Error
            } else {
                ItemKind::Notice
            };
            let mut item = TimelineItem::new(sequence, at, kind, title.clone()).with_body(message);
            if failed {
                let hint = crate::model::remediation_hint(&title, None);
                item.body = format!("{}\n→ {hint}", item.body);
                item.details = format!("remediation: {hint}");
                app.show_toast(format!("{title} → {hint}"), true);
            }
            push_or_coalesce_noise(app, item);
        }
    }
}

fn set_active_session_status(app: &mut AppState, status: &str) {
    if let Some(active) = app.active_session
        && let Some(session) = app.sessions.iter_mut().find(|session| session.id == active)
    {
        session.status = status.to_owned();
    }
}

/// Collapse consecutive same-title Notice/Error/Budget noise into one timeline row.
fn push_or_coalesce_noise(app: &mut AppState, item: TimelineItem) {
    let coalesce = matches!(
        item.kind,
        ItemKind::Notice | ItemKind::Error | ItemKind::Budget
    );
    if coalesce
        && let Some(last) = app.timeline.back_mut()
        && last.kind == item.kind
        && last.title == item.title
        && last.streaming_key.is_none()
    {
        last.sequence = item.sequence;
        last.at_unix_ms = item.at_unix_ms;
        last.body = item.body;
        last.details = item.details;
        app.last_sequence = app.last_sequence.max(last.sequence);
        if app.follow_tail {
            app.line_scroll_from_bottom = 0;
            app.selected_item = app.timeline.len().checked_sub(1);
        }
        app.dirty = true;
        return;
    }
    app.push_item(item);
}

fn strip_mode_prefix(text: &str) -> String {
    if text.starts_with("[Impetus UI mode:") {
        text.split_once("\n\n")
            .map(|(_, body)| body.to_owned())
            .unwrap_or_else(|| text.to_owned())
    } else {
        text.to_owned()
    }
}

fn scroll_up(app: &mut AppState, lines: usize) {
    app.follow_tail = false;
    app.line_scroll_from_bottom = app.line_scroll_from_bottom.saturating_add(lines);
    app.clamp_timeline_scroll();
    app.focus = Focus::Timeline;
    app.dirty = true;
}

fn scroll_down(app: &mut AppState, lines: usize) {
    app.line_scroll_from_bottom = app.line_scroll_from_bottom.saturating_sub(lines);
    if app.line_scroll_from_bottom == 0 {
        app.follow_tail = true;
    }
    app.focus = Focus::Timeline;
    app.dirty = true;
}

fn select_previous_item(app: &mut AppState) {
    app.selected_item = match app.selected_item {
        Some(index) => Some(index.saturating_sub(1)),
        None => app.timeline.len().checked_sub(1),
    };
    app.focus = Focus::Timeline;
}

fn select_next_item(app: &mut AppState) {
    if app.timeline.is_empty() {
        app.selected_item = None;
    } else {
        app.selected_item = Some(
            app.selected_item
                .map(|index| (index + 1).min(app.timeline.len() - 1))
                .unwrap_or(0),
        );
    }
    app.focus = Focus::Timeline;
}

fn toggle_selected_item(app: &mut AppState) {
    if let Some(item) = app
        .selected_item
        .and_then(|index| app.timeline.get_mut(index))
    {
        item.collapsed = !item.collapsed;
    }
}

fn set_steer_intent(app: &mut AppState) {
    if matches!(
        app.run_state,
        RunState::Working | RunState::WaitingApproval | RunState::Cancelling
    ) {
        app.prompt_intent = impetus_client::protocol::UserPromptIntent::Steer;
        app.show_toast(
            "Composer intent: steer (active run — submit to nudge)",
            false,
        );
    } else {
        app.show_toast(
            "Steer needs an active run. Ctrl+Shift+P cycles intent, or type /steer.",
            true,
        );
    }
}

fn cycle_focus(app: &mut AppState) {
    app.focus = match app.focus {
        Focus::Composer => Focus::Timeline,
        Focus::Timeline if app.show_inspector => Focus::Inspector,
        Focus::Timeline => Focus::Composer,
        Focus::Inspector => Focus::Composer,
    };
}

fn short(id: Uuid) -> String {
    id.to_string().chars().take(8).collect()
}

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
                    affected_files: vec!["crates/impetus-tui/src/app.rs".to_owned()],
                    estimated_scope: Some("Lines(2)".to_owned()),
                    attachment_refs: vec![],
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
        assert!(suggestions.len() >= 3);
        assert_eq!(suggestions[0].name, "children");
        assert_eq!(suggestions[1].name, "clear");
        assert_eq!(suggestions[2].name, "cancel");

        let _ = handle_key(&mut app, KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        let _ = handle_key(&mut app, KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        match &app.overlay {
            Overlay::Commands { selected, query } => {
                assert_eq!(query, "c");
                assert_eq!(*selected, 2);
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
        assert!(matches!(effects.as_slice(), [Effect::CreateSession]));

        open_session_picker(&mut app);
        let effects = handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('n'), KeyModifiers::CONTROL),
        );
        assert!(matches!(effects.as_slice(), [Effect::CreateSession]));
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
}
