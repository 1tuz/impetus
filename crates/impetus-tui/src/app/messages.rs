//! TEA messages: async effect results folded into AppState.

use uuid::Uuid;

use crate::model::{
    AppState, ExecutionMode, FilesOverlayState, Overlay, RunState, SessionSummary, UiEvent,
};

use super::effects::Effect;
use super::ingest::ingest_event;
use super::input::files_preview_for_selection;
use super::short;

#[derive(Debug)]
pub(super) enum AppMessage {
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
    SessionModelRestored {
        session_id: Uuid,
        generation: u64,
        result: Result<
            (
                Vec<impetus_client::protocol::ModelProviderStatus>,
                impetus_client::protocol::SessionModelSelection,
            ),
            String,
        >,
        open_picker: bool,
    },
    SessionModelUpdated {
        session_id: Uuid,
        generation: u64,
        result: Result<impetus_client::protocol::SessionModelSelection, String>,
        options: Option<serde_json::Value>,
    },
    FilesDirListed {
        session_id: Uuid,
        generation: u64,
        path: String,
        result: Result<impetus_client::protocol::WorkspaceDirListing, String>,
    },
    FilesContent {
        session_id: Uuid,
        generation: u64,
        path: String,
        result: Result<impetus_client::protocol::WorkspaceFileContent, String>,
    },
    FilesSearchResult {
        session_id: Uuid,
        generation: u64,
        result: Result<impetus_client::protocol::WorkspaceSearchResult, String>,
    },
    BranchesLoaded {
        session_id: Uuid,
        generation: u64,
        result: Result<Vec<impetus_client::protocol::GitBranchInfo>, String>,
    },
    BranchChanged {
        session_id: Uuid,
        generation: u64,
        action: &'static str,
        result: Result<impetus_client::protocol::GitCurrentBranch, String>,
    },
    CurrentBranchLoaded {
        session_id: Uuid,
        generation: u64,
        result: Result<impetus_client::protocol::GitCurrentBranch, String>,
    },
    ReviewLoaded {
        session_id: Uuid,
        generation: u64,
        result: Result<ReviewSnapshot, String>,
    },
    ReviewFileDiff {
        session_id: Uuid,
        generation: u64,
        path: String,
        result: Result<impetus_client::protocol::GitDiffPayload, String>,
    },
    SessionForked(Result<Uuid, String>),
    CheckpointCreated(Result<String, String>),
    CheckpointsLoaded {
        session_id: Uuid,
        generation: u64,
        result: Result<Vec<impetus_client::protocol::CheckpointInfo>, String>,
    },
    CheckpointRestored(Result<Uuid, String>),
    /// Local filesystem attach finished (chunked upload → DurableArtifactRef).
    ArtifactUploaded {
        session_id: Uuid,
        generation: u64,
        path: String,
        result: Result<crate::model::PendingArtifact, String>,
    },
}

#[derive(Debug)]
pub(super) struct ReviewSnapshot {
    pub(super) branch_label: String,
    pub(super) dirty: bool,
    pub(super) files: Vec<crate::model::ReviewFileRow>,
}

pub(super) fn apply_message(app: &mut AppState, message: AppMessage) -> Vec<Effect> {
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
                    app.dirty = true;
                    return vec![Effect::RefreshCurrentBranch];
                }
                Err(error) => {
                    app.status_message = "attach failed".to_owned();
                    app.show_toast(format!("attach failed: {error}"), true);
                    app.current_branch = None;
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
        AppMessage::SessionModelRestored {
            session_id,
            generation,
            result,
            open_picker,
        } => {
            if !is_current_operation(app, session_id, generation) {
                return vec![];
            }
            match result {
                Ok((catalog, selection)) => {
                    app.provider_catalog = catalog;
                    app.session_model = Some(selection.clone());
                    app.status_message = format!(
                        "model · {}",
                        crate::catalog::session_model_label(Some(&selection), None)
                    );
                    if open_picker {
                        let mut state = crate::catalog::ModelPickerState::fresh(Some(&selection));
                        // Highlight current provider when opening.
                        let providers = crate::catalog::provider_choices(&app.provider_catalog);
                        if let Some(idx) = providers
                            .iter()
                            .position(|p| p.provider_id == selection.provider_id)
                        {
                            state.selected = idx;
                        }
                        app.overlay = Overlay::ModelPicker { state };
                    }
                }
                Err(error) => {
                    app.show_toast(format!("provider catalog failed: {error}"), true);
                }
            }
            app.dirty = true;
        }
        AppMessage::SessionModelUpdated {
            session_id,
            generation,
            result,
            options,
        } => {
            if !is_current_operation(app, session_id, generation) {
                return vec![];
            }
            match result {
                Ok(selection) => {
                    app.session_model = Some(selection.clone());
                    app.session_model_options = options;
                    app.overlay = Overlay::None;
                    app.status_message = format!(
                        "model · {}",
                        crate::catalog::session_model_label(
                            Some(&selection),
                            app.session_model_options.as_ref(),
                        )
                    );
                    app.show_toast(
                        format!(
                            "Model: {}",
                            crate::catalog::session_model_label(
                                Some(&selection),
                                app.session_model_options.as_ref(),
                            )
                        ),
                        false,
                    );
                }
                Err(error) => {
                    app.show_toast(format!("set session model failed: {error}"), true);
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
        AppMessage::FilesDirListed {
            session_id,
            generation,
            path,
            result,
        } => {
            if !is_current_operation(app, session_id, generation) {
                return vec![];
            }
            let Overlay::Files { state } = &mut app.overlay else {
                return vec![];
            };
            match result {
                Ok(listing) => {
                    let key = FilesOverlayState::dir_key(&path);
                    state.apply_listing(&key, listing.entries);
                    app.dirty = true;
                    return files_preview_for_selection(state);
                }
                Err(error) => {
                    let key = FilesOverlayState::dir_key(&path);
                    state.loading_dirs.remove(&key);
                    state.error = Some(error);
                }
            }
            app.dirty = true;
        }
        AppMessage::FilesContent {
            session_id,
            generation,
            path,
            result,
        } => {
            if !is_current_operation(app, session_id, generation) {
                return vec![];
            }
            let Overlay::Files { state } = &mut app.overlay else {
                return vec![];
            };
            if state.preview_path.as_deref() != Some(path.as_str()) {
                return vec![];
            }
            state.preview_loading = false;
            match result {
                Ok(content) => {
                    state.preview_text = Some(content.content);
                    state.preview_error = None;
                    state.preview_scroll = 0;
                }
                Err(error) => {
                    state.preview_text = None;
                    state.preview_error = Some(error);
                }
            }
            app.dirty = true;
        }
        AppMessage::FilesSearchResult {
            session_id,
            generation,
            result,
        } => {
            if !is_current_operation(app, session_id, generation) {
                return vec![];
            }
            let Overlay::Files { state } = &mut app.overlay else {
                return vec![];
            };
            match result {
                Ok(search) => {
                    let hits = search
                        .hits
                        .into_iter()
                        .map(|hit| crate::model::FilesSearchHitRow {
                            path: hit.path,
                            line: hit.line,
                            text: hit.text,
                        })
                        .collect();
                    state.apply_search(hits, search.truncated);
                }
                Err(error) => {
                    state.search_loading = false;
                    state.error = Some(error);
                }
            }
            app.dirty = true;
        }
        AppMessage::BranchesLoaded {
            session_id,
            generation,
            result,
        } => {
            if !is_current_operation(app, session_id, generation) {
                return vec![];
            }
            match result {
                Ok(branches) => {
                    let selected = branches
                        .iter()
                        .position(|branch| branch.current)
                        .unwrap_or(0);
                    app.overlay = Overlay::Branches {
                        selected,
                        query: String::new(),
                        branches,
                    };
                    app.status_message = "branches".to_owned();
                }
                Err(error) => {
                    app.show_toast(format!("list branches failed: {error}"), true);
                }
            }
            app.dirty = true;
        }
        AppMessage::BranchChanged {
            session_id,
            generation,
            action,
            result,
        } => {
            if !is_current_operation(app, session_id, generation) {
                return vec![];
            }
            match result {
                Ok(branch) => {
                    let label = branch
                        .name
                        .clone()
                        .unwrap_or_else(|| "(detached)".to_owned());
                    app.current_branch = Some(label.clone());
                    app.show_toast(format!("Branch {action}: {label}"), false);
                    app.overlay = Overlay::None;
                    return vec![Effect::LoadBranches];
                }
                Err(error) => {
                    app.show_toast(format!("branch {action} failed: {error}"), true);
                }
            }
            app.dirty = true;
        }
        AppMessage::CurrentBranchLoaded {
            session_id,
            generation,
            result,
        } => {
            if !is_current_operation(app, session_id, generation) {
                return vec![];
            }
            match result {
                Ok(branch) => {
                    app.current_branch = branch.name;
                }
                Err(error) => {
                    app.show_toast(format!("current branch failed: {error}"), true);
                }
            }
            app.dirty = true;
        }
        AppMessage::ReviewLoaded {
            session_id,
            generation,
            result,
        } => {
            if !is_current_operation(app, session_id, generation) {
                return vec![];
            }
            match result {
                Ok(snapshot) => {
                    let mut state = match &app.overlay {
                        Overlay::Review { state } => state.clone(),
                        _ => crate::model::ReviewOverlayState::new(),
                    };
                    state.apply_snapshot(snapshot.branch_label, snapshot.dirty, snapshot.files);
                    app.overlay = Overlay::Review { state };
                }
                Err(error) => {
                    if let Overlay::Review { state } = &mut app.overlay {
                        state.loading = false;
                        state.error = Some(error.clone());
                    }
                    app.show_toast(format!("review load failed: {error}"), true);
                }
            }
            app.dirty = true;
        }
        AppMessage::ReviewFileDiff {
            session_id,
            generation,
            path,
            result,
        } => {
            if !is_current_operation(app, session_id, generation) {
                return vec![];
            }
            let Overlay::Review { state } = &mut app.overlay else {
                return vec![];
            };
            if state.diff_path.as_deref() != Some(path.as_str())
                && state.selected_path() != Some(path.as_str())
            {
                return vec![];
            }
            match result {
                Ok(diff) => {
                    let patch = diff.patch;
                    let observation = diff.observation;
                    let hunks = crate::review::hunk_line_indices(&patch);
                    state.set_diff(path, patch, hunks, observation);
                }
                Err(error) => {
                    state.diff_loading = false;
                    state.diff_error = Some(error);
                }
            }
            app.dirty = true;
        }
        AppMessage::SessionForked(result) => match result {
            Ok(session_id) => {
                app.show_toast(format!("Forked → {}", short(session_id)), false);
                return vec![Effect::RefreshSessions, Effect::ActivateSession(session_id)];
            }
            Err(error) => {
                app.show_toast(format!("fork failed: {error}"), true);
                app.dirty = true;
            }
        },
        AppMessage::CheckpointCreated(result) => match result {
            Ok(label) => {
                app.show_toast(format!("Checkpoint saved · {label}"), false);
                app.status_message = "ready".to_owned();
                app.dirty = true;
            }
            Err(error) => {
                app.show_toast(format!("checkpoint failed: {error}"), true);
                app.dirty = true;
            }
        },
        AppMessage::CheckpointsLoaded {
            session_id,
            generation,
            result,
        } => {
            if !is_current_operation(app, session_id, generation) {
                return vec![];
            }
            match result {
                Ok(checkpoints) => {
                    app.overlay = Overlay::Checkpoints {
                        selected: 0,
                        checkpoints,
                    };
                    app.status_message = "ready".to_owned();
                    app.dirty = true;
                }
                Err(error) => {
                    app.show_toast(format!("list checkpoints failed: {error}"), true);
                    app.dirty = true;
                }
            }
        }
        AppMessage::CheckpointRestored(result) => match result {
            Ok(session_id) => {
                app.show_toast(format!("Restored → {}", short(session_id)), false);
                return vec![Effect::RefreshSessions, Effect::ActivateSession(session_id)];
            }
            Err(error) => {
                app.show_toast(format!("restore failed: {error}"), true);
                app.dirty = true;
            }
        },
        AppMessage::ArtifactUploaded {
            session_id,
            generation,
            path,
            result,
        } => {
            if !is_current_operation(app, session_id, generation) {
                return vec![];
            }
            match result {
                Ok(pending) => {
                    let label = pending.label.clone();
                    let file_name = pending.file_name.clone();
                    let source_path = pending.path.clone();
                    let ref_line = crate::model::format_artifact_ref_label(
                        &pending.artifact.id,
                        pending.artifact.byte_count,
                        pending.content_type.as_deref(),
                    );
                    app.pending_artifact = Some(pending);
                    app.composer.clear();
                    app.composer.insert_str(&label);
                    app.status_message = "artifact attached".to_owned();
                    app.show_toast(
                        format!(
                            "Attached {file_name} ({source_path}) → {ref_line}. Enter sends Prompt + ArtifactRef."
                        ),
                        false,
                    );
                    app.focus = crate::model::Focus::Composer;
                    app.dirty = true;
                }
                Err(error) => {
                    app.status_message = "attach failed".to_owned();
                    app.show_toast(format!("attach failed ({path}): {error}"), true);
                    app.dirty = true;
                }
            }
        }
    }
    vec![]
}

pub(super) fn is_current_operation(app: &AppState, session_id: Uuid, generation: u64) -> bool {
    app.active_session == Some(session_id) && app.subscription_generation == generation
}
