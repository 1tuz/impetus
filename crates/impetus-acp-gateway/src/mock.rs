//! Mock ACP agent для тестирования protocol smoke.
//!
//! Эмулирует initialize/session/stream/cancel/permission/exit без реального
//! coding-agent. Stdout — JSON-RPC, stderr — logs.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Базовые JSON-RPC 2.0 типы для mock agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum JsonRpcMessage {
    Request(JsonRpcRequest),
    Response(JsonRpcResponse),
    Notification(JsonRpcNotification),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: String,
    pub id: serde_json::Value,
    pub method: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: String,
    pub id: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcNotification {
    pub jsonrpc: String,
    pub method: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcError {
    pub code: i32,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

/// Mock ACP agent state machine.
pub struct MockAgent {
    initialized: bool,
    sessions: HashMap<String, MockSession>,
    pending_auth: Option<String>,
}

#[derive(Debug)]
struct MockSession {
    #[allow(dead_code)]
    session_id: String,
    active: bool,
}

impl MockAgent {
    pub fn new() -> Self {
        Self {
            initialized: false,
            sessions: HashMap::new(),
            pending_auth: None,
        }
    }

    /// Обрабатывает входящий JSON-RPC request и возвращает response.
    pub fn handle_request(
        &mut self,
        request: JsonRpcRequest,
    ) -> Result<JsonRpcResponse, MockAgentError> {
        match request.method.as_str() {
            "initialize" => self.handle_initialize(request),
            "session/create" => self.handle_session_create(request),
            "session/cancel" => self.handle_session_cancel(request),
            "exit" => self.handle_exit(request),
            _ => Ok(JsonRpcResponse {
                jsonrpc: "2.0".into(),
                id: request.id,
                result: None,
                error: Some(JsonRpcError {
                    code: -32601,
                    message: format!("Method not found: {}", request.method),
                    data: None,
                }),
            }),
        }
    }

    /// Генерирует auth/requestCredential notification для agent-owned login.
    pub fn request_credential(&mut self, prompt: &str) -> JsonRpcNotification {
        let request_id = uuid::Uuid::new_v4().to_string();
        self.pending_auth = Some(request_id.clone());

        JsonRpcNotification {
            jsonrpc: "2.0".into(),
            method: "auth/requestCredential".into(),
            params: Some(serde_json::json!({
                "requestId": request_id,
                "prompt": prompt,
            })),
        }
    }

    /// Обрабатывает response на auth/requestCredential.
    pub fn handle_credential_response(
        &mut self,
        response: JsonRpcResponse,
    ) -> Result<Option<String>, MockAgentError> {
        if response.error.is_some() {
            self.pending_auth = None;
            return Ok(None);
        }

        let credential = response.result.and_then(|v| {
            v.get("credential")
                .and_then(|c| c.as_str().map(String::from))
        });

        self.pending_auth = None;
        Ok(credential)
    }

    fn handle_initialize(
        &mut self,
        request: JsonRpcRequest,
    ) -> Result<JsonRpcResponse, MockAgentError> {
        self.initialized = true;

        let result = serde_json::json!({
            "protocolVersion": "2.0",
            "capabilities": {
                "session": true,
                "streaming": true,
                "permission": true
            },
            "serverInfo": {
                "name": "mock-acp-agent",
                "version": "0.1.0"
            }
        });

        Ok(JsonRpcResponse {
            jsonrpc: "2.0".into(),
            id: request.id,
            result: Some(result),
            error: None,
        })
    }

    fn handle_session_create(
        &mut self,
        request: JsonRpcRequest,
    ) -> Result<JsonRpcResponse, MockAgentError> {
        if !self.initialized {
            return Ok(JsonRpcResponse {
                jsonrpc: "2.0".into(),
                id: request.id,
                result: None,
                error: Some(JsonRpcError {
                    code: -32600,
                    message: "Not initialized".into(),
                    data: None,
                }),
            });
        }

        let session_id = uuid::Uuid::new_v4().to_string();
        self.sessions.insert(
            session_id.clone(),
            MockSession {
                session_id: session_id.clone(),
                active: true,
            },
        );

        let result = serde_json::json!({
            "sessionId": session_id,
        });

        Ok(JsonRpcResponse {
            jsonrpc: "2.0".into(),
            id: request.id,
            result: Some(result),
            error: None,
        })
    }

    fn handle_session_cancel(
        &mut self,
        request: JsonRpcRequest,
    ) -> Result<JsonRpcResponse, MockAgentError> {
        let params = request
            .params
            .as_ref()
            .ok_or(MockAgentError::MissingParams)?;
        let session_id = params
            .get("sessionId")
            .and_then(|v| v.as_str())
            .ok_or(MockAgentError::InvalidParams)?;

        if let Some(session) = self.sessions.get_mut(session_id) {
            session.active = false;
        }

        Ok(JsonRpcResponse {
            jsonrpc: "2.0".into(),
            id: request.id,
            result: Some(serde_json::json!({})),
            error: None,
        })
    }

    fn handle_exit(&mut self, request: JsonRpcRequest) -> Result<JsonRpcResponse, MockAgentError> {
        self.initialized = false;
        self.sessions.clear();

        Ok(JsonRpcResponse {
            jsonrpc: "2.0".into(),
            id: request.id,
            result: Some(serde_json::json!({})),
            error: None,
        })
    }

    pub fn is_initialized(&self) -> bool {
        self.initialized
    }

    pub fn session_count(&self) -> usize {
        self.sessions.len()
    }
}

impl Default for MockAgent {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum MockAgentError {
    #[error("Missing params")]
    MissingParams,

    #[error("Invalid params")]
    InvalidParams,

    #[error("JSON parse error: {0}")]
    JsonParse(#[from] serde_json::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mock_agent_initialize_sequence() {
        let mut agent = MockAgent::new();
        assert!(!agent.is_initialized());

        let request = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: serde_json::json!(1),
            method: "initialize".into(),
            params: Some(serde_json::json!({
                "clientInfo": {"name": "test", "version": "0.1.0"}
            })),
        };

        let response = agent.handle_request(request).expect("handle initialize");
        assert!(response.error.is_none());
        assert!(agent.is_initialized());
    }

    #[test]
    fn mock_agent_session_create_and_cancel() {
        let mut agent = MockAgent::new();

        // Initialize
        let init_req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: serde_json::json!(1),
            method: "initialize".into(),
            params: None,
        };
        agent.handle_request(init_req).unwrap();

        // Create session
        let create_req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: serde_json::json!(2),
            method: "session/create".into(),
            params: Some(serde_json::json!({"prompt": "test"})),
        };
        let create_resp = agent.handle_request(create_req).unwrap();
        assert!(create_resp.error.is_none());
        assert_eq!(agent.session_count(), 1);

        let session_id = create_resp.result.unwrap()["sessionId"]
            .as_str()
            .unwrap()
            .to_string();

        // Cancel session
        let cancel_req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: serde_json::json!(3),
            method: "session/cancel".into(),
            params: Some(serde_json::json!({"sessionId": session_id})),
        };
        let cancel_resp = agent.handle_request(cancel_req).unwrap();
        assert!(cancel_resp.error.is_none());
    }

    #[test]
    fn mock_agent_rejects_unknown_method() {
        let mut agent = MockAgent::new();

        let request = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: serde_json::json!(1),
            method: "unknown/method".into(),
            params: None,
        };

        let response = agent.handle_request(request).unwrap();
        assert!(response.error.is_some());
        assert_eq!(response.error.unwrap().code, -32601);
    }

    #[test]
    fn mock_agent_exit_clears_state() {
        let mut agent = MockAgent::new();

        // Initialize and create session
        let init = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: serde_json::json!(1),
            method: "initialize".into(),
            params: None,
        };
        agent.handle_request(init).unwrap();

        let create = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: serde_json::json!(2),
            method: "session/create".into(),
            params: Some(serde_json::json!({})),
        };
        agent.handle_request(create).unwrap();
        assert_eq!(agent.session_count(), 1);

        // Exit
        let exit = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: serde_json::json!(3),
            method: "exit".into(),
            params: None,
        };
        agent.handle_request(exit).unwrap();

        assert!(!agent.is_initialized());
        assert_eq!(agent.session_count(), 0);
    }

    #[test]
    fn mock_agent_credential_flow() {
        let mut agent = MockAgent::new();

        // Request credential
        let notification = agent.request_credential("Enter API key:");
        assert_eq!(notification.method, "auth/requestCredential");

        let request_id = notification.params.unwrap()["requestId"]
            .as_str()
            .unwrap()
            .to_string();

        // User provides credential
        let response = JsonRpcResponse {
            jsonrpc: "2.0".into(),
            id: serde_json::json!(request_id),
            result: Some(serde_json::json!({"credential": "test-key-123"})),
            error: None,
        };

        let credential = agent.handle_credential_response(response).unwrap();
        assert_eq!(credential, Some("test-key-123".to_string()));
        assert!(agent.pending_auth.is_none());
    }

    #[test]
    fn mock_agent_credential_cancellation() {
        let mut agent = MockAgent::new();

        let notification = agent.request_credential("Enter token:");
        let request_id = notification.params.unwrap()["requestId"]
            .as_str()
            .unwrap()
            .to_string();

        // User cancels
        let response = JsonRpcResponse {
            jsonrpc: "2.0".into(),
            id: serde_json::json!(request_id),
            result: None,
            error: Some(JsonRpcError {
                code: -32000,
                message: "User cancelled".into(),
                data: None,
            }),
        };

        let credential = agent.handle_credential_response(response).unwrap();
        assert_eq!(credential, None);
        assert!(agent.pending_auth.is_none());
    }
}
