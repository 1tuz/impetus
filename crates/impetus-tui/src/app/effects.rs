//! TEA effects: async work kicked off by the TUI loop.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use uuid::Uuid;

use crate::backend::UiBackend;
use crate::model::{
    AppState, ExecutionMode, MAX_PASTE_UPLOAD_BYTES, Overlay, PendingArtifact, RunState,
    execution_mode_is_available, format_attach_placeholder, guess_content_type,
};
use crate::terminal::TerminalSession;

use super::messages::{AppMessage, ReviewSnapshot};

#[derive(Default)]
pub(super) struct TaskManager {
    subscription: Option<JoinHandle<()>>,
}

pub(super) fn spawn_detached(future: impl std::future::Future<Output = ()> + Send + 'static) {
    drop(tokio::spawn(future));
}

impl TaskManager {
    fn replace_subscription(&mut self, handle: JoinHandle<()>) {
        if let Some(previous) = self.subscription.replace(handle) {
            previous.abort();
        }
    }

    pub(super) fn abort_all(&mut self) {
        if let Some(handle) = self.subscription.take() {
            handle.abort();
        }
    }
}

#[derive(Debug)]
pub(super) enum Effect {
    RefreshSessions,
    CreateSession {
        workspace: PathBuf,
    },
    ActivateSession(Uuid),
    ForkSession {
        up_to_sequence: u64,
    },
    CreateCheckpoint {
        name: String,
    },
    LoadCheckpoints,
    RestoreCheckpoint {
        checkpoint_id: Uuid,
    },
    SendMessage {
        text: String,
        intent: impetus_client::protocol::UserPromptIntent,
        artifact: Option<impetus_client::protocol::DurableArtifactRef>,
    },
    SendLargePaste {
        label: String,
        body: String,
        intent: impetus_client::protocol::UserPromptIntent,
    },
    /// Read local filesystem path and upload via HarnessClient chunked IPC.
    UploadArtifact {
        path: String,
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
    /// Refresh daemon provider catalog and open the model picker.
    OpenModelPicker,
    SetSessionModel {
        provider_id: String,
        model_id: String,
        reasoning_effort: Option<String>,
        /// Catalog options draft: `service_tier` key + remaining → provider_options.
        options: Option<serde_json::Value>,
    },
    FilesListDir {
        path: String,
    },
    FilesRead {
        path: String,
    },
    FilesSearch {
        pattern: String,
    },
    LoadBranches,
    SwitchBranch {
        name: String,
    },
    CreateBranch {
        name: String,
    },
    RefreshCurrentBranch,
    ReviewLoad,
    ReviewLoadFile {
        path: String,
    },
    /// Leave Ratatui and attach daemon PTY (handled on the main loop).
    EnterPtyPassthrough {
        command: String,
        args: Vec<String>,
    },
}

pub(super) async fn run_pty_passthrough_effect(
    effect: Effect,
    backend: &Arc<dyn UiBackend>,
    terminal: &mut TerminalSession,
    app: &mut AppState,
) {
    let Effect::EnterPtyPassthrough { command, args } = effect else {
        return;
    };
    let Some(session_id) = app.active_session else {
        app.show_toast("No active session for PTY.", true);
        return;
    };
    if !app.connection.capabilities.contains("pty") {
        app.show_toast(
            "Daemon missing capability `pty` (need IPC v9 + portable-pty).",
            true,
        );
        return;
    }

    app.status_message = format!(
        "PTY passthrough ({}) · {} detaches",
        crate::pty_passthrough::OPEN_HINT,
        crate::pty_passthrough::DETACH_HINT
    );
    app.dirty = true;

    let outcome =
        crate::pty_passthrough::run(backend.as_ref(), terminal, session_id, command, args).await;

    match outcome {
        Ok(crate::pty_passthrough::PassthroughEnd::Detached { pty_id }) => {
            app.show_toast(
                format!("Detached PTY {pty_id}. Daemon keeps it live."),
                false,
            );
        }
        Ok(crate::pty_passthrough::PassthroughEnd::Exited { pty_id }) => {
            app.show_toast(format!("PTY {pty_id} exited."), false);
        }
        Ok(crate::pty_passthrough::PassthroughEnd::Failed { pty_id, message }) => {
            let id = pty_id.map(|id| format!(" PTY {id}")).unwrap_or_default();
            app.show_toast(format!("PTY{id} failed: {message}"), true);
        }
        Err(error) => {
            app.show_toast(format!("PTY passthrough error: {error}"), true);
        }
    }
    app.dirty = true;
}

pub(super) fn execute_effect(
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
        Effect::CreateSession { workspace } => {
            app.status_message = "creating session".to_owned();
            app.dirty = true;
            let backend = backend.clone();
            let tx = tx.clone();
            spawn_detached(async move {
                let result = backend
                    .create_session(workspace)
                    .await
                    .map_err(|error| error.to_string());
                let _ = tx.send(AppMessage::SessionCreated(result)).await;
            });
        }
        Effect::ForkSession { up_to_sequence } => {
            let Some(session_id) = app.active_session else {
                app.show_toast("No active session. Create or resume one first.", true);
                return;
            };
            if !app.connection.capabilities.contains("session_fork") {
                app.show_toast("Daemon missing `session_fork` capability.", true);
                return;
            }
            if up_to_sequence == 0 {
                app.show_toast("Nothing to fork yet — wait for durable events.", true);
                return;
            }
            app.status_message = format!("forking @{up_to_sequence}");
            app.dirty = true;
            let backend = backend.clone();
            let tx = tx.clone();
            spawn_detached(async move {
                let result = backend
                    .fork_session(session_id, up_to_sequence)
                    .await
                    .map_err(|error| error.to_string());
                let _ = tx.send(AppMessage::SessionForked(result)).await;
            });
        }
        Effect::CreateCheckpoint { name } => {
            let Some(session_id) = app.active_session else {
                app.show_toast("No active session. Create or resume one first.", true);
                return;
            };
            if !app.connection.capabilities.contains("session_checkpoint") {
                app.show_toast("Daemon missing `session_checkpoint` capability.", true);
                return;
            }
            app.status_message = format!("checkpoint `{name}`");
            app.dirty = true;
            let backend = backend.clone();
            let tx = tx.clone();
            spawn_detached(async move {
                let result = backend
                    .create_checkpoint(session_id, name, None)
                    .await
                    .map(|cp| format!("{} @{}", cp.name, cp.sequence))
                    .map_err(|error| error.to_string());
                let _ = tx.send(AppMessage::CheckpointCreated(result)).await;
            });
        }
        Effect::LoadCheckpoints => {
            let Some(session_id) = app.active_session else {
                app.show_toast("No active session. Create or resume one first.", true);
                return;
            };
            if !app.connection.capabilities.contains("session_checkpoint") {
                app.show_toast("Daemon missing `session_checkpoint` capability.", true);
                return;
            }
            app.status_message = "loading checkpoints".to_owned();
            app.dirty = true;
            let generation = app.subscription_generation;
            let backend = backend.clone();
            let tx = tx.clone();
            spawn_detached(async move {
                let result = backend
                    .list_checkpoints(session_id)
                    .await
                    .map_err(|error| error.to_string());
                let _ = tx
                    .send(AppMessage::CheckpointsLoaded {
                        session_id,
                        generation,
                        result,
                    })
                    .await;
            });
        }
        Effect::RestoreCheckpoint { checkpoint_id } => {
            if !app.connection.capabilities.contains("session_checkpoint") {
                app.show_toast("Daemon missing `session_checkpoint` capability.", true);
                return;
            }
            app.status_message = "restoring checkpoint".to_owned();
            app.dirty = true;
            let backend = backend.clone();
            let tx = tx.clone();
            spawn_detached(async move {
                let result = backend
                    .restore_checkpoint(checkpoint_id)
                    .await
                    .map_err(|error| error.to_string());
                let _ = tx.send(AppMessage::CheckpointRestored(result)).await;
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
            app.session_model = None;
            app.session_model_options = None;
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

                let model_bundle = fetch_session_model_bundle(backend.as_ref(), session_id).await;
                if tx
                    .send(AppMessage::SessionModelRestored {
                        session_id,
                        generation,
                        result: model_bundle,
                        open_picker: false,
                    })
                    .await
                    .is_err()
                {
                    return;
                }

                let mut after_sequence = 0u64;
                let mut backoff = Duration::from_millis(250);
                let mut first_subscribe = true;
                loop {
                    if !first_subscribe {
                        let model_bundle =
                            fetch_session_model_bundle(backend.as_ref(), session_id).await;
                        if tx
                            .send(AppMessage::SessionModelRestored {
                                session_id,
                                generation,
                                result: model_bundle,
                                open_picker: false,
                            })
                            .await
                            .is_err()
                        {
                            return;
                        }
                    }
                    first_subscribe = false;
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
        Effect::SendMessage {
            text,
            intent,
            artifact,
        } => {
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
                    .send_message(session_id, text, intent, artifact)
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
        Effect::UploadArtifact { path } => {
            let Some(session_id) = app.active_session else {
                app.show_toast("No active session. Create or resume one first.", true);
                return;
            };
            if !app.connection.capabilities.contains("artifact_upload") {
                app.show_toast(
                    "File attach requires a daemon with artifact_upload capability.",
                    true,
                );
                return;
            }
            app.status_message = "uploading attach".to_owned();
            app.dirty = true;
            let generation = app.subscription_generation;
            let backend = backend.clone();
            let tx = tx.clone();
            spawn_detached(async move {
                let result = upload_local_artifact(backend.as_ref(), session_id, &path).await;
                let _ = tx
                    .send(AppMessage::ArtifactUploaded {
                        session_id,
                        generation,
                        path,
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
        Effect::OpenModelPicker => {
            if !app.connection.capabilities.contains("list_providers")
                && !app.connection.capabilities.contains("session_model")
            {
                app.show_toast(
                    "Daemon missing `list_providers` / `session_model` capability.",
                    true,
                );
                return;
            }
            let Some(session_id) = app.active_session else {
                app.show_toast("No active session. Create or resume one first.", true);
                return;
            };
            app.status_message = "loading provider catalog".to_owned();
            app.dirty = true;
            let generation = app.subscription_generation;
            let backend = backend.clone();
            let tx = tx.clone();
            spawn_detached(async move {
                let result = fetch_session_model_bundle(backend.as_ref(), session_id).await;
                let _ = tx
                    .send(AppMessage::SessionModelRestored {
                        session_id,
                        generation,
                        result,
                        open_picker: true,
                    })
                    .await;
            });
        }
        Effect::SetSessionModel {
            provider_id,
            model_id,
            reasoning_effort,
            options,
        } => {
            let Some(session_id) = app.active_session else {
                app.show_toast("No active session. Create or resume one first.", true);
                return;
            };
            if !app.connection.capabilities.contains("session_model") {
                app.show_toast("Daemon missing `session_model` capability.", true);
                return;
            }
            app.status_message = format!("setting model · {provider_id}/{model_id}");
            app.dirty = true;
            let generation = app.subscription_generation;
            let backend = backend.clone();
            let tx = tx.clone();
            let (service_tier, provider_options) = split_session_model_options(options.clone());
            spawn_detached(async move {
                let result = backend
                    .set_session_model(
                        session_id,
                        provider_id,
                        model_id,
                        reasoning_effort,
                        service_tier,
                        provider_options,
                    )
                    .await
                    .map_err(|error| error.to_string());
                let _ = tx
                    .send(AppMessage::SessionModelUpdated {
                        session_id,
                        generation,
                        result,
                        options,
                    })
                    .await;
            });
        }
        Effect::FilesListDir { path } => {
            let Some(session_id) = app.active_session else {
                app.show_toast("No active session. Create or resume one first.", true);
                return;
            };
            let generation = app.subscription_generation;
            let backend = backend.clone();
            let tx = tx.clone();
            let path_for_req = path.clone();
            spawn_detached(async move {
                let result = backend
                    .list_workspace_dir(session_id, std::path::PathBuf::from(path_for_req))
                    .await
                    .map_err(|error| error.to_string());
                let _ = tx
                    .send(AppMessage::FilesDirListed {
                        session_id,
                        generation,
                        path,
                        result,
                    })
                    .await;
            });
        }
        Effect::FilesRead { path } => {
            let Some(session_id) = app.active_session else {
                app.show_toast("No active session. Create or resume one first.", true);
                return;
            };
            let generation = app.subscription_generation;
            let backend = backend.clone();
            let tx = tx.clone();
            let path_for_req = path.clone();
            spawn_detached(async move {
                let result = backend
                    .read_workspace_file(session_id, std::path::PathBuf::from(path_for_req), None)
                    .await
                    .map_err(|error| error.to_string());
                let _ = tx
                    .send(AppMessage::FilesContent {
                        session_id,
                        generation,
                        path,
                        result,
                    })
                    .await;
            });
        }
        Effect::FilesSearch { pattern } => {
            let Some(session_id) = app.active_session else {
                app.show_toast("No active session. Create or resume one first.", true);
                return;
            };
            let generation = app.subscription_generation;
            let backend = backend.clone();
            let tx = tx.clone();
            spawn_detached(async move {
                let result = backend
                    .search_workspace_files(session_id, std::path::PathBuf::from("."), pattern)
                    .await
                    .map_err(|error| error.to_string());
                let _ = tx
                    .send(AppMessage::FilesSearchResult {
                        session_id,
                        generation,
                        result,
                    })
                    .await;
            });
        }
        Effect::LoadBranches => {
            let Some(session_id) = app.active_session else {
                app.show_toast("No active session. Create or resume one first.", true);
                return;
            };
            if !app.connection.capabilities.contains("git") {
                app.show_toast("Daemon missing `git` capability.", true);
                return;
            }
            app.status_message = "loading branches".to_owned();
            app.dirty = true;
            let generation = app.subscription_generation;
            let backend = backend.clone();
            let tx = tx.clone();
            spawn_detached(async move {
                let result = backend
                    .list_branches(session_id)
                    .await
                    .map_err(|error| error.to_string());
                let _ = tx
                    .send(AppMessage::BranchesLoaded {
                        session_id,
                        generation,
                        result,
                    })
                    .await;
            });
        }
        Effect::SwitchBranch { name } => {
            let Some(session_id) = app.active_session else {
                app.show_toast("No active session. Create or resume one first.", true);
                return;
            };
            app.status_message = format!("switching to {name}");
            app.dirty = true;
            let generation = app.subscription_generation;
            let backend = backend.clone();
            let tx = tx.clone();
            spawn_detached(async move {
                let result = backend
                    .switch_branch(session_id, name)
                    .await
                    .map_err(|error| error.to_string());
                let _ = tx
                    .send(AppMessage::BranchChanged {
                        session_id,
                        generation,
                        action: "switch",
                        result,
                    })
                    .await;
            });
        }
        Effect::CreateBranch { name } => {
            let Some(session_id) = app.active_session else {
                app.show_toast("No active session. Create or resume one first.", true);
                return;
            };
            app.status_message = format!("creating branch {name}");
            app.dirty = true;
            let generation = app.subscription_generation;
            let backend = backend.clone();
            let tx = tx.clone();
            spawn_detached(async move {
                let result = backend
                    .create_branch(session_id, name, true)
                    .await
                    .map_err(|error| error.to_string());
                let _ = tx
                    .send(AppMessage::BranchChanged {
                        session_id,
                        generation,
                        action: "create",
                        result,
                    })
                    .await;
            });
        }
        Effect::RefreshCurrentBranch => {
            let Some(session_id) = app.active_session else {
                return;
            };
            if !app.connection.capabilities.contains("git") {
                return;
            }
            let generation = app.subscription_generation;
            let backend = backend.clone();
            let tx = tx.clone();
            spawn_detached(async move {
                let result = backend
                    .get_current_branch(session_id)
                    .await
                    .map_err(|error| error.to_string());
                let _ = tx
                    .send(AppMessage::CurrentBranchLoaded {
                        session_id,
                        generation,
                        result,
                    })
                    .await;
            });
        }
        Effect::ReviewLoad => {
            let Some(session_id) = app.active_session else {
                app.show_toast("No active session. Create or resume one first.", true);
                return;
            };
            if !app.connection.capabilities.contains("git") {
                app.show_toast("Daemon missing `git` capability.", true);
                return;
            }
            app.status_message = "loading review".to_owned();
            app.dirty = true;
            let generation = app.subscription_generation;
            let backend = backend.clone();
            let tx = tx.clone();
            spawn_detached(async move {
                let result = load_review_snapshot(backend.as_ref(), session_id).await;
                let _ = tx
                    .send(AppMessage::ReviewLoaded {
                        session_id,
                        generation,
                        result,
                    })
                    .await;
            });
        }
        Effect::ReviewLoadFile { path } => {
            let Some(session_id) = app.active_session else {
                return;
            };
            let generation = app.subscription_generation;
            let backend = backend.clone();
            let tx = tx.clone();
            let path_for_req = path.clone();
            spawn_detached(async move {
                let result = backend
                    .get_file_diff(session_id, std::path::PathBuf::from(path_for_req), None)
                    .await
                    .map_err(|error| error.to_string());
                let _ = tx
                    .send(AppMessage::ReviewFileDiff {
                        session_id,
                        generation,
                        path,
                        result,
                    })
                    .await;
            });
        }
        Effect::EnterPtyPassthrough { .. } => {
            // Handled synchronously on the main loop (needs TerminalSession).
        }
    }
}

async fn upload_local_artifact(
    backend: &dyn UiBackend,
    session_id: Uuid,
    path: &str,
) -> Result<PendingArtifact, String> {
    let path_buf = PathBuf::from(path);
    let meta =
        std::fs::metadata(&path_buf).map_err(|error| format!("cannot stat `{path}`: {error}"))?;
    if !meta.is_file() {
        return Err(format!("`{path}` is not a regular file"));
    }
    let byte_count = meta.len() as usize;
    if byte_count > MAX_PASTE_UPLOAD_BYTES {
        return Err(format!(
            "file is too large ({byte_count} bytes). Maximum upload size is {MAX_PASTE_UPLOAD_BYTES} bytes."
        ));
    }
    let bytes =
        std::fs::read(&path_buf).map_err(|error| format!("cannot read `{path}`: {error}"))?;
    let content_type = guess_content_type(&path_buf);
    let artifact = backend
        .upload_artifact(session_id, bytes, content_type.clone())
        .await
        .map_err(|error| error.to_string())?;
    let (byte_count, content_type) = match backend.get_artifact_metadata(artifact.id.clone()).await
    {
        Ok(meta) => (
            if meta.byte_count > 0 {
                meta.byte_count
            } else {
                artifact.byte_count
            },
            meta.content_type.or(content_type),
        ),
        Err(_) => (artifact.byte_count, content_type),
    };
    let file_name = path_buf
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(path)
        .to_owned();
    let label = format_attach_placeholder(&file_name, byte_count, content_type.as_deref());
    Ok(PendingArtifact {
        artifact: impetus_client::protocol::DurableArtifactRef {
            id: artifact.id,
            byte_count,
        },
        path: path.to_owned(),
        file_name,
        content_type,
        label,
    })
}

async fn fetch_session_model_bundle(
    backend: &dyn UiBackend,
    session_id: Uuid,
) -> Result<
    (
        Vec<impetus_client::protocol::ModelProviderStatus>,
        impetus_client::protocol::SessionModelSelection,
    ),
    String,
> {
    let catalog = backend
        .list_providers()
        .await
        .map_err(|error| error.to_string())?;
    let selection = backend
        .get_session_model(session_id)
        .await
        .map_err(|error| error.to_string())?;
    Ok((catalog, selection))
}

async fn load_review_snapshot(
    backend: &dyn UiBackend,
    session_id: Uuid,
) -> Result<ReviewSnapshot, String> {
    let status = backend
        .git_status(session_id)
        .await
        .map_err(|error| error.to_string())?;
    let files = if status.files.is_empty() {
        backend
            .list_changed_files(session_id)
            .await
            .map_err(|error| error.to_string())?
    } else {
        status.files.clone()
    };
    let patch = backend
        .get_diff(session_id, None)
        .await
        .map(|diff| diff.patch)
        .unwrap_or_default();
    let branch_label = status.branch.name.clone().unwrap_or_else(|| {
        if status.branch.detached {
            "DETACHED".to_owned()
        } else {
            "(no branch)".to_owned()
        }
    });
    Ok(ReviewSnapshot {
        branch_label,
        dirty: status.dirty,
        files: crate::review::build_file_rows(&files, Some(patch.as_str())),
    })
}

/// Split picker draft JSON into IPC fields.
///
/// `service_tier` string key → `service_tier`; remaining object keys →
/// `provider_options`. Missing / empty draft → `(None, Null)`.
fn split_session_model_options(
    options: Option<serde_json::Value>,
) -> (Option<String>, serde_json::Value) {
    let Some(value) = options else {
        return (None, serde_json::Value::Null);
    };
    let serde_json::Value::Object(mut map) = value else {
        return (None, serde_json::Value::Null);
    };
    let service_tier = match map.remove("service_tier") {
        Some(serde_json::Value::String(s)) if !s.is_empty() => Some(s),
        _ => None,
    };
    let provider_options = if map.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::Value::Object(map)
    };
    (service_tier, provider_options)
}
