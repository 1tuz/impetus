//! Minimal host_process JSON-RPC protocol (stdio, newline-delimited JSON).
//!
//! Authors of `host_process` extensions speak this ABI. Framing is one JSON
//! object per line (UTF-8). No Content-Length (MCP-style) in v1 — keep fixtures
//! trivial.

use serde::{Deserialize, Serialize};

/// Host ↔ extension process protocol major.
pub const HOST_PROTOCOL_VERSION: u32 = 1;

/// Initialize the child after spawn.
pub const METHOD_INITIALIZE: &str = "extension/initialize";
/// Graceful shutdown before the host kills the child.
pub const METHOD_SHUTDOWN: &str = "extension/shutdown";
/// Liveness probe (optional for children).
pub const METHOD_PING: &str = "extension/ping";

/// `extension/initialize` params.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InitializeParams {
    pub protocol_version: u32,
    pub extension_id: String,
    pub extension_api_version: u32,
}

/// `extension/initialize` result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InitializeResult {
    pub protocol_version: u32,
    #[serde(default)]
    pub name: Option<String>,
}

/// JSON-RPC 2.0 request envelope (subset).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JsonRpcRequest<T> {
    pub jsonrpc: String,
    pub id: u64,
    pub method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<T>,
}

impl<T> JsonRpcRequest<T> {
    pub fn new(id: u64, method: impl Into<String>, params: T) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            id,
            method: method.into(),
            params: Some(params),
        }
    }
}

/// JSON-RPC 2.0 success-or-error response (subset).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JsonRpcResponse<T> {
    pub jsonrpc: String,
    pub id: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<T>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

/// JSON-RPC error object.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JsonRpcError {
    pub code: i64,
    pub message: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initialize_roundtrip() {
        let req = JsonRpcRequest::new(
            1,
            METHOD_INITIALIZE,
            InitializeParams {
                protocol_version: HOST_PROTOCOL_VERSION,
                extension_id: "demo".into(),
                extension_api_version: 1,
            },
        );
        let line = serde_json::to_string(&req).unwrap();
        let back: JsonRpcRequest<InitializeParams> = serde_json::from_str(&line).unwrap();
        assert_eq!(back.method, METHOD_INITIALIZE);
        assert_eq!(back.params.unwrap().protocol_version, 1);
    }
}
