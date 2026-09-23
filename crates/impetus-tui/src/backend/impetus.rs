use anyhow::{Result, bail};
use async_trait::async_trait;
use impetus_client::protocol::ExecutionMode;
use impetus_client::protocol::{
    AgentEvent, ApprovalEvent, ApprovalState, BackendEvent, BudgetEvent, ChildEvent, CommandEvent,
    Event, EventPayload, IpcRequest, IpcResponse, MAX_IPC_LINE_BYTES, NoticeEvent, PtyEvent,
    RetryEvent, RunEvent, SandboxEvent, SessionEvent, ToolEvent,
};
use impetus_client::{EventSubscription, HarnessClient, UnixSocketTransport};
use std::collections::BTreeSet;
use std::path::PathBuf;
use uuid::Uuid;

use super::{UiBackend, UiEventStream};
use crate::model::{
    ApprovalCard, ApprovalDetailView, BudgetState, ConnectionInfo, SessionSummary, UiEvent,
    UiEventKind,
};

pub struct ImpetusBackend {
    client: UnixSocketTransport,
}

impl ImpetusBackend {
    pub async fn connect(socket_path: &str) -> Result<Self> {
        Ok(Self {
            client: UnixSocketTransport::connect(socket_path).await?,
        })
    }
}

#[async_trait]
impl UiBackend for ImpetusBackend {
    async fn connection_info(&self) -> Result<ConnectionInfo> {
        match self.client.hello().await? {
            IpcResponse::Hello {
                version,
                capabilities,
            } => Ok(ConnectionInfo {
                protocol_version: version,
                capabilities: capabilities.into_iter().collect::<BTreeSet<_>>(),
                label: "impetusd · unix socket".to_owned(),
            }),
            IpcResponse::Incompatible {
                supported_version,
                client_version,
                upgrade_recommendation,
                ..
            } => bail!(
                "IPC incompatible: client={client_version}, daemon={supported_version}. {}",
                upgrade_recommendation
                    .unwrap_or_else(|| "Upgrade the client or daemon.".to_owned())
            ),
            response => bail!("unexpected hello response: {response:?}"),
        }
    }

    async fn list_sessions(&self) -> Result<Vec<SessionSummary>> {
        Ok(self
            .client
            .list_sessions()
            .await?
            .into_iter()
            .map(|session| {
                SessionSummary::from_session_info(
                    session.id,
                    session.parent_session_id,
                    session.fork_sequence,
                    None,
                    None,
                    None,
                )
            })
            .collect())
    }

    async fn create_session(&self, workspace_root: PathBuf) -> Result<Uuid> {
        self.client.create_session(workspace_root).await
    }

    async fn fork_session(&self, session_id: Uuid, up_to_sequence: u64) -> Result<Uuid> {
        self.client.fork_session(session_id, up_to_sequence).await
    }

    async fn create_checkpoint(
        &self,
        session_id: Uuid,
        name: String,
        sequence: Option<u64>,
    ) -> Result<impetus_client::protocol::CheckpointInfo> {
        self.client
            .create_checkpoint(session_id, name, sequence)
            .await
    }

    async fn list_checkpoints(
        &self,
        session_id: Uuid,
    ) -> Result<Vec<impetus_client::protocol::CheckpointInfo>> {
        self.client.list_checkpoints(session_id).await
    }

    async fn restore_checkpoint(&self, checkpoint_id: Uuid) -> Result<Uuid> {
        self.client.restore_checkpoint(checkpoint_id).await
    }

    async fn resume_session(&self, session_id: Uuid) -> Result<String> {
        Ok(format!(
            "{:?}",
            self.client.resume_session(session_id).await?
        ))
    }

    async fn send_message(
        &self,
        session_id: Uuid,
        text: String,
        intent: impetus_client::protocol::UserPromptIntent,
        artifact: Option<impetus_client::protocol::DurableArtifactRef>,
    ) -> Result<String> {
        let request = IpcRequest::Prompt {
            session_id,
            text,
            artifact,
            intent,
        };
        let encoded_len = serde_json::to_vec(&request)?.len().saturating_add(1);
        if encoded_len > MAX_IPC_LINE_BYTES {
            bail!(
                "prompt serializes to {encoded_len} bytes, above the 64 KiB IPC line limit; paste again so the TUI can upload via artifact_upload"
            );
        }
        match self.client.request(request).await? {
            IpcResponse::Status { status, .. } => Ok(format!("{status:?}")),
            IpcResponse::Error { message, .. } => bail!(message),
            response => bail!("unexpected prompt response: {response:?}"),
        }
    }

    async fn send_large_paste(
        &self,
        session_id: Uuid,
        label: String,
        body: Vec<u8>,
        intent: impetus_client::protocol::UserPromptIntent,
    ) -> Result<String> {
        let hello = self.client.hello().await?;
        let supports_upload = match &hello {
            IpcResponse::Hello { capabilities, .. } => {
                capabilities.iter().any(|cap| cap == "artifact_upload")
            }
            IpcResponse::Incompatible {
                supported_version,
                client_version,
                upgrade_recommendation,
                ..
            } => {
                bail!(
                    "IPC incompatible: client={client_version}, daemon={supported_version}. {}",
                    upgrade_recommendation
                        .as_deref()
                        .unwrap_or("Upgrade the client or daemon.")
                );
            }
            response => bail!("unexpected hello response: {response:?}"),
        };
        if !supports_upload {
            bail!(
                "daemon does not expose artifact_upload; cannot send large paste ({} bytes)",
                body.len()
            );
        }

        let artifact = self
            .client
            .upload_artifact(session_id, &body, Some("text/plain".into()))
            .await
            .map_err(|error| anyhow::anyhow!("artifact upload failed: {error}"))?;

        match self
            .client
            .send_message_with_intent(session_id, label, Some(artifact), intent)
            .await
        {
            Ok(status) => Ok(format!("{status:?}")),
            Err(error) => bail!("prompt after upload failed: {error}"),
        }
    }

    async fn upload_artifact(
        &self,
        session_id: Uuid,
        bytes: Vec<u8>,
        content_type: Option<String>,
    ) -> Result<impetus_client::protocol::DurableArtifactRef> {
        let hello = self.client.hello().await?;
        let supports_upload = match &hello {
            IpcResponse::Hello { capabilities, .. } => {
                capabilities.iter().any(|cap| cap == "artifact_upload")
            }
            IpcResponse::Incompatible {
                supported_version,
                client_version,
                upgrade_recommendation,
                ..
            } => {
                bail!(
                    "IPC incompatible: client={client_version}, daemon={supported_version}. {}",
                    upgrade_recommendation
                        .as_deref()
                        .unwrap_or("Upgrade the client or daemon.")
                );
            }
            response => bail!("unexpected hello response: {response:?}"),
        };
        if !supports_upload {
            bail!(
                "daemon does not expose artifact_upload; cannot attach file ({} bytes)",
                bytes.len()
            );
        }
        self.client
            .upload_artifact(session_id, &bytes, content_type)
            .await
            .map_err(|error| anyhow::anyhow!("artifact upload failed: {error}"))
    }

    async fn get_artifact_metadata(
        &self,
        artifact_id: String,
    ) -> Result<impetus_client::protocol::DurableArtifactMeta> {
        self.client
            .get_artifact_metadata(artifact_id)
            .await
            .map_err(|error| anyhow::anyhow!("artifact metadata failed: {error}"))
    }

    async fn cancel(&self, session_id: Uuid) -> Result<String> {
        Ok(format!("{:?}", self.client.cancel(session_id).await?))
    }

    async fn resolve_approval(
        &self,
        session_id: Uuid,
        approval_id: Uuid,
        accepted: bool,
    ) -> Result<()> {
        self.client
            .resolve_approval(session_id, approval_id, accepted)
            .await
    }

    async fn approval_detail(
        &self,
        session_id: Uuid,
        approval_id: Uuid,
    ) -> Result<ApprovalDetailView> {
        match self
            .client
            .request(IpcRequest::GetApprovalDetail {
                session_id,
                approval_id,
            })
            .await?
        {
            IpcResponse::ApprovalDetail { detail, .. } => {
                let mut attachments = Vec::new();
                for attachment_id in &detail.attachment_refs {
                    // Session-bound fetch: owner session_id required by GetAttachment.
                    if let Ok((content_type, content)) =
                        self.get_attachment(session_id, *attachment_id).await
                    {
                        attachments.push(crate::model::FetchedAttachment {
                            id: *attachment_id,
                            content_type,
                            content,
                        });
                    }
                }
                Ok(ApprovalDetailView {
                    diff_preview: detail.diff_preview,
                    diff_observation: detail.diff_observation,
                    affected_files: detail.affected_files,
                    estimated_scope: detail.estimated_scope.map(|scope| format!("{scope:?}")),
                    attachment_refs: detail.attachment_refs,
                    attachments,
                })
            }
            IpcResponse::Error { message, .. } => bail!(message),
            response => bail!("unexpected approval detail response: {response:?}"),
        }
    }

    async fn get_attachment(
        &self,
        session_id: Uuid,
        attachment_id: Uuid,
    ) -> Result<(String, Vec<u8>)> {
        self.client.get_attachment(session_id, attachment_id).await
    }

    async fn diagnostics(&self) -> Result<String> {
        match self.client.request(IpcRequest::Diagnostics).await? {
            IpcResponse::Diagnostics { subsystems } => {
                Ok(serde_json::to_string_pretty(&subsystems)
                    .unwrap_or_else(|_| format!("{subsystems:#?}")))
            }
            IpcResponse::Error { message, .. } => bail!(message),
            response => bail!("unexpected diagnostics response: {response:?}"),
        }
    }

    async fn list_child_runs(&self, session_id: Uuid) -> Result<String> {
        match self
            .client
            .request(IpcRequest::ListChildRuns { session_id })
            .await?
        {
            IpcResponse::ChildRuns { runs, .. } => {
                if runs.is_empty() {
                    return Ok("(no child runs)".into());
                }
                let mut lines = Vec::new();
                for run in runs {
                    lines.push(format!(
                        "{}  {}  {}  {}",
                        run.child_id,
                        run.role_label,
                        run.status.as_str(),
                        run.summary_label
                    ));
                }
                Ok(lines.join("\n"))
            }
            IpcResponse::Error { message, .. } => bail!(message),
            response => bail!("unexpected child runs response: {response:?}"),
        }
    }

    async fn get_execution_mode(&self, session_id: Uuid) -> Result<ExecutionMode> {
        self.client.get_execution_mode(session_id).await
    }

    async fn set_execution_mode(
        &self,
        session_id: Uuid,
        mode: ExecutionMode,
    ) -> Result<ExecutionMode> {
        self.client.set_execution_mode(session_id, mode).await
    }

    async fn list_providers(&self) -> Result<Vec<impetus_client::protocol::ModelProviderStatus>> {
        self.client.list_providers().await
    }

    async fn get_session_model(
        &self,
        session_id: Uuid,
    ) -> Result<impetus_client::protocol::SessionModelSelection> {
        self.client.get_session_model(session_id).await
    }

    async fn set_session_model(
        &self,
        session_id: Uuid,
        provider_id: String,
        model_id: String,
        reasoning_effort: Option<String>,
    ) -> Result<impetus_client::protocol::SessionModelSelection> {
        self.client
            .set_session_model(session_id, provider_id, model_id, reasoning_effort)
            .await
    }

    async fn list_workspace_dir(
        &self,
        session_id: Uuid,
        path: PathBuf,
    ) -> Result<impetus_client::protocol::WorkspaceDirListing> {
        self.client.list_workspace_dir(session_id, path).await
    }

    async fn read_workspace_file(
        &self,
        session_id: Uuid,
        path: PathBuf,
        max_bytes: Option<usize>,
    ) -> Result<impetus_client::protocol::WorkspaceFileContent> {
        self.client
            .read_workspace_file(session_id, path, max_bytes)
            .await
    }

    async fn search_workspace_files(
        &self,
        session_id: Uuid,
        path: PathBuf,
        pattern: String,
    ) -> Result<impetus_client::protocol::WorkspaceSearchResult> {
        self.client
            .search_workspace_files(session_id, path, pattern)
            .await
    }

    async fn list_branches(
        &self,
        session_id: Uuid,
    ) -> Result<Vec<impetus_client::protocol::GitBranchInfo>> {
        self.client.list_branches(session_id).await
    }

    async fn get_current_branch(
        &self,
        session_id: Uuid,
    ) -> Result<impetus_client::protocol::GitCurrentBranch> {
        self.client.get_current_branch(session_id).await
    }

    async fn create_branch(
        &self,
        session_id: Uuid,
        name: String,
        checkout: bool,
    ) -> Result<impetus_client::protocol::GitCurrentBranch> {
        self.client.create_branch(session_id, name, checkout).await
    }

    async fn switch_branch(
        &self,
        session_id: Uuid,
        name: String,
    ) -> Result<impetus_client::protocol::GitCurrentBranch> {
        self.client.switch_branch(session_id, name).await
    }

    async fn git_status(
        &self,
        session_id: Uuid,
    ) -> Result<impetus_client::protocol::GitStatusSnapshot> {
        self.client.git_status(session_id).await
    }

    async fn list_changed_files(
        &self,
        session_id: Uuid,
    ) -> Result<Vec<impetus_client::protocol::GitChangedFile>> {
        self.client.list_changed_files(session_id).await
    }

    async fn get_diff(
        &self,
        session_id: Uuid,
        base_ref: Option<String>,
    ) -> Result<impetus_client::protocol::GitDiffPayload> {
        self.client.get_diff(session_id, base_ref).await
    }

    async fn get_file_diff(
        &self,
        session_id: Uuid,
        path: PathBuf,
        base_ref: Option<String>,
    ) -> Result<impetus_client::protocol::GitDiffPayload> {
        self.client.get_file_diff(session_id, path, base_ref).await
    }

    async fn pty_start(
        &self,
        session_id: Uuid,
        command: String,
        args: Vec<String>,
        working_dir: Option<PathBuf>,
        cols: Option<u16>,
        rows: Option<u16>,
    ) -> Result<impetus_client::PtySessionView> {
        self.client
            .pty_start(session_id, command, args, working_dir, cols, rows)
            .await
    }

    async fn pty_input(&self, session_id: Uuid, pty_id: u64, data: &[u8]) -> Result<()> {
        self.client.pty_input(session_id, pty_id, data).await
    }

    async fn pty_output(
        &self,
        session_id: Uuid,
        pty_id: u64,
        max_bytes: Option<usize>,
    ) -> Result<impetus_client::PtyOutputView> {
        self.client.pty_output(session_id, pty_id, max_bytes).await
    }

    async fn pty_resize(&self, session_id: Uuid, pty_id: u64, cols: u16, rows: u16) -> Result<()> {
        self.client.pty_resize(session_id, pty_id, cols, rows).await
    }

    async fn pty_detach(&self, session_id: Uuid, pty_id: u64) -> Result<()> {
        self.client.pty_detach(session_id, pty_id).await
    }

    async fn subscribe(
        &self,
        session_id: Uuid,
        after_sequence: u64,
    ) -> Result<Box<dyn UiEventStream>> {
        Ok(Box::new(ImpetusEventStream {
            inner: self
                .client
                .subscribe_live(session_id, after_sequence)
                .await?,
        }))
    }
}

struct ImpetusEventStream {
    inner: Box<dyn EventSubscription>,
}

#[async_trait]
impl UiEventStream for ImpetusEventStream {
    async fn next_batch(&mut self) -> Result<Vec<UiEvent>> {
        Ok(self
            .inner
            .next_events()
            .await?
            .into_iter()
            .map(map_event)
            .collect())
    }
}

fn map_event(event: Event) -> UiEvent {
    let kind = match event.payload {
        EventPayload::Session(SessionEvent::Created) => UiEventKind::SessionCreated,
        EventPayload::Session(SessionEvent::WorkspaceRoot { workspace_root }) => {
            UiEventKind::SessionWorkspace {
                workspace: workspace_root.display().to_string(),
            }
        }
        EventPayload::Session(SessionEvent::Attached) => UiEventKind::SessionAttached,
        EventPayload::Session(SessionEvent::ExecutionModeChanged { mode }) => {
            UiEventKind::ExecutionModeChanged { mode }
        }
        EventPayload::Intent(intent) => UiEventKind::UserInput {
            text: intent.text,
            artifact: intent.artifact,
        },
        EventPayload::Plan(plan) => UiEventKind::Plan {
            summary: plan.summary,
        },
        EventPayload::Run(RunEvent::Started { run_id }) => UiEventKind::RunStarted { run_id },
        EventPayload::Run(RunEvent::Completed { run_id }) => UiEventKind::RunCompleted { run_id },
        EventPayload::Run(RunEvent::Failed { run_id, reason }) => {
            UiEventKind::RunFailed { run_id, reason }
        }
        EventPayload::Run(RunEvent::Cancelled { run_id }) => UiEventKind::RunCancelled { run_id },
        EventPayload::Run(RunEvent::InterruptedUnknown { run_id }) => {
            UiEventKind::RunUnknown { run_id }
        }
        EventPayload::Agent(AgentEvent::Chunk {
            run_id,
            chunk_id,
            text,
            ..
        }) => UiEventKind::AgentChunk {
            run_id,
            chunk_id,
            text,
        },
        EventPayload::Agent(AgentEvent::Final { run_id, text }) => {
            UiEventKind::AgentFinal { run_id, text }
        }
        EventPayload::Agent(AgentEvent::ReasoningSummary { run_id, text }) => {
            UiEventKind::ReasoningSummary { run_id, text }
        }
        EventPayload::Tool(ToolEvent::Started { name, .. }) => UiEventKind::ToolStarted { name },
        EventPayload::Tool(ToolEvent::Finished { name, summary, .. }) => {
            UiEventKind::ToolFinished { name, summary }
        }
        EventPayload::Tool(ToolEvent::Output {
            tool_call_id,
            tool_name,
            preview,
        }) => UiEventKind::ToolObserved {
            call_id: tool_call_id,
            name: tool_name,
            arguments: String::new(),
            outcome: "running".to_owned(),
            preview,
            artifact: None,
            error: None,
        },
        EventPayload::Tool(ToolEvent::Observed {
            tool_call_id,
            tool_name,
            arguments_summary,
            outcome,
            preview,
            artifact,
            error,
        }) => UiEventKind::ToolObserved {
            call_id: tool_call_id,
            name: tool_name,
            arguments: arguments_summary,
            outcome: format!("{outcome:?}"),
            preview,
            artifact: artifact.map(|value| format!("{value:?}")),
            error,
        },
        EventPayload::Tool(ToolEvent::Deferred {
            approval_id,
            tool_call_id,
            tool_name,
            arguments,
        }) => UiEventKind::ToolDeferred {
            approval_id,
            call_id: tool_call_id,
            name: tool_name,
            arguments: serde_json::to_string_pretty(&arguments)
                .unwrap_or_else(|_| arguments.to_string()),
        },
        EventPayload::Tool(ToolEvent::FileRead { path, preview, .. }) => {
            UiEventKind::ActivityStep {
                label: format!("read · {path}"),
                detail: Some(preview),
                is_error: false,
            }
        }
        EventPayload::Tool(ToolEvent::SearchStarted {
            pattern, target, ..
        }) => UiEventKind::ActivityStep {
            label: format!("search · {pattern}"),
            detail: Some(format!("target: {target}")),
            is_error: false,
        },
        EventPayload::Tool(ToolEvent::SearchResult {
            match_count,
            preview,
            ..
        }) => UiEventKind::ActivityStep {
            label: format!("search · {match_count} hits"),
            detail: Some(preview),
            is_error: false,
        },
        EventPayload::Pty(pty) => match pty {
            PtyEvent::Started {
                pty_id, command, ..
            } => UiEventKind::ActivityStep {
                label: format!("pty {pty_id} · {command}"),
                detail: None,
                is_error: false,
            },
            PtyEvent::Output {
                pty_id,
                preview,
                eof,
                ..
            } => UiEventKind::ActivityStep {
                label: format!("pty {pty_id} · output"),
                detail: Some(if eof {
                    format!("{preview}\n(eof)")
                } else {
                    preview
                }),
                is_error: false,
            },
            PtyEvent::Spill {
                pty_id,
                artifact,
                dropped_bytes,
            } => UiEventKind::ActivityStep {
                label: format!("pty {pty_id} · spill"),
                detail: Some(format!("{dropped_bytes} bytes → artifact {}", artifact.id)),
                is_error: false,
            },
            PtyEvent::Exited { pty_id, exit_code } => UiEventKind::ActivityStep {
                label: format!("pty {pty_id} · exit {exit_code:?}"),
                detail: None,
                is_error: exit_code.is_some_and(|code| code != 0),
            },
        },
        EventPayload::Command(cmd) => match cmd {
            CommandEvent::Started {
                tool_call_id,
                command,
            } => UiEventKind::ActivityStep {
                label: format!("cmd · {command}"),
                detail: Some(format!("call_id: {tool_call_id}")),
                is_error: false,
            },
            CommandEvent::Output {
                tool_call_id,
                preview,
            } => UiEventKind::ActivityStep {
                label: "cmd · output".to_owned(),
                detail: Some(format!("call_id: {tool_call_id}\n{preview}")),
                is_error: false,
            },
            CommandEvent::Finished {
                tool_call_id,
                exit_code,
                summary,
            } => UiEventKind::ActivityStep {
                label: format!("cmd · exit {exit_code:?}"),
                detail: Some(format!(
                    "call_id: {tool_call_id}{}",
                    summary.map(|s| format!("\n{s}")).unwrap_or_default()
                )),
                is_error: exit_code.is_some_and(|code| code != 0),
            },
        },
        EventPayload::Child(child) => match child {
            ChildEvent::Started {
                child_id,
                parent_id,
                role,
            } => UiEventKind::ChildStarted {
                child_id,
                role,
                parent_id,
            },
            ChildEvent::StatusChanged {
                child_id,
                status,
                current_action,
            } => UiEventKind::ChildStatus {
                child_id,
                status,
                current_action,
            },
            ChildEvent::Progress {
                child_id,
                percent,
                summary,
            } => UiEventKind::ChildStatus {
                child_id,
                status: percent
                    .map(|p| format!("progress:{p}%"))
                    .unwrap_or_else(|| "progress".into()),
                current_action: Some(summary),
            },
            ChildEvent::Action {
                child_id,
                name,
                preview,
            } => UiEventKind::ActivityStep {
                label: format!("child · {name}"),
                detail: Some(format!("child_id: {child_id}\n{preview}")),
                is_error: false,
            },
            ChildEvent::Finished {
                child_id,
                status,
                summary,
                error,
            } => UiEventKind::ChildFinished {
                child_id,
                status,
                summary,
                error,
            },
        },
        EventPayload::Approval(ApprovalEvent::Requested { request }) => {
            let fingerprint = serde_json::to_string(&request.action_fingerprint)
                .unwrap_or_else(|_| "unknown".to_owned());
            UiEventKind::ApprovalRequested {
                approval: ApprovalCard {
                    id: request.id,
                    action_kind: format!("{:?}", request.action.kind),
                    summary: request.action.summary,
                    target: request.action.target,
                    reason: request.reason,
                    fingerprint,
                    detail: None,
                },
            }
        }
        EventPayload::Approval(ApprovalEvent::Resolved { request }) => {
            UiEventKind::ApprovalResolved {
                approval_id: request.id,
                accepted: matches!(request.state, ApprovalState::Approved),
            }
        }
        EventPayload::Backend(BackendEvent::ProviderHealthy { profile }) => UiEventKind::Backend {
            title: format!("provider {profile}"),
            detail: "healthy".to_owned(),
            healthy: true,
        },
        EventPayload::Backend(BackendEvent::ProviderDegraded { profile, reason }) => {
            UiEventKind::Backend {
                title: format!("provider {profile}"),
                detail: reason,
                healthy: false,
            }
        }
        EventPayload::Backend(BackendEvent::ProviderUnavailable { profile, reason }) => {
            UiEventKind::Backend {
                title: format!("provider {profile}"),
                detail: reason,
                healthy: false,
            }
        }
        EventPayload::Backend(BackendEvent::KeychainAvailable) => UiEventKind::Backend {
            title: "credential store".to_owned(),
            detail: "Keychain available".to_owned(),
            healthy: true,
        },
        EventPayload::Backend(BackendEvent::KeychainUnavailable { reason }) => {
            UiEventKind::Backend {
                title: "credential store".to_owned(),
                detail: reason,
                healthy: false,
            }
        }
        EventPayload::Backend(BackendEvent::TokenExpiryWarning {
            profile,
            expires_in_seconds,
        }) => UiEventKind::Backend {
            title: format!("token expiry · {profile}"),
            detail: format!("expires in {expires_in_seconds}s"),
            healthy: false,
        },
        EventPayload::Budget(BudgetEvent::Updated {
            turns_used,
            tokens_used,
            measured: _,
            compaction_count,
            context_used_percent,
        }) => UiEventKind::BudgetUpdated(BudgetState {
            turns_used,
            tokens_used,
            context_used_percent,
            compactions: compaction_count,
            warning: None,
        }),
        EventPayload::Budget(BudgetEvent::CompactionRequired { threshold, used }) => {
            UiEventKind::BudgetWarning {
                message: format!("context compaction required: {used}/{threshold} tokens"),
            }
        }
        EventPayload::Budget(BudgetEvent::CompactionStarted {
            from_sequence,
            to_sequence,
            threshold,
            used,
        }) => UiEventKind::BudgetWarning {
            message: format!(
                "compacting events {from_sequence}..{to_sequence} ({used}/{threshold} tokens)"
            ),
        },
        EventPayload::Budget(BudgetEvent::CompactionCompleted {
            compacted_to,
            compaction_count,
            ..
        }) => UiEventKind::Notice {
            title: "context compacted".to_owned(),
            message: format!("{compacted_to} tokens · compaction #{compaction_count}"),
            error: false,
            remediation: None,
        },
        EventPayload::Budget(BudgetEvent::TurnLimitApproaching { limit, used }) => {
            UiEventKind::BudgetWarning {
                message: format!("turn limit approaching: {used}/{limit}"),
            }
        }
        EventPayload::Budget(BudgetEvent::TokenLimitApproaching { limit, used }) => {
            UiEventKind::BudgetWarning {
                message: format!("token limit approaching: {used}/{limit}"),
            }
        }
        EventPayload::Notice(NoticeEvent::PolicyAllowed) => UiEventKind::Notice {
            title: "policy".to_owned(),
            message: "action allowed".to_owned(),
            error: false,
            remediation: None,
        },
        EventPayload::Notice(NoticeEvent::PolicyDenied { reason }) => UiEventKind::Notice {
            title: "policy denied".to_owned(),
            message: reason,
            error: true,
            remediation: None,
        },
        EventPayload::Notice(NoticeEvent::Runtime { message }) => UiEventKind::Notice {
            title: "runtime".to_owned(),
            message,
            error: false,
            remediation: None,
        },
        EventPayload::Notice(NoticeEvent::Legacy { event_kind, body }) => {
            let remediation = body
                .get("remediation")
                .and_then(|value| value.as_str())
                .filter(|value| !value.trim().is_empty())
                .map(str::to_owned);
            let error = body
                .get("error")
                .and_then(|value| value.as_bool())
                .unwrap_or(false);
            UiEventKind::Notice {
                title: format!("legacy event · {event_kind}"),
                message: serde_json::to_string_pretty(&body).unwrap_or_else(|_| body.to_string()),
                error,
                remediation,
            }
        }
        EventPayload::Retry(RetryEvent::Attempting {
            attempt,
            max_attempts,
            reason,
            backoff_ms,
        }) => UiEventKind::Retry {
            title: format!("retry {attempt}/{max_attempts}"),
            message: format!("{reason} · backoff {backoff_ms}ms"),
            failed: false,
        },
        EventPayload::Retry(RetryEvent::Succeeded { attempt }) => UiEventKind::Retry {
            title: "retry succeeded".to_owned(),
            message: format!("recovered on attempt {attempt}"),
            failed: false,
        },
        EventPayload::Retry(RetryEvent::Exhausted {
            attempts,
            last_error,
        }) => UiEventKind::Retry {
            title: "retries exhausted".to_owned(),
            message: format!("{attempts} attempts · {last_error}"),
            failed: true,
        },
        EventPayload::Sandbox(sandbox) => {
            let (title, message) = match sandbox {
                SandboxEvent::Decision {
                    backend,
                    prepare_state,
                    network_allowed,
                    writable_root_count,
                    reason_code,
                } => (
                    format!("sandbox · {backend}"),
                    format!(
                        "{prepare_state:?} · network={network_allowed} · writable_roots={writable_root_count}{}",
                        reason_code
                            .map(|code| format!(" · {code}"))
                            .unwrap_or_default()
                    ),
                ),
            };
            UiEventKind::Notice {
                title,
                message,
                error: false,
                remediation: None,
            }
        }
    };

    UiEvent {
        sequence: event.sequence,
        at_unix_ms: event.at_unix_ms,
        kind,
    }
}
