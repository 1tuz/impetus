//! Minimal local MCP server-config contract (`impetus.mcp.v1`).
//!
//! Vertical slice: validated stdio config envelope for install/plan paths.
//! Not a full MCP JSON-RPC catalog; no HTTP/SSE transport implementation.

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::extension_compat::{McpCapabilities, McpModule, McpTransport};
use crate::schema::{SCHEMA_MCP, SchemaValidationError, require_version, validate_envelope};

/// Documented schema id for the MCP config contract.
pub const MCP_SCHEMA_ID: &str = SCHEMA_MCP.id;

/// Current MCP config schema version.
pub const MCP_SCHEMA_VERSION: u16 = SCHEMA_MCP.version;

fn default_mcp_schema_version() -> u16 {
    MCP_SCHEMA_VERSION
}

/// Minimal validated MCP server-config envelope.
///
/// Critical envelope fields match [`SCHEMA_MCP`]. Env is represented as
/// [`env_keys`](Self::env_keys) only — never token/secret values.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpManifest {
    /// Version of this contract. Omitted JSON defaults to v1.
    #[serde(default = "default_mcp_schema_version")]
    pub schema_version: u16,
    /// Stable server id (typically sanitized from config `name`).
    pub id: String,
    pub transport: McpTransport,
    /// Executable for stdio transport (required for local config slice).
    pub command: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    /// Capability flags (tools/resources/prompts/sampling) — no secrets.
    pub capabilities: McpCapabilities,
    /// Env var names or Keychain labels only — never secret values.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub env_keys: Vec<String>,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum McpManifestError {
    #[error("MCP id must be non-empty")]
    EmptyId,
    #[error("MCP command must be non-empty")]
    EmptyCommand,
    #[error("MCP env_keys entry must be non-empty")]
    EmptyEnvKey,
    #[error("duplicate MCP env_keys entry `{key}`")]
    DuplicateEnvKey { key: String },
    #[error(transparent)]
    Schema(#[from] SchemaValidationError),
}

impl McpManifest {
    /// Build a manifest and validate id / command / env_keys / schema envelope.
    pub fn new(
        id: impl Into<String>,
        transport: McpTransport,
        command: impl Into<String>,
        args: Vec<String>,
        capabilities: McpCapabilities,
        env_keys: Vec<String>,
    ) -> Result<Self, McpManifestError> {
        let manifest = Self {
            schema_version: MCP_SCHEMA_VERSION,
            id: id.into(),
            transport,
            command: command.into(),
            args,
            capabilities,
            env_keys,
        };
        manifest.validate()?;
        Ok(manifest)
    }

    /// Project a parsed [`McpModule`] into the `impetus.mcp.v1` envelope.
    ///
    /// Copies env **keys** only — never values from `module.env`.
    pub fn from_module(module: &McpModule) -> Result<Self, McpManifestError> {
        let mut env_keys: Vec<String> = module.env.keys().cloned().collect();
        env_keys.sort();
        Self::new(
            module.name.clone(),
            module.transport,
            module.command.clone(),
            module.args.clone(),
            module.capabilities.clone(),
            env_keys,
        )
    }

    /// Validate field rules and the registered critical envelope.
    pub fn validate(&self) -> Result<(), McpManifestError> {
        if self.id.trim().is_empty() {
            return Err(McpManifestError::EmptyId);
        }
        if self.command.trim().is_empty() {
            return Err(McpManifestError::EmptyCommand);
        }
        validate_env_keys(&self.env_keys)?;
        require_version(&SCHEMA_MCP, self.schema_version)?;
        let value = serde_json::to_value(self).expect("McpManifest serializes");
        validate_envelope(MCP_SCHEMA_ID, &value)?;
        Ok(())
    }
}

/// Env key / label names: non-empty, unique. Values must never appear here.
pub fn validate_env_keys(env_keys: &[String]) -> Result<(), McpManifestError> {
    let mut seen = std::collections::BTreeSet::new();
    for key in env_keys {
        if key.trim().is_empty() {
            return Err(McpManifestError::EmptyEnvKey);
        }
        if !seen.insert(key.as_str()) {
            return Err(McpManifestError::DuplicateEnvKey { key: key.clone() });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::HashMap;

    fn sample_caps() -> McpCapabilities {
        McpCapabilities {
            tools: true,
            resources: false,
            prompts: false,
            sampling: false,
        }
    }

    #[test]
    fn valid_manifest_round_trips_and_validates() {
        let manifest = McpManifest::new(
            "filesystem",
            McpTransport::Stdio,
            "npx",
            vec![
                "-y".into(),
                "@modelcontextprotocol/server-filesystem".into(),
            ],
            sample_caps(),
            vec!["HOME".into(), "keychain:mcp-fs-label".into()],
        )
        .expect("valid");
        assert_eq!(manifest.schema_version, MCP_SCHEMA_VERSION);
        assert_eq!(MCP_SCHEMA_ID, "impetus.mcp.v1");

        let value = serde_json::to_value(&manifest).expect("serialize");
        let back: McpManifest = serde_json::from_value(value.clone()).expect("deserialize");
        assert_eq!(back, manifest);
        back.validate().expect("round-trip still valid");

        let blob = value.to_string();
        assert!(!blob.contains("sk-"));
        assert!(!blob.contains("Bearer "));
        assert!(!blob.contains("api_token"));
        // Envelope must not carry env values — only keys/labels.
        assert!(blob.contains("env_keys"));
        assert!(!blob.contains("\"env\""));
    }

    #[test]
    fn from_module_keeps_env_keys_only() {
        let mut env = HashMap::new();
        env.insert("HOME".into(), "/tmp".into());
        env.insert(
            "keychain:mcp-label".into(),
            "never-a-secret-in-tests".into(),
        );
        let module = McpModule {
            name: "filesystem".into(),
            command: "true".into(),
            args: vec![],
            env,
            transport: McpTransport::Stdio,
            capabilities: sample_caps(),
        };
        let manifest = McpManifest::from_module(&module).expect("from module");
        assert_eq!(
            manifest.env_keys,
            vec!["HOME".to_string(), "keychain:mcp-label".to_string()]
        );
        let blob = serde_json::to_string(&manifest).expect("serialize");
        assert!(!blob.contains("/tmp"));
        assert!(!blob.contains("never-a-secret"));
        assert!(!blob.contains("sk-"));
    }

    #[test]
    fn unknown_critical_field_rejected_via_envelope() {
        let value = json!({
            "schema_version": 1,
            "id": "x",
            "transport": "stdio",
            "command": "true",
            "capabilities": {
                "tools": true,
                "resources": false,
                "prompts": false,
                "sampling": false
            },
            "api_token": "should-never-appear"
        });
        let err = validate_envelope(MCP_SCHEMA_ID, &value).unwrap_err();
        match err {
            SchemaValidationError::UnknownCriticalField { id, field } => {
                assert_eq!(id, "impetus.mcp.v1");
                assert_eq!(field, "api_token");
            }
            other => panic!("expected UnknownCriticalField, got {other:?}"),
        }
    }

    #[test]
    fn env_map_at_top_level_rejected_as_unknown_field() {
        let value = json!({
            "schema_version": 1,
            "id": "x",
            "transport": "stdio",
            "command": "true",
            "capabilities": sample_caps(),
            "env": { "HOME": "/tmp" }
        });
        let err = validate_envelope(MCP_SCHEMA_ID, &value).unwrap_err();
        match err {
            SchemaValidationError::UnknownCriticalField { id, field } => {
                assert_eq!(id, "impetus.mcp.v1");
                assert_eq!(field, "env");
            }
            other => panic!("expected UnknownCriticalField for env, got {other:?}"),
        }
    }

    #[test]
    fn empty_id_or_command_rejected() {
        assert!(matches!(
            McpManifest::new(
                "",
                McpTransport::Stdio,
                "true",
                vec![],
                sample_caps(),
                vec![]
            ),
            Err(McpManifestError::EmptyId)
        ));
        assert!(matches!(
            McpManifest::new(
                "x",
                McpTransport::Stdio,
                "  ",
                vec![],
                sample_caps(),
                vec![]
            ),
            Err(McpManifestError::EmptyCommand)
        ));
    }

    #[test]
    fn env_keys_reject_empty_and_duplicate() {
        assert!(matches!(
            validate_env_keys(&["".into()]),
            Err(McpManifestError::EmptyEnvKey)
        ));
        assert!(matches!(
            validate_env_keys(&["HOME".into(), "HOME".into()]),
            Err(McpManifestError::DuplicateEnvKey { .. })
        ));
        assert!(validate_env_keys(&["HOME".into(), "PATH".into()]).is_ok());
    }
}
