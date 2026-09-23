//! Keyboard, mouse, overlay, and composer handlers.

use std::path::PathBuf;
use std::time::Instant;

use crossterm::event::{
    Event as TerminalEvent, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEvent,
    MouseEventKind,
};

use crate::catalog::{
    self, ModelPickerState, ModelPickerStep, filter_by_query, merge_option_choice,
    models_for_provider, option_choices, provider_choices, row_is_selectable,
};
use crate::command::{self, CommandAction};
use crate::hit::{HitKind, PointerClick, cycle_prompt_intent, is_double_click, resolve_hit};
use crate::model::{
    AppState, EXECUTION_MODE_ALL, ExecutionMode, FilesFocus, FilesOverlayState, Focus, ItemKind,
    LARGE_PASTE_BYTES, MAX_PASTE_UPLOAD_BYTES, Overlay, ReviewFocus, ReviewOverlayState, RunState,
    TextPromptKind, execution_mode_is_available, format_paste_placeholder, is_attach_placeholder,
    is_paste_placeholder, max_scroll_from_bottom, normalize_paste, paste_line_count,
};
use crate::render::{filtered_branches, filtered_sessions};
use crate::theme::{self, THEME_CATALOG};

use super::effects::Effect;

pub(super) fn handle_terminal_event(app: &mut AppState, event: TerminalEvent) -> Vec<Effect> {
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

pub(super) fn handle_mouse(app: &mut AppState, mouse: MouseEvent) -> Vec<Effect> {
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

pub(super) fn apply_hit(app: &mut AppState, kind: HitKind, double: bool) -> Vec<Effect> {
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

pub(super) fn handle_key(app: &mut AppState, key: KeyEvent) -> Vec<Effect> {
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
        KeyCode::F(6) => return open_review_overlay(app),
        KeyCode::F(7) => {
            app.dirty = true;
            return vec![Effect::LoadCheckpoints];
        }
        KeyCode::F(8) => return vec![Effect::OpenModelPicker],
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
            if !app.composer.is_empty()
                || app.pending_large_paste.is_some()
                || app.pending_artifact.is_some()
            {
                app.composer.clear();
                app.pending_large_paste = None;
                app.pending_artifact = None;
            } else {
                app.focus = Focus::Composer;
            }
        }
        _ => return handle_composer_key(app, key),
    }
    app.dirty = true;
    vec![]
}

pub(super) fn open_session_picker(app: &mut AppState) {
    let selected = app
        .active_session
        .and_then(|active| app.sessions.iter().position(|session| session.id == active))
        .unwrap_or(0);
    app.overlay = Overlay::Sessions {
        selected,
        query: String::new(),
    };
}

pub(super) fn open_theme_picker(app: &mut AppState) {
    app.overlay = Overlay::Themes {
        selected: theme::theme_index(&app.theme_id),
    };
}

fn open_workspace_prompt(app: &mut AppState) {
    let value = std::env::current_dir()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|_| ".".to_owned());
    app.overlay = Overlay::TextPrompt {
        kind: TextPromptKind::WorkspaceRoot,
        title: " workspace root · Enter create · Esc cancel ".to_owned(),
        value,
    };
    app.dirty = true;
}

fn open_checkpoint_name_prompt(app: &mut AppState) {
    if app.active_session.is_none() {
        app.show_toast("No active session. Create or resume one first.", true);
        return;
    }
    if !app.connection.capabilities.contains("session_checkpoint") {
        app.show_toast("Daemon missing `session_checkpoint` capability.", true);
        return;
    }
    app.overlay = Overlay::TextPrompt {
        kind: TextPromptKind::CheckpointName,
        title: " checkpoint name · Enter save · Esc cancel ".to_owned(),
        value: String::new(),
    };
    app.dirty = true;
}

fn fork_session_effects(app: &mut AppState, sequence: Option<u64>) -> Vec<Effect> {
    if app.active_session.is_none() {
        app.show_toast("No active session. Create or resume one first.", true);
        return vec![];
    }
    if !app.connection.capabilities.contains("session_fork") {
        app.show_toast("Daemon missing `session_fork` capability.", true);
        return vec![];
    }
    let up_to_sequence = sequence.unwrap_or(app.last_sequence);
    if up_to_sequence == 0 {
        app.show_toast("Nothing to fork yet — wait for durable events.", true);
        return vec![];
    }
    app.dirty = true;
    vec![Effect::ForkSession { up_to_sequence }]
}

pub(super) fn open_files_overlay(app: &mut AppState) -> Vec<Effect> {
    if app.active_session.is_none() {
        app.show_toast("No active session. Create or resume one first.", true);
        return vec![];
    }
    let mut state = FilesOverlayState::new();
    state.loading_dirs.insert(".".to_owned());
    app.overlay = Overlay::Files { state };
    app.dirty = true;
    vec![Effect::FilesListDir {
        path: ".".to_owned(),
    }]
}

pub(super) fn open_branch_picker(app: &mut AppState) -> Vec<Effect> {
    if app.active_session.is_none() {
        app.show_toast("No active session. Create or resume one first.", true);
        return vec![];
    }
    if !app.connection.capabilities.contains("git") {
        app.show_toast("Daemon missing `git` capability.", true);
        return vec![];
    }
    app.status_message = "loading branches".to_owned();
    app.dirty = true;
    vec![Effect::LoadBranches]
}

pub(super) fn files_preview_for_selection(state: &mut FilesOverlayState) -> Vec<Effect> {
    let Some(row) = state.selected_row() else {
        state.preview_path = None;
        state.preview_text = None;
        state.preview_error = None;
        state.preview_loading = false;
        return vec![];
    };
    if row.is_dir {
        return vec![];
    }
    if state.preview_path.as_deref() == Some(row.path.as_str()) && state.preview_text.is_some() {
        return vec![];
    }
    state.preview_path = Some(row.path.clone());
    state.preview_text = None;
    state.preview_error = None;
    state.preview_loading = true;
    state.preview_scroll = 0;
    vec![Effect::FilesRead { path: row.path }]
}

pub(super) fn handle_files_overlay_key(
    mut state: FilesOverlayState,
    key: KeyEvent,
) -> (Overlay, Vec<Effect>) {
    let mut effects = Vec::new();
    match (state.focus, key.code) {
        (_, KeyCode::Esc) if state.search_active || state.focus == FilesFocus::Search => {
            state.clear_search();
        }
        (_, KeyCode::Char('/')) if state.focus != FilesFocus::Search => {
            state.focus = FilesFocus::Search;
            state.search_query.clear();
        }
        (FilesFocus::Search, KeyCode::Enter) => {
            let pattern = state.search_query.trim().to_owned();
            if pattern.is_empty() {
                state.clear_search();
            } else {
                state.search_loading = true;
                state.error = None;
                effects.push(Effect::FilesSearch { pattern });
            }
        }
        (FilesFocus::Search, KeyCode::Backspace) => {
            state.search_query.pop();
        }
        (FilesFocus::Search, KeyCode::Char(ch))
            if !key.modifiers.contains(KeyModifiers::CONTROL)
                && !key.modifiers.contains(KeyModifiers::ALT) =>
        {
            state.search_query.push(ch);
        }
        (_, KeyCode::Tab) => {
            state.focus = match state.focus {
                FilesFocus::Tree => FilesFocus::Preview,
                FilesFocus::Preview => FilesFocus::Tree,
                FilesFocus::Search => FilesFocus::Tree,
            };
        }
        (_, KeyCode::Char('r') | KeyCode::Char('R'))
            if state.focus != FilesFocus::Search
                && (key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT) =>
        {
            state.begin_refresh();
            effects.push(Effect::FilesListDir {
                path: ".".to_owned(),
            });
        }
        (FilesFocus::Preview, KeyCode::Up | KeyCode::PageUp) => {
            state.preview_scroll = state.preview_scroll.saturating_sub(1);
        }
        (FilesFocus::Preview, KeyCode::Down | KeyCode::PageDown) => {
            state.preview_scroll = state.preview_scroll.saturating_add(1);
        }
        (FilesFocus::Tree, KeyCode::Up) => {
            state.selected = state.selected.saturating_sub(1);
            effects.extend(files_preview_for_selection(&mut state));
        }
        (FilesFocus::Tree, KeyCode::Down) => {
            let len = state.visible_rows().len();
            if len > 0 {
                state.selected = (state.selected + 1).min(len - 1);
            }
            effects.extend(files_preview_for_selection(&mut state));
        }
        (FilesFocus::Tree, KeyCode::Left) if !state.search_active => {
            if let Some(row) = state.selected_row()
                && row.is_dir
                && row.expanded
            {
                state.expanded.remove(&row.path);
                state.clamp_selected();
            }
        }
        (FilesFocus::Tree, KeyCode::Right | KeyCode::Enter) => {
            if let Some(row) = state.selected_row() {
                if row.is_dir && !state.search_active {
                    if row.expanded {
                        state.expanded.remove(&row.path);
                    } else {
                        state.expanded.insert(row.path.clone());
                        let key = FilesOverlayState::dir_key(&row.path);
                        if !state.children.contains_key(&key) {
                            state.loading_dirs.insert(key);
                            effects.push(Effect::FilesListDir { path: row.path });
                        }
                    }
                    state.clamp_selected();
                } else {
                    effects.extend(files_preview_for_selection(&mut state));
                    state.focus = FilesFocus::Preview;
                }
            }
        }
        (FilesFocus::Tree, KeyCode::Char(ch))
            if !state.search_active
                && !key.modifiers.contains(KeyModifiers::CONTROL)
                && !key.modifiers.contains(KeyModifiers::ALT)
                && ch != '/'
                && ch != 'r'
                && ch != 'R' =>
        {
            state.filter.push(ch);
            state.selected = 0;
            state.clamp_selected();
            effects.extend(files_preview_for_selection(&mut state));
        }
        (FilesFocus::Tree, KeyCode::Backspace)
            if !state.search_active && !state.filter.is_empty() =>
        {
            state.filter.pop();
            state.clamp_selected();
            effects.extend(files_preview_for_selection(&mut state));
        }
        _ => {}
    }
    (Overlay::Files { state }, effects)
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

pub(super) fn scroll_timeline_home(app: &mut AppState) {
    app.follow_tail = false;
    app.line_scroll_from_bottom =
        max_scroll_from_bottom(app.timeline_line_count, app.timeline_viewport_rows);
    app.focus = Focus::Timeline;
}

pub(super) fn handle_overlay_key(app: &mut AppState, key: KeyEvent) -> Vec<Effect> {
    if key.code == KeyCode::Esc {
        if let Overlay::Files { state } = &app.overlay
            && (state.search_active || state.focus == FilesFocus::Search)
        {
            let Overlay::Files { mut state } = std::mem::take(&mut app.overlay) else {
                unreachable!();
            };
            state.clear_search();
            app.overlay = Overlay::Files { state };
            app.dirty = true;
            return vec![];
        }
        if matches!(app.overlay, Overlay::ModelPicker { .. }) {
            let Overlay::ModelPicker { mut state } = std::mem::take(&mut app.overlay) else {
                unreachable!();
            };
            if state.step_back() {
                app.overlay = Overlay::ModelPicker { state };
                app.dirty = true;
                return vec![];
            }
            app.overlay = Overlay::None;
            app.dirty = true;
            return vec![];
        }
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
                    open_workspace_prompt(app);
                    return vec![];
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
        Overlay::ModelPicker { state } => handle_model_picker_key(app, state, key),
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
        Overlay::Files { state } => handle_files_overlay_key(state, key),
        Overlay::Review { state } => handle_review_overlay_key(state, key),
        Overlay::TextPrompt {
            kind,
            title,
            mut value,
        } => match key.code {
            KeyCode::Backspace => {
                let _ = value.pop();
                (Overlay::TextPrompt { kind, title, value }, vec![])
            }
            KeyCode::Char(ch)
                if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT =>
            {
                value.push(ch);
                (Overlay::TextPrompt { kind, title, value }, vec![])
            }
            KeyCode::Enter => {
                let trimmed = value.trim().to_owned();
                if trimmed.is_empty() {
                    app.show_toast("Enter a non-empty value.", true);
                    return vec![];
                }
                app.overlay = Overlay::None;
                app.dirty = true;
                match kind {
                    TextPromptKind::WorkspaceRoot => {
                        return vec![Effect::CreateSession {
                            workspace: PathBuf::from(trimmed),
                        }];
                    }
                    TextPromptKind::CheckpointName => {
                        return vec![Effect::CreateCheckpoint { name: trimmed }];
                    }
                }
            }
            _ => (Overlay::TextPrompt { kind, title, value }, vec![]),
        },
        Overlay::Checkpoints {
            mut selected,
            checkpoints,
        } => match key.code {
            KeyCode::Up => {
                selected = selected.saturating_sub(1);
                (
                    Overlay::Checkpoints {
                        selected,
                        checkpoints,
                    },
                    vec![],
                )
            }
            KeyCode::Down => {
                selected = (selected + 1).min(checkpoints.len().saturating_sub(1));
                (
                    Overlay::Checkpoints {
                        selected,
                        checkpoints,
                    },
                    vec![],
                )
            }
            KeyCode::Enter => {
                if let Some(cp) = checkpoints.get(selected) {
                    let checkpoint_id = cp.id;
                    app.overlay = Overlay::None;
                    app.dirty = true;
                    return vec![Effect::RestoreCheckpoint { checkpoint_id }];
                }
                (
                    Overlay::Checkpoints {
                        selected,
                        checkpoints,
                    },
                    vec![],
                )
            }
            KeyCode::Char('n') | KeyCode::Char('N') => {
                open_checkpoint_name_prompt(app);
                return vec![];
            }
            _ => (
                Overlay::Checkpoints {
                    selected,
                    checkpoints,
                },
                vec![],
            ),
        },
        Overlay::AttachPath { mut path } => match key.code {
            KeyCode::Backspace => {
                let _ = path.pop();
                (Overlay::AttachPath { path }, vec![])
            }
            KeyCode::Char(ch)
                if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT =>
            {
                path.push(ch);
                (Overlay::AttachPath { path }, vec![])
            }
            KeyCode::Enter => {
                let trimmed = path.trim().to_owned();
                if trimmed.is_empty() {
                    app.show_toast("Enter a filesystem path to attach.", true);
                    return vec![];
                }
                app.overlay = Overlay::None;
                app.dirty = true;
                return start_attach_upload(app, trimmed);
            }
            _ => (Overlay::AttachPath { path }, vec![]),
        },
        Overlay::Branches {
            mut selected,
            mut query,
            branches,
        } => {
            let filtered = filtered_branches(&branches, &query);
            let filtered_len = filtered.len();
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
                    if key.modifiers.contains(KeyModifiers::CONTROL) =>
                {
                    let name = query.trim().to_owned();
                    if name.is_empty() {
                        app.show_toast("Type a branch name, then Ctrl+N to create.", true);
                    } else {
                        app.overlay = Overlay::Branches {
                            selected,
                            query,
                            branches,
                        };
                        app.dirty = true;
                        return vec![Effect::CreateBranch { name }];
                    }
                }
                KeyCode::Char(ch)
                    if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT =>
                {
                    query.push(ch);
                    selected = 0;
                }
                KeyCode::Enter => {
                    let filtered = filtered_branches(&branches, &query);
                    if let Some(branch) = filtered.get(selected) {
                        let name = branch.name.clone();
                        app.overlay = Overlay::Branches {
                            selected,
                            query,
                            branches,
                        };
                        app.dirty = true;
                        return vec![Effect::SwitchBranch { name }];
                    }
                    let name = query.trim().to_owned();
                    if !name.is_empty() {
                        app.overlay = Overlay::Branches {
                            selected,
                            query,
                            branches,
                        };
                        app.dirty = true;
                        return vec![Effect::CreateBranch { name }];
                    }
                }
                _ => {}
            }
            (
                Overlay::Branches {
                    selected,
                    query,
                    branches,
                },
                vec![],
            )
        }
        other => (other, vec![]),
    };

    app.overlay = new_overlay;
    app.dirty = true;
    effects
}

pub(super) fn handle_review_overlay_key(
    mut state: ReviewOverlayState,
    key: KeyEvent,
) -> (Overlay, Vec<Effect>) {
    let mut effects = Vec::new();
    match (state.focus, key.code) {
        (_, KeyCode::Tab) => {
            state.focus = match state.focus {
                ReviewFocus::Files => ReviewFocus::Diff,
                ReviewFocus::Diff => ReviewFocus::Files,
            };
        }
        (_, KeyCode::Char('r') | KeyCode::Char('R'))
            if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT =>
        {
            state.loading = true;
            state.error = None;
            effects.push(Effect::ReviewLoad);
        }
        (ReviewFocus::Files, KeyCode::Up) => {
            state.selected = state.selected.saturating_sub(1);
            effects.extend(review_load_selected(&mut state));
        }
        (ReviewFocus::Files, KeyCode::Down) => {
            if !state.files.is_empty() {
                state.selected = (state.selected + 1).min(state.files.len() - 1);
            }
            effects.extend(review_load_selected(&mut state));
        }
        (ReviewFocus::Files, KeyCode::Enter | KeyCode::Right) => {
            effects.extend(review_load_selected(&mut state));
            state.focus = ReviewFocus::Diff;
        }
        (ReviewFocus::Diff, KeyCode::Up | KeyCode::PageUp) => {
            state.diff_scroll = state.diff_scroll.saturating_sub(1);
        }
        (ReviewFocus::Diff, KeyCode::Down | KeyCode::PageDown) => {
            state.diff_scroll = state.diff_scroll.saturating_add(1);
        }
        (ReviewFocus::Diff, KeyCode::Left) => {
            state.focus = ReviewFocus::Files;
        }
        (_, KeyCode::Char('n') | KeyCode::Char(']')) if key.modifiers.is_empty() => {
            state.jump_hunk(1);
        }
        (_, KeyCode::Char('[')) if key.modifiers.is_empty() => {
            state.jump_hunk(-1);
        }
        (ReviewFocus::Diff, KeyCode::Char('p')) if key.modifiers.is_empty() => {
            state.jump_hunk(-1);
        }
        _ => {}
    }
    (Overlay::Review { state }, effects)
}

fn review_load_selected(state: &mut ReviewOverlayState) -> Vec<Effect> {
    let Some(path) = state.selected_path().map(str::to_owned) else {
        return vec![];
    };
    if state.diff_path.as_deref() == Some(path.as_str()) && state.diff_patch.is_some() {
        return vec![];
    }
    state.diff_path = Some(path.clone());
    state.diff_patch = None;
    state.diff_error = None;
    state.diff_loading = true;
    state.diff_scroll = 0;
    state.hunk_line_idxs.clear();
    state.selected_hunk = 0;
    vec![Effect::ReviewLoadFile { path }]
}

pub(super) fn open_review_overlay(app: &mut AppState) -> Vec<Effect> {
    if !app.connection.capabilities.contains("git") {
        app.show_toast("Daemon missing `git` capability.", true);
        return vec![];
    }
    let Some(_session_id) = app.active_session else {
        app.show_toast("No active session. Create or resume one first.", true);
        return vec![];
    };
    app.overlay = Overlay::Review {
        state: ReviewOverlayState::new(),
    };
    app.dirty = true;
    vec![Effect::ReviewLoad]
}

pub(super) fn open_pty_passthrough(
    app: &mut AppState,
    command: Option<String>,
    args: Vec<String>,
) -> Vec<Effect> {
    if !app.connection.capabilities.contains("pty") {
        app.show_toast(
            "Daemon missing capability `pty` (need IPC v9 + portable-pty).",
            true,
        );
        return vec![];
    }
    if app.active_session.is_none() {
        app.show_toast("No active session. Create or resume one first.", true);
        return vec![];
    }
    let (command, args) = match command {
        Some(command) => (command, args),
        None => crate::pty_passthrough::default_shell(),
    };
    app.overlay = Overlay::None;
    app.dirty = true;
    vec![Effect::EnterPtyPassthrough { command, args }]
}

pub(super) fn handle_composer_key(app: &mut AppState, key: KeyEvent) -> Vec<Effect> {
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
            KeyCode::Char('f') => return open_files_overlay(app),
            KeyCode::Char('b') => return open_branch_picker(app),
            KeyCode::Char('r') => return open_review_overlay(app),
            KeyCode::Char('\\') => return open_pty_passthrough(app, None, Vec::new()),
            KeyCode::Char('t') => set_steer_intent(app),
            KeyCode::Char('c') => {
                if !app.composer.is_empty()
                    || app.pending_large_paste.is_some()
                    || app.pending_artifact.is_some()
                {
                    app.composer.clear();
                    app.pending_large_paste = None;
                    app.pending_artifact = None;
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
            KeyCode::Char('a') | KeyCode::Char('A')
                if key.modifiers.contains(KeyModifiers::SHIFT) =>
            {
                return execute_command(app, CommandAction::Attach { path: None });
            }
            KeyCode::Char('a') => app.composer.move_home(),
            KeyCode::Char('e') => app.composer.move_end(),
            KeyCode::Char('w') => app.composer.delete_previous_word(),
            KeyCode::Char('u') => app.composer.kill_to_line_start(),
            KeyCode::Char('k') | KeyCode::Char('K')
                if key.modifiers.contains(KeyModifiers::SHIFT) =>
            {
                return fork_session_effects(app, None);
            }
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
                app.pending_artifact = None;
                return execute_command(app, action);
            }
            if let Some(body) = app.pending_large_paste.take() {
                app.pending_artifact = None;
                let label = if is_paste_placeholder(&text) {
                    format_paste_placeholder(body.len(), paste_line_count(&body))
                } else {
                    text
                };
                return vec![send_large_paste_effect(app.prompt_intent, label, body)];
            }
            if let Some(pending) = app.pending_artifact.take() {
                let label = if is_attach_placeholder(&text) || text.trim().is_empty() {
                    pending.label
                } else {
                    text
                };
                return vec![send_effect(
                    app.prompt_intent,
                    label,
                    Some(pending.artifact),
                )];
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
            return vec![send_effect(app.prompt_intent, text, None)];
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

pub(super) fn execute_command(app: &mut AppState, action: CommandAction) -> Vec<Effect> {
    match action {
        CommandAction::NewSession => {
            open_workspace_prompt(app);
            vec![]
        }
        CommandAction::Attach { path } => match path {
            Some(path) if !path.trim().is_empty() => {
                start_attach_upload(app, path.trim().to_owned())
            }
            _ => {
                app.overlay = Overlay::AttachPath {
                    path: String::new(),
                };
                app.dirty = true;
                vec![]
            }
        },
        CommandAction::Fork(seq) => fork_session_effects(app, seq),
        CommandAction::Checkpoint(name) => match name {
            Some(name) if !name.trim().is_empty() => {
                vec![Effect::CreateCheckpoint {
                    name: name.trim().to_owned(),
                }]
            }
            _ => {
                open_checkpoint_name_prompt(app);
                vec![]
            }
        },
        CommandAction::Checkpoints => {
            app.dirty = true;
            vec![Effect::LoadCheckpoints]
        }
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
        CommandAction::ModelPicker => vec![Effect::OpenModelPicker],
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
        CommandAction::Files => open_files_overlay(app),
        CommandAction::Review => open_review_overlay(app),
        CommandAction::PtyPassthrough { command, args } => open_pty_passthrough(app, command, args),
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

fn send_effect(
    intent: impetus_client::protocol::UserPromptIntent,
    text: String,
    artifact: Option<impetus_client::protocol::DurableArtifactRef>,
) -> Effect {
    Effect::SendMessage {
        text,
        intent,
        artifact,
    }
}

fn start_attach_upload(app: &mut AppState, path: String) -> Vec<Effect> {
    if app.active_session.is_none() {
        app.show_toast("No active session. Create or resume one first.", true);
        return vec![];
    }
    if !app.connection.capabilities.contains("artifact_upload") {
        app.show_toast(
            "File attach requires a daemon with artifact_upload capability.",
            true,
        );
        return vec![];
    }
    app.status_message = format!("uploading attach · {path}");
    app.dirty = true;
    vec![Effect::UploadArtifact { path }]
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

fn handle_model_picker_key(
    app: &mut AppState,
    mut state: ModelPickerState,
    key: KeyEvent,
) -> (Overlay, Vec<Effect>) {
    match key.code {
        KeyCode::Left => {
            if state.step_back() {
                (Overlay::ModelPicker { state }, vec![])
            } else {
                (Overlay::None, vec![])
            }
        }
        KeyCode::Backspace if !state.query.is_empty() => {
            state.query.pop();
            state.selected = 0;
            (Overlay::ModelPicker { state }, vec![])
        }
        KeyCode::Char(ch)
            if !key.modifiers.contains(KeyModifiers::CONTROL)
                && !key.modifiers.contains(KeyModifiers::ALT)
                && !matches!(
                    state.step,
                    ModelPickerStep::Reasoning | ModelPickerStep::Options
                ) =>
        {
            state.query.push(ch);
            state.selected = 0;
            (Overlay::ModelPicker { state }, vec![])
        }
        KeyCode::Up => {
            state.selected = state.selected.saturating_sub(1);
            (Overlay::ModelPicker { state }, vec![])
        }
        KeyCode::Down => {
            let len = model_picker_row_count(app, &state).max(1);
            state.selected = (state.selected + 1).min(len.saturating_sub(1));
            (Overlay::ModelPicker { state }, vec![])
        }
        KeyCode::Enter => advance_model_picker(app, state),
        _ => (Overlay::ModelPicker { state }, vec![]),
    }
}

fn model_picker_row_count(app: &AppState, state: &ModelPickerState) -> usize {
    match state.step {
        ModelPickerStep::Provider => {
            let providers = provider_choices(&app.provider_catalog);
            filter_by_query(&providers, &state.query, |p| p.display_name.clone()).len()
        }
        ModelPickerStep::Model => {
            let Some(provider_id) = state.draft_provider_id.as_deref() else {
                return 0;
            };
            let models = models_for_provider(&app.provider_catalog, provider_id);
            filter_by_query(&models, &state.query, |row| {
                row.model_display_name
                    .clone()
                    .unwrap_or_else(|| row.model_id.clone())
            })
            .len()
        }
        ModelPickerStep::Reasoning => {
            let Some(provider_id) = state.draft_provider_id.as_deref() else {
                return 0;
            };
            let Some(model_id) = state.draft_model_id.as_deref() else {
                return 0;
            };
            catalog::find_row(&app.provider_catalog, provider_id, model_id)
                .map(|row| row.reasoning_efforts.len())
                .unwrap_or(0)
        }
        ModelPickerStep::Options => {
            let Some(provider_id) = state.draft_provider_id.as_deref() else {
                return 0;
            };
            let Some(model_id) = state.draft_model_id.as_deref() else {
                return 0;
            };
            catalog::find_row(&app.provider_catalog, provider_id, model_id)
                .map(|row| option_choices(row).len().saturating_add(1)) // + Skip
                .unwrap_or(0)
        }
    }
}

fn advance_model_picker(app: &mut AppState, mut state: ModelPickerState) -> (Overlay, Vec<Effect>) {
    match state.step {
        ModelPickerStep::Provider => {
            let providers = provider_choices(&app.provider_catalog);
            let filtered = filter_by_query(&providers, &state.query, |p| p.display_name.clone());
            let Some((_, choice)) = filtered.get(state.selected) else {
                app.show_toast("No providers in catalog.", true);
                return (Overlay::ModelPicker { state }, vec![]);
            };
            if !choice.selectable {
                app.show_toast(
                    format!("Provider `{}` unavailable.", choice.provider_id),
                    true,
                );
                return (Overlay::ModelPicker { state }, vec![]);
            }
            state.draft_provider_id = Some(choice.provider_id.clone());
            state.draft_model_id = None;
            state.draft_reasoning = None;
            state.draft_options = None;
            state.visited_reasoning = false;
            state.step = ModelPickerStep::Model;
            state.selected = 0;
            state.query.clear();
            (Overlay::ModelPicker { state }, vec![])
        }
        ModelPickerStep::Model => {
            let Some(provider_id) = state.draft_provider_id.clone() else {
                state.step = ModelPickerStep::Provider;
                return (Overlay::ModelPicker { state }, vec![]);
            };
            let models = models_for_provider(&app.provider_catalog, &provider_id);
            let filtered = filter_by_query(&models, &state.query, |row| {
                row.model_display_name
                    .clone()
                    .unwrap_or_else(|| row.model_id.clone())
            });
            let Some((_, row)) = filtered.get(state.selected) else {
                app.show_toast("No models for provider.", true);
                return (Overlay::ModelPicker { state }, vec![]);
            };
            if !row_is_selectable(row) {
                app.show_toast(format!("Model `{}` unavailable.", row.model_id), true);
                return (Overlay::ModelPicker { state }, vec![]);
            }
            state.draft_model_id = Some(row.model_id.clone());
            state.draft_reasoning = row.default_reasoning_effort.clone();
            state.draft_options = None;
            state.visited_reasoning = false;
            state.query.clear();
            state.selected = 0;
            if !row.reasoning_efforts.is_empty() {
                if let Some(default) = row.default_reasoning_effort.as_ref()
                    && let Some(idx) = row.reasoning_efforts.iter().position(|e| e == default)
                {
                    state.selected = idx;
                }
                state.step = ModelPickerStep::Reasoning;
                state.visited_reasoning = true;
                return (Overlay::ModelPicker { state }, vec![]);
            }
            if !option_choices(row).is_empty() {
                state.step = ModelPickerStep::Options;
                return (Overlay::ModelPicker { state }, vec![]);
            }
            commit_model_picker(app, state)
        }
        ModelPickerStep::Reasoning => {
            let Some(provider_id) = state.draft_provider_id.clone() else {
                state.step = ModelPickerStep::Provider;
                return (Overlay::ModelPicker { state }, vec![]);
            };
            let Some(model_id) = state.draft_model_id.clone() else {
                state.step = ModelPickerStep::Model;
                return (Overlay::ModelPicker { state }, vec![]);
            };
            let Some(row) = catalog::find_row(&app.provider_catalog, &provider_id, &model_id)
            else {
                app.show_toast("Catalog row missing.", true);
                return (Overlay::ModelPicker { state }, vec![]);
            };
            let Some(effort) = row.reasoning_efforts.get(state.selected).cloned() else {
                app.show_toast("Select a reasoning effort.", true);
                return (Overlay::ModelPicker { state }, vec![]);
            };
            state.draft_reasoning = Some(effort);
            state.visited_reasoning = true;
            state.selected = 0;
            state.query.clear();
            if !option_choices(row).is_empty() {
                state.step = ModelPickerStep::Options;
                return (Overlay::ModelPicker { state }, vec![]);
            }
            commit_model_picker(app, state)
        }
        ModelPickerStep::Options => {
            let Some(provider_id) = state.draft_provider_id.clone() else {
                state.step = ModelPickerStep::Provider;
                return (Overlay::ModelPicker { state }, vec![]);
            };
            let Some(model_id) = state.draft_model_id.clone() else {
                state.step = ModelPickerStep::Model;
                return (Overlay::ModelPicker { state }, vec![]);
            };
            let Some(row) = catalog::find_row(&app.provider_catalog, &provider_id, &model_id)
            else {
                app.show_toast("Catalog row missing.", true);
                return (Overlay::ModelPicker { state }, vec![]);
            };
            let choices = option_choices(row);
            // index 0 = Skip / none
            if state.selected == 0 {
                state.draft_options = None;
            } else if let Some(choice) = choices.get(state.selected.saturating_sub(1)) {
                state.draft_options =
                    Some(merge_option_choice(state.draft_options.clone(), choice));
            }
            commit_model_picker(app, state)
        }
    }
}

fn commit_model_picker(app: &mut AppState, state: ModelPickerState) -> (Overlay, Vec<Effect>) {
    let Some(provider_id) = state.draft_provider_id.clone() else {
        app.show_toast("Provider required.", true);
        return (Overlay::ModelPicker { state }, vec![]);
    };
    let Some(model_id) = state.draft_model_id.clone() else {
        app.show_toast("Model required.", true);
        return (Overlay::ModelPicker { state }, vec![]);
    };
    if let Some(row) = catalog::find_row(&app.provider_catalog, &provider_id, &model_id)
        && !row_is_selectable(row)
    {
        app.show_toast("Selected model is unavailable.", true);
        return (Overlay::ModelPicker { state }, vec![]);
    }
    (
        Overlay::None,
        vec![Effect::SetSessionModel {
            provider_id,
            model_id,
            reasoning_effort: state.draft_reasoning,
            options: state.draft_options,
        }],
    )
}

pub(super) fn cycle_execution_mode(app: &mut AppState) -> Vec<Effect> {
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
        "# Session status\n\n- **Backend:** {}\n- **IPC:** v{}\n- **Session:** {}\n- **Run:** {}\n- **Mode:** {}\n- **Model:** {}\n- **Events rendered:** {}\n- **Last sequence:** {}\n- **Tokens used:** {}\n- **Context:** {}%\n- **Turns:** {}\n- **Compactions:** {}\n\nThe client owns only this projection. Durable history, policy and execution remain in `impetusd`.",
        app.connection.label,
        app.connection.protocol_version,
        app.active_session
            .map(|id| id.to_string())
            .unwrap_or_else(|| "none".to_owned()),
        app.run_state.label(),
        app.mode.label(),
        app.session_model_label(),
        app.timeline.len(),
        app.last_sequence,
        app.budget.tokens_used,
        app.budget.context_used_percent,
        app.budget.turns_used,
        app.budget.compactions,
    )
}

pub(super) fn scroll_up(app: &mut AppState, lines: usize) {
    app.follow_tail = false;
    app.line_scroll_from_bottom = app.line_scroll_from_bottom.saturating_add(lines);
    app.clamp_timeline_scroll();
    app.focus = Focus::Timeline;
    app.dirty = true;
}

pub(super) fn scroll_down(app: &mut AppState, lines: usize) {
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

pub(super) fn set_steer_intent(app: &mut AppState) {
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
