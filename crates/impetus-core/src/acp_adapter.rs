//! ACP adapter для ModelProvider trait.
//!
//! Связывает external coding-agent через AcpGatewayV2 с harness ModelProvider.
//! Agent owns authentication; Impetus owns policy, session state, and orchestration.

use crate::{ModelProvider, ProviderError, ProviderHealth, ProviderMessage};
use agent_client_protocol::AcpAgentConfig;
use async_trait::async_trait;
use impetus_acp_gateway::{AcpGatewayV2, GatewayState, PermissionDecision, StreamUpdate};
use std::path::PathBuf;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

/// Adapter для ACP gateway V2 как ModelProvider.
#[derive(Debug)]
pub struct AcpAdapter {
    gateway: Arc<AcpGatewayV2>,
    provider_id: String,
    model_id: String,
    workspace_dir: PathBuf,
}

impl AcpAdapter {
    pub fn new(
        config: AcpAgentConfig,
        provider_id: String,
        model_id: String,
        workspace_dir: PathBuf,
    ) -> Self {
        let gateway = AcpGatewayV2::new(config);

        Self {
            gateway: Arc::new(gateway),
            provider_id,
            model_id,
            workspace_dir,
        }
    }
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
                            break;
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

                            // TODO: route через Policy
                            // Пока всё отклоняем — требуется approval flow
                            let decision = PermissionDecision::Deny;

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
