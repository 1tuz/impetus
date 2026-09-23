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

// --- Coding / Browser capability dispatch (extension-first; #336) ---
// Core owns contracts + IPC + host dispatch tokens. Concrete Browser CDP /
// WebDriver and language-pack installers live in extensions, not core.

/// Extension `LspIntegration`: go-to-definition.
pub const METHOD_CODING_DEFINITION: &str = "coding/definition";
/// Extension `LspIntegration`: hover.
pub const METHOD_CODING_HOVER: &str = "coding/hover";
/// Extension `LspIntegration`: diagnostics pull / last push.
pub const METHOD_CODING_DIAGNOSTICS: &str = "coding/diagnostics";
/// Extension `LspIntegration`: document symbols.
pub const METHOD_CODING_SYMBOLS: &str = "coding/symbols";
/// Extension `LspIntegration`: cancel in-flight request.
pub const METHOD_CODING_CANCEL: &str = "coding/cancel";
/// Extension `BrowserIntegration`: negotiate (Absent until CDP land).
pub const METHOD_BROWSER_NEGOTIATE: &str = "browser/negotiate";
/// Extension `BrowserIntegration`: health (Absent until CDP land).
pub const METHOD_BROWSER_HEALTH: &str = "browser/health";

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

    #[test]
    fn coding_and_browser_dispatch_tokens_stable() {
        assert_eq!(METHOD_CODING_DEFINITION, "coding/definition");
        assert_eq!(METHOD_CODING_HOVER, "coding/hover");
        assert_eq!(METHOD_CODING_DIAGNOSTICS, "coding/diagnostics");
        assert_eq!(METHOD_CODING_SYMBOLS, "coding/symbols");
        assert_eq!(METHOD_CODING_CANCEL, "coding/cancel");
        assert_eq!(METHOD_BROWSER_NEGOTIATE, "browser/negotiate");
        assert_eq!(METHOD_BROWSER_HEALTH, "browser/health");
    }
}
