use anyhow::{Result, bail};
use async_trait::async_trait;
use impetus_client::protocol::{
    AgentEvent, ApprovalEvent, ApprovalState, BackendEvent, BudgetEvent, Event, EventPayload,
    IpcRequest, IpcResponse, NoticeEvent, RetryEvent, RunEvent, SessionEvent, ToolEvent,
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

const MAX_IPC_LINE_BYTES: usize = 64 * 1024;

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
            .list_session_branches()
            .await?
            .into_iter()
            .map(SessionSummary::from_branch)
            .collect())
    }

    async fn create_session(&self, workspace_root: PathBuf) -> Result<Uuid> {
        self.client.create_session(workspace_root).await
    }

    async fn resume_session(&self, session_id: Uuid) -> Result<String> {
        Ok(format!(
            "{:?}",
            self.client.resume_session(session_id).await?
        ))
    }

    async fn send_message(&self, session_id: Uuid, text: String) -> Result<String> {
        let request = IpcRequest::Prompt { session_id, text };
        let encoded_len = serde_json::to_vec(&request)?.len().saturating_add(1);
        if encoded_len > MAX_IPC_LINE_BYTES {
            bail!(
                "prompt serializes to {encoded_len} bytes, above the current 64 KiB IPC line limit; use an artifact-backed large-paste flow once the daemon exposes upload support"
            );
        }
        match self.client.request(request).await? {
            IpcResponse::Status { status, .. } => Ok(format!("{status:?}")),
            IpcResponse::Error { message, .. } => bail!(message),
            response => bail!("unexpected prompt response: {response:?}"),
        }
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
            IpcResponse::ApprovalDetail { detail, .. } => Ok(ApprovalDetailView {
                diff_preview: detail.diff_preview,
                affected_files: detail.affected_files,
                estimated_scope: detail.estimated_scope.map(|scope| format!("{scope:?}")),
                attachment_refs: detail.attachment_refs,
            }),
            IpcResponse::Error { message, .. } => bail!(message),
            response => bail!("unexpected approval detail response: {response:?}"),
        }
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
        EventPayload::Intent(intent) => UiEventKind::UserInput { text: intent.text },
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
        }) => UiEventKind::AgentChunk {
            run_id,
            chunk_id,
            text,
        },
        EventPayload::Agent(AgentEvent::Final { run_id, text }) => {
            UiEventKind::AgentFinal { run_id, text }
        }
        EventPayload::Tool(ToolEvent::Started { name }) => UiEventKind::ToolStarted { name },
        EventPayload::Tool(ToolEvent::Finished { name, summary }) => {
            UiEventKind::ToolFinished { name, summary }
        }
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
        EventPayload::Budget(BudgetEvent::CompactionCompleted {
            compacted_to,
            compaction_count,
        }) => UiEventKind::Notice {
            title: "context compacted".to_owned(),
            message: format!("{compacted_to} tokens · compaction #{compaction_count}"),
            error: false,
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
        },
        EventPayload::Notice(NoticeEvent::PolicyDenied { reason }) => UiEventKind::Notice {
            title: "policy denied".to_owned(),
            message: reason,
            error: true,
        },
        EventPayload::Notice(NoticeEvent::Runtime { message }) => UiEventKind::Notice {
            title: "runtime".to_owned(),
            message,
            error: false,
        },
        EventPayload::Notice(NoticeEvent::Legacy { event_kind, body }) => UiEventKind::Notice {
            title: format!("legacy event · {event_kind}"),
            message: serde_json::to_string_pretty(&body).unwrap_or_else(|_| body.to_string()),
            error: false,
        },
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
    };

    UiEvent {
        sequence: event.sequence,
        at_unix_ms: event.at_unix_ms,
        kind,
    }
}
