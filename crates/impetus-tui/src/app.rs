use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use crossterm::event::{
    Event as TerminalEvent, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers,
    MouseEventKind,
};
use futures_util::StreamExt;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use uuid::Uuid;

use crate::backend::UiBackend;
use crate::command::{self, CommandAction};
use crate::model::{
    AppState, ExecutionMode, Focus, ItemKind, LARGE_PASTE_BYTES, MAX_DIRECT_PROMPT_BYTES, Overlay,
    RunOptions, RunState, SessionSummary, TimelineItem, UiEvent, UiEventKind, bounded,
};
use crate::render::{filtered_sessions, render};
use crate::terminal::TerminalSession;
use crate::theme::Theme;

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

    terminal.draw(|frame| render(frame, &app, Theme::default()))?;
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
                app.expire_transients();
            }
        }

        if app.dirty && !app.should_quit && last_draw.elapsed() >= options.tick_rate {
            terminal.draw(|frame| render(frame, &app, Theme::default()))?;
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
}

#[derive(Debug)]
enum Effect {
    RefreshSessions,
    CreateSession,
    ActivateSession(Uuid),
    SendMessage(String),
    Cancel,
    ResolveApproval { approval_id: Uuid, accepted: bool },
    LoadApprovalDetail(Uuid),
    Diagnostics,
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
        Effect::SendMessage(text) => {
            let Some(session_id) = app.active_session else {
                app.show_toast("No active session. Create or resume one first.", true);
                return;
            };
            app.run_state = RunState::Working;
            app.status_message = "submitting task".to_owned();
            app.dirty = true;
            let generation = app.subscription_generation;
            let backend = backend.clone();
            let tx = tx.clone();
            spawn_detached(async move {
                let result = backend
                    .send_message(session_id, text)
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
                    app.status_message = format!("cancel · {status}");
                    app.show_toast("Cancellation forwarded to the daemon.", false);
                }
                Err(error) => {
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
            if text.len() > LARGE_PASTE_BYTES {
                app.pending_large_paste = Some(text);
                app.overlay = Overlay::LargePaste;
            } else {
                app.composer.insert_str(&text);
            }
            app.dirty = true;
            vec![]
        }
        TerminalEvent::Mouse(mouse) => {
            match mouse.kind {
                MouseEventKind::ScrollUp => scroll_up(app, 4),
                MouseEventKind::ScrollDown => scroll_down(app, 4),
                _ => {}
            }
            vec![]
        }
        TerminalEvent::Resize(_, _) | TerminalEvent::FocusGained | TerminalEvent::FocusLost => {
            app.dirty = true;
            vec![]
        }
        _ => vec![],
    }
}

fn handle_key(app: &mut AppState, key: KeyEvent) -> Vec<Effect> {
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('q') {
        app.should_quit = true;
        return vec![];
    }

    if !matches!(app.overlay, Overlay::None) {
        return handle_overlay_key(app, key);
    }

    match key.code {
        KeyCode::F(1) => app.overlay = Overlay::Help,
        KeyCode::F(2) => {
            let selected = app
                .active_session
                .and_then(|active| app.sessions.iter().position(|session| session.id == active))
                .unwrap_or(0);
            app.overlay = Overlay::Sessions {
                selected,
                query: String::new(),
            };
        }
        KeyCode::F(3) => app.show_inspector = !app.show_inspector,
        KeyCode::F(4) => {
            let selected = ExecutionMode::ALL
                .iter()
                .position(|mode| *mode == app.mode)
                .unwrap_or(0);
            app.overlay = Overlay::Modes { selected };
        }
        KeyCode::PageUp => scroll_up(app, 10),
        KeyCode::PageDown => scroll_down(app, 10),
        KeyCode::End if app.focus == Focus::Timeline => {
            app.follow_tail = true;
            app.line_scroll_from_bottom = 0;
        }
        KeyCode::Up if key.modifiers.contains(KeyModifiers::ALT) => select_previous_item(app),
        KeyCode::Down if key.modifiers.contains(KeyModifiers::ALT) => select_next_item(app),
        KeyCode::Tab => cycle_focus(app),
        KeyCode::Enter if app.focus == Focus::Timeline => toggle_selected_item(app),
        KeyCode::Esc => {
            if !app.composer.is_empty() {
                app.composer.clear();
            } else {
                app.focus = Focus::Composer;
            }
        }
        _ => return handle_composer_key(app, key),
    }
    app.dirty = true;
    vec![]
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
                KeyCode::Char('n') if key.modifiers.contains(KeyModifiers::CONTROL) => {
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
                selected = (selected + 1).min(ExecutionMode::ALL.len() - 1);
                (Overlay::Modes { selected }, vec![])
            }
            KeyCode::Enter => {
                let mode = ExecutionMode::ALL[selected];
                if mode.is_available(&app.connection.capabilities) {
                    app.mode = mode;
                    app.show_toast(format!("Execution mode: {}", mode.label()), false);
                    (Overlay::None, vec![])
                } else {
                    app.show_toast(
                        format!(
                            "{} is locked until impetusd exposes a durable scoped-grant capability.",
                            mode.label()
                        ),
                        true,
                    );
                    (Overlay::Modes { selected }, vec![])
                }
            }
            _ => (Overlay::Modes { selected }, vec![]),
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
            KeyCode::Char('y') | KeyCode::Char('Y') => {
                let byte_len = app
                    .pending_large_paste
                    .as_ref()
                    .map(String::len)
                    .unwrap_or_default();
                if byte_len > MAX_DIRECT_PROMPT_BYTES {
                    app.show_toast(
                        "Paste exceeds the safe direct-IPC budget. Insert it into the composer and trim it, or wait for ArtifactStore upload support.",
                        true,
                    );
                    (Overlay::LargePaste, vec![])
                } else {
                    let text = app.pending_large_paste.take();
                    let effects = text
                        .map(|text| vec![send_effect(app.mode, text)])
                        .unwrap_or_default();
                    (Overlay::None, effects)
                }
            }
            KeyCode::Char('i') | KeyCode::Char('I') => {
                if let Some(text) = app.pending_large_paste.take() {
                    app.composer.insert_str(&text);
                    app.show_toast("Large paste inserted into the composer for editing.", false);
                }
                (Overlay::None, vec![])
            }
            KeyCode::Char('n') | KeyCode::Char('N') => {
                app.pending_large_paste = None;
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
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        match key.code {
            KeyCode::Char('p') => {
                app.overlay = Overlay::Commands {
                    selected: 0,
                    query: String::new(),
                };
            }
            KeyCode::Char('c') => {
                if !app.composer.is_empty() {
                    app.composer.clear();
                } else if matches!(
                    app.run_state,
                    RunState::Working | RunState::WaitingApproval | RunState::Cancelling
                ) {
                    app.dirty = true;
                    return vec![Effect::Cancel];
                }
            }
            KeyCode::Char('l') => {
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
        KeyCode::Enter if key.modifiers.contains(KeyModifiers::ALT) => app.composer.newline(),
        KeyCode::Enter => {
            let Some(text) = app.composer.take_for_submit() else {
                return vec![];
            };
            if let Some(action) = command::parse_command(&text) {
                return execute_command(app, action);
            }
            if text.len() > LARGE_PASTE_BYTES {
                app.pending_large_paste = Some(text);
                app.overlay = Overlay::LargePaste;
                app.dirty = true;
                return vec![];
            }
            return vec![send_effect(app.mode, text)];
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
            let selected = app
                .active_session
                .and_then(|active| app.sessions.iter().position(|session| session.id == active))
                .unwrap_or(0);
            app.overlay = Overlay::Sessions {
                selected,
                query: String::new(),
            };
            vec![]
        }
        CommandAction::ModePicker => {
            let selected = ExecutionMode::ALL
                .iter()
                .position(|mode| *mode == app.mode)
                .unwrap_or(0);
            app.overlay = Overlay::Modes { selected };
            vec![]
        }
        CommandAction::SetMode(mode) => {
            if mode.is_available(&app.connection.capabilities) {
                app.mode = mode;
                app.show_toast(format!("Execution mode: {}", mode.label()), false);
            } else {
                app.show_toast(
                    format!(
                        "{} is not supported by the current daemon contract.",
                        mode.label()
                    ),
                    true,
                );
            }
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
        CommandAction::Cancel => vec![Effect::Cancel],
        CommandAction::ClearViewport => {
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

fn send_effect(mode: ExecutionMode, text: String) -> Effect {
    let payload = mode
        .prompt_prefix()
        .map(|prefix| format!("{prefix}{text}"))
        .unwrap_or(text);
    Effect::SendMessage(payload)
}

fn show_selected_detail(app: &mut AppState) -> Vec<Effect> {
    if let Some(approval) = app.approval_queue.front() {
        app.overlay = Overlay::ApprovalDetail;
        return vec![Effect::LoadApprovalDetail(approval.id)];
    }
    if let Some(item) = app.selected_item.and_then(|index| app.timeline.get(index)) {
        app.overlay = Overlay::Message {
            title: format!(" {} · event {} ", item.title, item.sequence),
            body: if item.details.is_empty() {
                item.body.clone()
            } else {
                item.details.clone()
            },
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
            app.run_state = RunState::Idle;
            set_active_session_status(app, "ready");
            app.status_message = "complete".to_owned();
            app.push_item(
                TimelineItem::new(sequence, at, ItemKind::Notice, "run completed")
                    .with_body(format!("run_id: {run_id}")),
            );
        }
        UiEventKind::RunFailed { run_id, reason } => {
            app.run_state = RunState::Failed;
            set_active_session_status(app, "failed");
            app.status_message = "failed".to_owned();
            app.push_item(
                TimelineItem::new(sequence, at, ItemKind::Error, "run failed")
                    .with_body(reason)
                    .with_details(format!("run_id: {run_id}")),
            );
        }
        UiEventKind::RunCancelled { run_id } => {
            app.run_state = RunState::Idle;
            set_active_session_status(app, "cancelled");
            app.status_message = "cancelled".to_owned();
            app.push_item(
                TimelineItem::new(sequence, at, ItemKind::Notice, "run cancelled")
                    .with_body(format!("run_id: {run_id}")),
            );
        }
        UiEventKind::RunUnknown { run_id } => {
            app.run_state = RunState::Unknown;
            set_active_session_status(app, "unknown");
            app.status_message = "unknown outcome".to_owned();
            app.push_item(
                TimelineItem::new(sequence, at, ItemKind::Error, "unknown outcome")
                    .with_body("The client disconnected before the daemon could prove completion. Do not retry non-replayable work automatically.")
                    .with_details(format!("run_id: {run_id}")),
            );
        }
        UiEventKind::AgentChunk {
            run_id,
            chunk_id,
            text,
        } => {
            let key = run_id.to_string();
            if let Some(item) = app
                .timeline
                .iter_mut()
                .rev()
                .find(|item| item.streaming_key.as_deref() == Some(key.as_str()))
            {
                item.body.push_str(&text);
                item.body = bounded(std::mem::take(&mut item.body), crate::model::MAX_BODY_CHARS);
                item.sequence = sequence;
                item.at_unix_ms = at;
                item.details = format!("run_id: {run_id}\nlast_chunk_id: {chunk_id}");
                app.last_sequence = sequence;
                app.dirty = true;
            } else {
                let mut item = TimelineItem::new(sequence, at, ItemKind::Assistant, "assistant")
                    .with_body(text)
                    .with_details(format!("run_id: {run_id}\nchunk_id: {chunk_id}"));
                item.streaming_key = Some(key);
                app.push_item(item);
            }
        }
        UiEventKind::AgentFinal { run_id, text } => {
            let key = run_id.to_string();
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
            if kind == ItemKind::Tool {
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
        } => app.push_item(
            TimelineItem::new(
                sequence,
                at,
                if healthy {
                    ItemKind::Notice
                } else {
                    ItemKind::Error
                },
                title,
            )
            .with_body(detail),
        ),
        UiEventKind::BudgetUpdated(budget) => {
            app.budget = budget;
            app.last_sequence = sequence;
            app.dirty = true;
        }
        UiEventKind::BudgetWarning { message } => {
            app.budget.warning = Some(message.clone());
            app.push_item(
                TimelineItem::new(sequence, at, ItemKind::Budget, "budget warning")
                    .with_body(message),
            );
        }
        UiEventKind::Notice {
            title,
            message,
            error,
        } => app.push_item(
            TimelineItem::new(
                sequence,
                at,
                if error {
                    ItemKind::Error
                } else {
                    ItemKind::Notice
                },
                title,
            )
            .with_body(message),
        ),
        UiEventKind::Retry {
            title,
            message,
            failed,
        } => app.push_item(
            TimelineItem::new(
                sequence,
                at,
                if failed {
                    ItemKind::Error
                } else {
                    ItemKind::Notice
                },
                title,
            )
            .with_body(message),
        ),
    }
}

fn set_active_session_status(app: &mut AppState, status: &str) {
    if let Some(active) = app.active_session
        && let Some(session) = app.sessions.iter_mut().find(|session| session.id == active)
    {
        session.status = status.to_owned();
    }
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
    use crate::model::{ConnectionInfo, UiEvent};

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
        assert_eq!(app.timeline[0].body, "hello world");
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
            },
        };
        ingest_event(&mut app, event.clone());
        ingest_event(&mut app, event);
        assert_eq!(app.timeline.len(), 1);
    }
}
