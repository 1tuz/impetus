//! host_process JSON-RPC protocol (stdio, newline-delimited JSON).
//!
//! Authors of `host_process` extensions speak this ABI. Framing is one JSON
//! object per line (UTF-8). No Content-Length (MCP-style) in v1 — keep fixtures
//! trivial.
//!
//! Surface: `initialize` / `shutdown` / `ping` / `operate` / `cancel`.
//! No shell, no raw secrets, no CDP/LSP payloads in the core contract —
//! extensions implement those behind typed `operate` ops gated by manifest
//! permissions.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::permissions::ExtensionPermission;

/// Host ↔ extension process protocol major.
pub const HOST_PROTOCOL_VERSION: u32 = 1;

/// Hard cap on a single newline-delimited JSON RPC line (request or response).
pub const MAX_HOST_RPC_LINE_BYTES: usize = 256 * 1024;

/// Default host-side wait for `extension/operate` when caller omits timeout.
pub const DEFAULT_OPERATE_TIMEOUT_MS: u64 = 30_000;

/// Initialize the child after spawn.
pub const METHOD_INITIALIZE: &str = "extension/initialize";
/// Graceful shutdown before the host kills the child.
pub const METHOD_SHUTDOWN: &str = "extension/shutdown";
/// Liveness probe (optional for children).
pub const METHOD_PING: &str = "extension/ping";
/// Typed capability invoke (request id + op + bounded params).
pub const METHOD_OPERATE: &str = "extension/operate";
/// Cancel an in-flight operate by `request_id`.
pub const METHOD_CANCEL: &str = "extension/cancel";

/// Well-known operate op names (extensions may advertise more via initialize).
pub mod ops {
    /// Echo / fixture liveness — no extra manifest permission beyond spawn.
    pub const ECHO: &str = "echo";
    /// Generic capability invoke — requires `OperateParams.permission`.
    pub const INVOKE: &str = "invoke";

    // --- Tool / Command (permission declared on pack; no shell) ---
    /// Extension `Tool`: invoke a named tool with JSON params.
    pub const TOOL_CALL: &str = "tool/call";
    /// Extension `Command`: invoke a named command (never shell argv).
    pub const COMMAND_INVOKE: &str = "command/invoke";

    // --- LspIntegration (permission `lsp`) ---
    pub const CODING_DEFINITION: &str = super::METHOD_CODING_DEFINITION;
    pub const CODING_REFERENCES: &str = super::METHOD_CODING_REFERENCES;
    pub const CODING_HOVER: &str = super::METHOD_CODING_HOVER;
    pub const CODING_DIAGNOSTICS: &str = super::METHOD_CODING_DIAGNOSTICS;
    pub const CODING_SYMBOLS: &str = super::METHOD_CODING_SYMBOLS;
    pub const CODING_CANCEL: &str = super::METHOD_CODING_CANCEL;

    // --- BrowserIntegration (permission `browser`) ---
    pub const BROWSER_NEGOTIATE: &str = super::METHOD_BROWSER_NEGOTIATE;
    pub const BROWSER_HEALTH: &str = super::METHOD_BROWSER_HEALTH;

    // --- MemoryProvider (permission `memory`) — optional long-term; not MemoryStore ---
    pub const MEMORY_RECALL: &str = "memory/recall";
    pub const MEMORY_STORE: &str = "memory/store";

    // --- ContextProvider (permission `filesystem_read` or pack-declared) ---
    pub const CONTEXT_CONTRIBUTE: &str = "context/contribute";
}

/// JSON-RPC application error codes for operate / cancel (host + child).
pub mod error_codes {
    pub const DENIED: i64 = -32010;
    pub const TIMEOUT: i64 = -32011;
    pub const CANCELLED: i64 = -32012;
    pub const UNSUPPORTED_OP: i64 = -32013;
    pub const PAYLOAD_TOO_LARGE: i64 = -32014;
    pub const CRASHED: i64 = -32015;
    pub const INVALID_PARAMS: i64 = -32016;
    pub const SECRETS_FORBIDDEN: i64 = -32017;
}

/// Object keys forbidden in operate params (labels only — never raw secrets).
pub const FORBIDDEN_SECRET_KEYS: &[&str] = &[
    "token",
    "access_token",
    "refresh_token",
    "password",
    "passwd",
    "api_key",
    "apikey",
    "secret",
    "private_key",
    "passphrase",
    "authorization",
    "auth_header",
    "bearer",
    "credentials",
];

// --- Coding / Browser capability dispatch (extension-first; #336) ---
// Core owns contracts + IPC + host dispatch tokens. Concrete Browser CDP /
// WebDriver and language-pack installers live in extensions, not core (Won't).

/// Extension `LspIntegration`: go-to-definition.
pub const METHOD_CODING_DEFINITION: &str = "coding/definition";
/// Extension `LspIntegration`: find references.
pub const METHOD_CODING_REFERENCES: &str = "coding/references";
/// Extension `LspIntegration`: hover.
pub const METHOD_CODING_HOVER: &str = "coding/hover";
/// Extension `LspIntegration`: diagnostics pull / last push.
pub const METHOD_CODING_DIAGNOSTICS: &str = "coding/diagnostics";
/// Extension `LspIntegration`: document symbols.
pub const METHOD_CODING_SYMBOLS: &str = "coding/symbols";
/// Extension `LspIntegration`: cancel in-flight request.
pub const METHOD_CODING_CANCEL: &str = "coding/cancel";
/// Extension `BrowserIntegration`: negotiate (Absent without Active pack).
pub const METHOD_BROWSER_NEGOTIATE: &str = "browser/negotiate";
/// Extension `BrowserIntegration`: health (Absent without Active pack).
pub const METHOD_BROWSER_HEALTH: &str = "browser/health";

/// `extension/initialize` params.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InitializeParams {
    pub protocol_version: u32,
    pub extension_id: String,
    pub extension_api_version: u32,
}

/// `extension/initialize` result (compat negotiate).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InitializeResult {
    pub protocol_version: u32,
    #[serde(default)]
    pub name: Option<String>,
    /// API version the child will speak (must fall in host supported range).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extension_api_version: Option<u32>,
    /// Ops the child advertises. Empty = host does not pre-filter (legacy).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub supported_ops: Vec<String>,
}

/// `extension/operate` params.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OperateParams {
    /// Caller-supplied correlation id (stable across cancel).
    pub request_id: String,
    /// Operation name (`echo`, `invoke`, or extension-defined).
    pub op: String,
    #[serde(default)]
    pub params: Value,
    /// Manifest permission required for this op (host gates before dispatch).
    /// Required for every op except [`ops::ECHO`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission: Option<ExtensionPermission>,
}

/// `extension/operate` success result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OperateResult {
    pub request_id: String,
    pub op: String,
    #[serde(default)]
    pub data: Value,
}

/// `extension/cancel` params.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CancelParams {
    pub request_id: String,
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

/// Reject oversized RPC lines before write / after read.
pub fn check_rpc_line_size(line: &str) -> Result<(), String> {
    if line.len() > MAX_HOST_RPC_LINE_BYTES {
        return Err(format!(
            "RPC line {} bytes exceeds limit {MAX_HOST_RPC_LINE_BYTES}",
            line.len()
        ));
    }
    Ok(())
}

/// Walk JSON and reject known secret-bearing object keys (case-insensitive).
pub fn reject_secret_keys(value: &Value) -> Result<(), String> {
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                let lower = key.to_ascii_lowercase();
                if FORBIDDEN_SECRET_KEYS
                    .iter()
                    .any(|forbidden| lower == *forbidden || lower.contains(forbidden))
                {
                    return Err(format!(
                        "operate params must not carry secret field `{key}` (labels only)"
                    ));
                }
                reject_secret_keys(child)?;
            }
            Ok(())
        }
        Value::Array(items) => {
            for item in items {
                reject_secret_keys(item)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

/// Host-side permission gate before `extension/operate` dispatch.
///
/// - `echo` needs no extra permission (process already Active).
/// - every other op requires `permission` present **and** declared on manifest.
/// - op name `shell` / `exec` / opaque shell always denied.
pub fn gate_operate_permission(
    op: &str,
    permission: Option<ExtensionPermission>,
    declared: &[ExtensionPermission],
) -> Result<(), String> {
    let op = op.trim();
    if op.is_empty() {
        return Err("operate op must be non-empty".into());
    }
    let lower = op.to_ascii_lowercase();
    if matches!(
        lower.as_str(),
        "shell" | "exec" | "system" | "bash" | "sh" | "zsh" | "cmd" | "powershell"
    ) {
        return Err(format!("operate refuses shell-like op `{op}`"));
    }
    if op == ops::ECHO {
        return Ok(());
    }
    let Some(required) = permission else {
        return Err(format!(
            "operate op `{op}` requires a manifest permission token"
        ));
    };
    if !declared.contains(&required) {
        return Err(format!(
            "operate op `{op}` needs permission `{}` not declared on package",
            required.as_str()
        ));
    }
    Ok(())
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

    #[test]
    fn operate_roundtrip() {
        let req = JsonRpcRequest::new(
            2,
            METHOD_OPERATE,
            OperateParams {
                request_id: "r1".into(),
                op: ops::ECHO.into(),
                params: serde_json::json!({"message": "hi"}),
                permission: None,
            },
        );
        let line = serde_json::to_string(&req).unwrap();
        check_rpc_line_size(&line).unwrap();
        let back: JsonRpcRequest<OperateParams> = serde_json::from_str(&line).unwrap();
        let p = back.params.unwrap();
        assert_eq!(p.request_id, "r1");
        assert_eq!(p.op, ops::ECHO);
    }

    #[test]
    fn reject_secret_keys_blocks_token() {
        let err = reject_secret_keys(&serde_json::json!({"token": "x"})).unwrap_err();
        assert!(err.contains("secret field"));
        assert!(reject_secret_keys(&serde_json::json!({"label": "keychain:foo"})).is_ok());
    }

    #[test]
    fn gate_echo_ok_invoke_needs_perm() {
        assert!(gate_operate_permission(ops::ECHO, None, &[]).is_ok());
        let err = gate_operate_permission(ops::INVOKE, None, &[ExtensionPermission::Browser])
            .unwrap_err();
        assert!(err.contains("requires a manifest permission"));
        assert!(
            gate_operate_permission(
                ops::INVOKE,
                Some(ExtensionPermission::Browser),
                &[ExtensionPermission::Browser],
            )
            .is_ok()
        );
        let denied = gate_operate_permission(
            ops::INVOKE,
            Some(ExtensionPermission::Lsp),
            &[ExtensionPermission::Browser],
        )
        .unwrap_err();
        assert!(denied.contains("not declared"));
        assert!(gate_operate_permission("shell", None, &[]).is_err());
    }

    #[test]
    fn line_size_limit() {
        let big = "x".repeat(MAX_HOST_RPC_LINE_BYTES + 1);
        assert!(check_rpc_line_size(&big).is_err());
    }
}
