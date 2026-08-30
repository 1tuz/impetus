//! ACP adapter для ModelProvider trait.
//!
//! Связывает external coding-agent через AcpGateway с harness ModelProvider.
//! Один gateway инстанс на profile; все session requests идут через него.

use crate::{ModelProvider, ProviderError, ProviderHealth, ProviderMessage};
use async_trait::async_trait;
use impetus_acp_gateway::{AcpGateway, AgentStatus};
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, warn};

/// Adapter для ACP gateway как ModelProvider.
#[derive(Debug)]
pub struct AcpAdapter {
    gateway: Arc<Mutex<AcpGateway>>,
    provider_id: String,
    model_id: String,
}

impl AcpAdapter {
    pub fn new(gateway: AcpGateway) -> Self {
        let provider_id = gateway.profile().id.clone();
        let model_id = gateway.profile().display_name.clone();

        Self {
            gateway: Arc::new(Mutex::new(gateway)),
            provider_id,
            model_id,
        }
    }

    /// Запускает gateway если ещё не запущен.
    async fn ensure_started(&self) -> Result<(), ProviderError> {
        let mut gateway = self.gateway.lock().await;

        match gateway.status() {
            AgentStatus::NotStarted => {
                debug!("Starting ACP agent: {}", self.provider_id);
                gateway.start().await.map_err(|e| {
                    error!("Failed to start ACP agent: {}", e);
                    ProviderError::RequestFailed(format!("agent start failed: {}", e))
                })?;

                // Initialize ACP protocol
                gateway.initialize().await.map_err(|e| {
                    error!("Failed to initialize ACP agent: {}", e);
                    ProviderError::RequestFailed(format!("agent init failed: {}", e))
                })?;

                debug!("ACP agent initialized: {}", self.provider_id);
                Ok(())
            }
            AgentStatus::Connected => Ok(()),
            AgentStatus::Initializing => {
                // Ждём завершения инициализации
                tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
                Ok(())
            }
            status => Err(ProviderError::RequestFailed(format!(
                "agent in unexpected state: {:?}",
                status
            ))),
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
        // Блокирующая проверка статуса
        let gateway = match self.gateway.try_lock() {
            Ok(g) => g,
            Err(_) => return ProviderHealth::Unknown,
        };

        match gateway.status() {
            AgentStatus::Connected => ProviderHealth::Healthy,
            AgentStatus::NotStarted | AgentStatus::Initializing => ProviderHealth::Unknown,
            AgentStatus::Unavailable | AgentStatus::Incompatible | AgentStatus::Crashed => {
                ProviderHealth::Unavailable {
                    last_error_redacted: "agent unavailable".into(),
                }
            }
        }
    }

    async fn stream_messages(
        &self,
        messages: &[ProviderMessage],
        _credential: Option<&str>,
        cancel: CancellationToken,
        mut on_chunk: Box<dyn FnMut(String) -> Result<(), ProviderError> + Send>,
    ) -> Result<(), ProviderError> {
        self.ensure_started().await?;

        // Конвертируем messages в ACP format
        let acp_messages: Vec<serde_json::Value> = messages
            .iter()
            .map(|msg| {
                serde_json::json!({
                    "role": msg.role(),
                    "content": msg.content(),
                })
            })
            .collect();

        let params = serde_json::json!({
            "messages": acp_messages,
        });

        let mut gateway = self.gateway.lock().await;

        // Отправляем chat/completions request
        let response = tokio::select! {
            result = gateway.send_request("chat/completions", Some(params)) => {
                result.map_err(|e| {
                    error!("ACP request failed: {}", e);
                    ProviderError::RequestFailed(format!("acp error: {}", e))
                })?
            }
            _ = cancel.cancelled() => {
                warn!("ACP request cancelled");
                return Err(ProviderError::Cancelled);
            }
        };

        if let Some(error) = response.error {
            return Err(ProviderError::RequestFailed(format!(
                "agent error: {}",
                error.message
            )));
        }

        let result = response
            .result
            .ok_or_else(|| ProviderError::RequestFailed("no result from agent".into()))?;

        // Извлекаем content из ответа
        let content = result
            .get("choices")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("message"))
            .and_then(|m| m.get("content"))
            .and_then(|c| c.as_str())
            .ok_or_else(|| ProviderError::RequestFailed("invalid response format".into()))?;

        // Передаём chunks (можно разбить на token-based chunks)
        on_chunk(content.to_string())?;

        Ok(())
    }
}
