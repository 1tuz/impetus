//! ACP adapter для ModelProvider trait.
//!
//! Связывает external coding-agent через AcpGatewayV2 с harness ModelProvider.
//! Agent owns authentication; Impetus owns policy, session state, and orchestration.

use crate::{
    Action, ActionKind, ActionOrigin, ModelProvider, PolicyDecision, PolicyEngine, ProviderError,
    ProviderHealth, ProviderMessage,
};
use agent_client_protocol::AcpAgentConfig;
use async_trait::async_trait;
use impetus_acp_gateway::{
    AcpGatewayV2, GatewayState, PermissionDecision, PermissionKind, PermissionRequest, StreamUpdate,
};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

/// Adapter для ACP gateway V2 как ModelProvider.
#[derive(Debug)]
pub struct AcpAdapter {
    gateway: Arc<AcpGatewayV2>,
    policy: Arc<PolicyEngine>,
    provider_id: String,
    model_id: String,
    workspace_dir: PathBuf,
}

impl AcpAdapter {
    pub fn new(
        config: AcpAgentConfig,
        auth_method_id: Option<String>,
        provider_id: String,
        model_id: String,
        workspace_dir: PathBuf,
        policy: Arc<PolicyEngine>,
    ) -> Self {
        let gateway = AcpGatewayV2::new(config).with_auth_method(auth_method_id);

        Self {
            gateway: Arc::new(gateway),
            policy,
            provider_id,
            model_id,
            workspace_dir,
        }
    }
}

fn action_for_permission(request: &PermissionRequest, workspace: &Path) -> Option<Action> {
    let kind = match request.kind {
        PermissionKind::Read | PermissionKind::Search => ActionKind::ReadFile,
        PermissionKind::Edit | PermissionKind::Delete | PermissionKind::Move => {
            ActionKind::WriteFile
        }
        PermissionKind::Execute => ActionKind::SpawnProcess,
        PermissionKind::Fetch => ActionKind::NetworkConnect,
        PermissionKind::Think | PermissionKind::SwitchMode | PermissionKind::Other => return None,
    };
    let target = match kind {
        ActionKind::ReadFile | ActionKind::WriteFile => request
            .target
            .as_ref()
            .map(|target| target.to_string_lossy().into_owned()),
        ActionKind::SpawnProcess | ActionKind::NetworkConnect => {
            Some(workspace.to_string_lossy().into_owned())
        }
        _ => None,
    };
    Some(Action {
        origin: ActionOrigin::Agent,
        kind,
        summary: request.description.clone(),
        target,
    })
}

#[async_trait]
impl ModelProvider for AcpAdapter {
    fn provider_id(&self) -> &str {
        &self.provider_id
    }

    fn model_id(&self) -> &str {
        &self.model_id
    }

    fn health(&self) -> ProviderHealth {
        // Блокирующая проверка невозможна с async state, возвращаем Unknown
        // Реальный health check делается через отдельный monitoring механизм
        ProviderHealth::Unknown
    }

    async fn stream_messages(
        &self,
        messages: &[ProviderMessage],
        _credential: Option<&str>,
        _runtime: Option<Arc<crate::AgentRuntime>>,
        cancel: CancellationToken,
        mut on_chunk: Box<dyn FnMut(String) -> Result<(), ProviderError> + Send>,
    ) -> Result<(), ProviderError> {
        // Проверяем состояние
        let state = self.gateway.state().await;
        if state == GatewayState::Crashed || state == GatewayState::Incompatible {
            return Err(ProviderError::RequestFailed(format!(
                "agent in bad state: {:?}",
                state
            )));
        }

        // Формируем prompt из messages
        let prompt = messages
            .iter()
            .map(|msg| format!("{}: {}", msg.role(), msg.content()))
            .collect::<Vec<_>>()
            .join("\n\n");

        info!("Starting ACP session with prompt length: {}", prompt.len());

        // Клонируем Arc для spawned task
        let gateway = Arc::clone(&self.gateway);
        let workspace = self.workspace_dir.clone();

        // Запускаем session в отдельной задаче
        let mut session_handle =
            tokio::spawn(async move { gateway.start_session(workspace, prompt).await });

        // Обрабатываем updates и permission requests
        loop {
            tokio::select! {
                _ = cancel.cancelled() => {
                    warn!("ACP session cancelled");
                    if let Err(error) = self.gateway.cancel_active_session().await {
                        debug!("ACP cancellation notification was not acknowledged: {}", error);
                    }
                    session_handle.abort();
                    let _ = session_handle.await;
                    return Err(ProviderError::Cancelled);
                }

                update = self.gateway.recv_update() => {
                    match update {
                        Some(StreamUpdate::Text(text)) => {
                            debug!("Received text chunk: {} chars", text.len());
                            on_chunk(text).map_err(|e| {
                                error!("Chunk callback failed: {:?}", e);
                                e
                            })?;
                        }
                        Some(StreamUpdate::ToolUse { tool_name, status }) => {
                            debug!("Tool use: {} - {}", tool_name, status);
                            // Можно отправить как chunk или пропустить
                        }
                        Some(StreamUpdate::Status(status)) => {
                            debug!("Status update: {}", status);
                        }
                        Some(StreamUpdate::Completed { stop_reason }) => {
                            info!("Session completed: {:?}", stop_reason);
                        }
                        Some(StreamUpdate::Error(err)) => {
                            error!("Agent error: {}", err);
                            return Err(ProviderError::RequestFailed(err));
                        }
                        None => {
                            // Channel closed
                            break;
                        }
                    }
                }

                perm_req = self.gateway.recv_permission_request() => {
                    match perm_req {
                        Some((request, response_tx)) => {
                            warn!(
                                "Permission request received: {} - {}",
                                request.request_id, request.description
                            );

                            // Route через Policy
                            let decision = if let Some(action) = action_for_permission(&request, &self.workspace_dir) {
                                match self.policy.evaluate(&action) {
                                    PolicyDecision::Allow => {
                                        // Выбираем первый AllowOnce/AllowAlways option
                                        let allow_option = request.options.iter().find(|opt| {
                                            matches!(
                                                opt.kind,
                                                impetus_acp_gateway::PermissionChoiceKind::AllowOnce
                                                    | impetus_acp_gateway::PermissionChoiceKind::AllowAlways
                                            )
                                        });

                                        if let Some(opt) = allow_option {
                                            info!("Policy allowed: selecting option {}", opt.option_id);
                                            PermissionDecision::Select(opt.option_id.clone())
                                        } else {
                                            warn!("Policy allowed but no allow option available");
                                            PermissionDecision::Deny
                                        }
                                    }
                                    PolicyDecision::Deny { reason } => {
                                        info!("Policy denied: {}", reason);
                                        PermissionDecision::Deny
                                    }
                                    PolicyDecision::NeedsApproval { reason } => {
                                        info!("Policy requires approval: {}", reason);
                                        // TODO: интеграция с durable approval flow
                                        // Пока отклоняем, требуется approval broker
                                        PermissionDecision::Deny
                                    }
                                }
                            } else {
                                // Think/SwitchMode/Other — не policy-relevant
                                debug!("Permission kind {:?} не требует Policy evaluation", request.kind);
                                let allow_option = request.options.iter().find(|opt| {
                                    matches!(
                                        opt.kind,
                                        impetus_acp_gateway::PermissionChoiceKind::AllowOnce
                                            | impetus_acp_gateway::PermissionChoiceKind::AllowAlways
                                    )
                                });
                                if let Some(opt) = allow_option {
                                    PermissionDecision::Select(opt.option_id.clone())
                                } else {
                                    PermissionDecision::Deny
                                }
                            };

                            if response_tx.send(decision).is_err() {
                                error!("Failed to send permission decision");
                            }
                        }
                        None => {
                            // Channel closed
                            break;
                        }
                    }
                }

                session_result = &mut session_handle => {
                    match session_result {
                        Ok(Ok(session_id)) => {
                            info!("Session completed: {:?}", session_id);
                            break;
                        }
                        Ok(Err(e)) => {
                            error!("Session failed: {}", e);
                            return Err(ProviderError::RequestFailed(format!("session error: {}", e)));
                        }
                        Err(e) => {
                            error!("Session task panicked: {}", e);
                            return Err(ProviderError::RequestFailed(format!("task panic: {}", e)));
                        }
                    }
                }
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use impetus_acp_gateway::{
        PermissionChoiceKind, PermissionKind, PermissionOption, PermissionRequest,
    };

    #[test]
    fn edit_permission_is_normalized_as_agent_write_action() {
        let workspace = tempfile::tempdir().expect("workspace");
        let target = workspace.path().join("file.txt");
        let request = PermissionRequest {
            request_id: "permission-1".into(),
            description: "Edit file".into(),
            kind: PermissionKind::Edit,
            target: Some(target.clone()),
            options: vec![PermissionOption {
                option_id: "allow-once".into(),
                description: "Allow once".into(),
                kind: PermissionChoiceKind::AllowOnce,
            }],
        };

        let action = action_for_permission(&request, workspace.path()).expect("known action");

        assert_eq!(action.origin, crate::ActionOrigin::Agent);
        assert_eq!(action.kind, crate::ActionKind::WriteFile);
        assert_eq!(action.target.as_deref(), target.to_str());
    }
}
