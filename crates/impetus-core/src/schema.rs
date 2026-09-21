//! Shared registry for versioned canonical Impetus payload schemas.
//!
//! Convention:
//! - Stable string id: `impetus.<name>.vN` (see [`SchemaSpec::id`])
//! - Numeric field on payloads: `schema_version` (u16)
//! - Evolution: bump version; reject mismatch and unknown critical fields
//! - Provider/harness-specific details nest under [`NEST_PROVIDER`] / [`NEST_HARNESS`]
//!   — never as common top-level fields

use serde_json::Value;
use thiserror::Error;

/// Nest container for provider-specific envelope details.
pub const NEST_PROVIDER: &str = "provider";

/// Nest container for harness-specific envelope details.
pub const NEST_HARNESS: &str = "harness";

/// Provider-owned keys forbidden at the common top level (must nest under [`NEST_PROVIDER`]).
pub const PROVIDER_NEST_KEYS: &[&str] = &[
    "model",
    "model_id",
    "provider_id",
    "provider_profile",
    "base_url",
    "api_base",
    "endpoint",
    "temperature",
    "max_tokens",
    "top_p",
    "api_key",
    "api_token",
    "api_key_ref",
    "auth_ref",
    "openai_http_api",
];

/// Harness-owned keys forbidden at the common top level (must nest under [`NEST_HARNESS`]).
pub const HARNESS_NEST_KEYS: &[&str] = &[
    "harness_id",
    "harness_version",
    "seatbelt",
    "sandbox_profile",
    "worktree",
    "worktree_path",
    "seatbelt_process_wrap",
];

/// One registered canonical schema (id + current numeric version).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SchemaSpec {
    /// Stable string clients may advertise or log (e.g. `impetus.approval_detail.v1`).
    pub id: &'static str,
    /// Current numeric `schema_version` for this id.
    pub version: u16,
    /// Field names allowed on critical envelopes (unknown keys rejected).
    pub critical_fields: &'static [&'static str],
}

/// ApprovalDetail IPC UI contract (`GetApprovalDetail`).
pub const SCHEMA_APPROVAL_DETAIL: SchemaSpec = SchemaSpec {
    id: "impetus.approval_detail.v1",
    version: 1,
    critical_fields: &[
        "schema_version",
        "request",
        "diff_preview",
        "affected_files",
        "estimated_scope",
        "attachment_refs",
        NEST_PROVIDER,
        NEST_HARNESS,
    ],
};

/// Capability truth snapshot (`doctor` / Diagnostics matrix).
pub const SCHEMA_CAPABILITIES: SchemaSpec = SchemaSpec {
    id: "impetus.capabilities.v1",
    version: 1,
    critical_fields: &[
        "schema_version",
        "capabilities",
        NEST_PROVIDER,
        NEST_HARNESS,
    ],
};

/// Session-ish envelope slice for nest validation (common fields + provider/harness nests).
///
/// Full session catalog remains Planned; this registers only the critical top-level shape
/// so leaked provider/harness keys fail the same way as capabilities.
pub const SCHEMA_SESSION: SchemaSpec = SchemaSpec {
    id: "impetus.session.v1",
    version: 1,
    critical_fields: &["schema_version", "session_id", NEST_PROVIDER, NEST_HARNESS],
};

/// Minimal extension manifest (`plan_install` / Skill + MCP config).
pub const SCHEMA_EXTENSION: SchemaSpec = SchemaSpec {
    id: "impetus.extension.v1",
    version: 1,
    critical_fields: &[
        "schema_version",
        "id",
        "kind",
        "version",
        "digest",
        "capabilities",
    ],
};

/// Minimal local MCP server-config envelope (stdio config shape; not full MCP RPC).
pub const SCHEMA_MCP: SchemaSpec = SchemaSpec {
    id: "impetus.mcp.v1",
    version: 1,
    critical_fields: &[
        "schema_version",
        "id",
        "transport",
        "command",
        "args",
        "capabilities",
        "env_keys",
    ],
};

/// All schemas known to this crate build. Order is stable for tests/docs.
pub const KNOWN_SCHEMAS: &[SchemaSpec] = &[
    SCHEMA_APPROVAL_DETAIL,
    SCHEMA_CAPABILITIES,
    SCHEMA_SESSION,
    SCHEMA_EXTENSION,
    SCHEMA_MCP,
];

#[derive(Debug, Error, PartialEq, Eq)]
pub enum SchemaValidationError {
    #[error("unknown schema id `{id}`")]
    UnknownSchema { id: String },
    #[error("schema `{id}` version mismatch: expected {expected}, got {actual}")]
    VersionMismatch {
        id: String,
        expected: u16,
        actual: u16,
    },
    #[error("schema `{id}` has unknown critical field `{field}`")]
    UnknownCriticalField { id: String, field: String },
    #[error(
        "schema `{id}` field `{field}` must nest under `{nest_under}` (provider/harness details are not common fields)"
    )]
    LeakedNestedField {
        id: String,
        field: String,
        nest_under: String,
    },
    #[error("schema `{id}` nest `{nest}` must be a JSON object")]
    NestNotAnObject { id: String, nest: String },
    #[error("schema `{id}` payload must be a JSON object")]
    NotAnObject { id: String },
}

/// Look up a registered schema by stable id.
pub fn lookup(schema_id: &str) -> Option<&'static SchemaSpec> {
    KNOWN_SCHEMAS.iter().find(|spec| spec.id == schema_id)
}

/// Fail clearly when `schema_version` does not match the registered spec.
pub fn require_version(
    spec: &SchemaSpec,
    schema_version: u16,
) -> Result<(), SchemaValidationError> {
    if schema_version != spec.version {
        return Err(SchemaValidationError::VersionMismatch {
            id: spec.id.to_string(),
            expected: spec.version,
            actual: schema_version,
        });
    }
    Ok(())
}

fn nest_under_for_key(key: &str) -> Option<&'static str> {
    if PROVIDER_NEST_KEYS.contains(&key) {
        Some(NEST_PROVIDER)
    } else if HARNESS_NEST_KEYS.contains(&key) {
        Some(NEST_HARNESS)
    } else {
        None
    }
}

fn schema_uses_nests(spec: &SchemaSpec) -> bool {
    spec.critical_fields
        .iter()
        .any(|f| *f == NEST_PROVIDER || *f == NEST_HARNESS)
}

/// Reject provider/harness-owned keys when they appear as common top-level fields.
///
/// No-op for schemas that do not declare nest containers in `critical_fields`
/// (e.g. `impetus.extension.v1`); those still reject unknowns via
/// [`reject_unknown_critical_fields`].
pub fn reject_leaked_nested_fields(
    spec: &SchemaSpec,
    object: &serde_json::Map<String, Value>,
) -> Result<(), SchemaValidationError> {
    if !schema_uses_nests(spec) {
        return Ok(());
    }
    for key in object.keys() {
        if let Some(nest_under) = nest_under_for_key(key) {
            return Err(SchemaValidationError::LeakedNestedField {
                id: spec.id.to_string(),
                field: key.clone(),
                nest_under: nest_under.to_string(),
            });
        }
    }
    Ok(())
}

/// Reject unknown keys on a critical envelope (provider-specific data stays nested).
pub fn reject_unknown_critical_fields(
    spec: &SchemaSpec,
    object: &serde_json::Map<String, Value>,
) -> Result<(), SchemaValidationError> {
    for key in object.keys() {
        if !spec.critical_fields.iter().any(|allowed| *allowed == key) {
            return Err(SchemaValidationError::UnknownCriticalField {
                id: spec.id.to_string(),
                field: key.clone(),
            });
        }
    }
    Ok(())
}

/// When `provider` / `harness` are present, they must be objects (not scalars/arrays).
///
/// No-op for schemas that do not declare nest containers.
pub fn require_nest_objects(
    spec: &SchemaSpec,
    object: &serde_json::Map<String, Value>,
) -> Result<(), SchemaValidationError> {
    if !schema_uses_nests(spec) {
        return Ok(());
    }
    for nest in [NEST_PROVIDER, NEST_HARNESS] {
        if let Some(value) = object.get(nest)
            && !value.is_object()
        {
            return Err(SchemaValidationError::NestNotAnObject {
                id: spec.id.to_string(),
                nest: nest.to_string(),
            });
        }
    }
    Ok(())
}

/// Validate a JSON value as a known critical schema envelope.
///
/// Checks: known id, version match, no leaked provider/harness top-level fields,
/// no unknown top-level fields, nest containers are objects when present.
pub fn validate_envelope(
    schema_id: &str,
    value: &Value,
) -> Result<&'static SchemaSpec, SchemaValidationError> {
    let spec = lookup(schema_id).ok_or_else(|| SchemaValidationError::UnknownSchema {
        id: schema_id.to_string(),
    })?;
    let object = value
        .as_object()
        .ok_or_else(|| SchemaValidationError::NotAnObject {
            id: spec.id.to_string(),
        })?;

    let version = object
        .get("schema_version")
        .and_then(Value::as_u64)
        .map(|v| v as u16)
        .unwrap_or(spec.version);
    require_version(spec, version)?;
    reject_leaked_nested_fields(spec, object)?;
    reject_unknown_critical_fields(spec, object)?;
    require_nest_objects(spec, object)?;
    Ok(spec)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn known_schemas_include_approval_capabilities_session_extension_and_mcp() {
        assert_eq!(SCHEMA_APPROVAL_DETAIL.id, "impetus.approval_detail.v1");
        assert_eq!(SCHEMA_APPROVAL_DETAIL.version, 1);
        assert_eq!(SCHEMA_CAPABILITIES.id, "impetus.capabilities.v1");
        assert_eq!(SCHEMA_CAPABILITIES.version, 1);
        assert_eq!(SCHEMA_SESSION.id, "impetus.session.v1");
        assert_eq!(SCHEMA_SESSION.version, 1);
        assert_eq!(SCHEMA_EXTENSION.id, "impetus.extension.v1");
        assert_eq!(SCHEMA_EXTENSION.version, 1);
        assert_eq!(SCHEMA_MCP.id, "impetus.mcp.v1");
        assert_eq!(SCHEMA_MCP.version, 1);
        assert!(lookup(SCHEMA_APPROVAL_DETAIL.id).is_some());
        assert!(lookup(SCHEMA_CAPABILITIES.id).is_some());
        assert!(lookup(SCHEMA_SESSION.id).is_some());
        assert!(lookup(SCHEMA_EXTENSION.id).is_some());
        assert!(lookup(SCHEMA_MCP.id).is_some());
        assert!(SCHEMA_CAPABILITIES.critical_fields.contains(&NEST_PROVIDER));
        assert!(SCHEMA_CAPABILITIES.critical_fields.contains(&NEST_HARNESS));
        assert!(SCHEMA_MCP.critical_fields.contains(&"env_keys"));
        assert!(!SCHEMA_MCP.critical_fields.contains(&"env"));
    }

    #[test]
    fn version_mismatch_fails_clearly() {
        let err = require_version(&SCHEMA_CAPABILITIES, 99).unwrap_err();
        assert_eq!(
            err,
            SchemaValidationError::VersionMismatch {
                id: "impetus.capabilities.v1".into(),
                expected: 1,
                actual: 99,
            }
        );
        let msg = err.to_string();
        assert!(msg.contains("version mismatch"));
        assert!(msg.contains("expected 1"));
        assert!(msg.contains("got 99"));
    }

    #[test]
    fn leaked_provider_field_rejected_at_top_level() {
        let value = json!({
            "schema_version": 1,
            "capabilities": [],
            "api_token": "label-only-never-a-secret"
        });
        let err = validate_envelope(SCHEMA_CAPABILITIES.id, &value).unwrap_err();
        let msg = err.to_string();
        match err {
            SchemaValidationError::LeakedNestedField {
                id,
                field,
                nest_under,
            } => {
                assert_eq!(id, "impetus.capabilities.v1");
                assert_eq!(field, "api_token");
                assert_eq!(nest_under, NEST_PROVIDER);
            }
            other => panic!("expected LeakedNestedField, got {other:?}"),
        }
        assert!(msg.contains("must nest under"));
        assert!(msg.contains(NEST_PROVIDER));
        // Secrets must not be required for the test — only the field name is asserted.
        assert!(!value.to_string().contains("sk-"));
    }

    #[test]
    fn leaked_harness_field_rejected_at_top_level() {
        let value = json!({
            "schema_version": 1,
            "session_id": "00000000-0000-0000-0000-000000000001",
            "worktree_path": "/tmp/should-nest"
        });
        let err = validate_envelope(SCHEMA_SESSION.id, &value).unwrap_err();
        assert_eq!(
            err,
            SchemaValidationError::LeakedNestedField {
                id: "impetus.session.v1".into(),
                field: "worktree_path".into(),
                nest_under: NEST_HARNESS.into(),
            }
        );
    }

    #[test]
    fn nested_provider_and_harness_accepted() {
        let value = json!({
            "schema_version": 1,
            "capabilities": [],
            "provider": {
                "provider_id": "openai",
                "model": "gpt-test",
                "api_key_ref": "keychain:openai-label"
            },
            "harness": {
                "harness_id": "impetusd",
                "seatbelt_process_wrap": true
            }
        });
        let spec = validate_envelope(SCHEMA_CAPABILITIES.id, &value).expect("nested ok");
        assert_eq!(spec.id, SCHEMA_CAPABILITIES.id);
        assert!(!value.to_string().contains("sk-"));
        assert!(!value.to_string().contains("Bearer "));
    }

    #[test]
    fn session_ish_accepts_nested_provider() {
        let value = json!({
            "schema_version": 1,
            "session_id": "00000000-0000-0000-0000-000000000002",
            "provider": { "model_id": "mock-1", "provider_id": "mock" },
            "harness": { "worktree_path": "/tmp/session-wt" }
        });
        let spec = validate_envelope(SCHEMA_SESSION.id, &value).expect("session nested ok");
        assert_eq!(spec.id, SCHEMA_SESSION.id);
    }

    #[test]
    fn provider_nest_must_be_object() {
        let value = json!({
            "schema_version": 1,
            "capabilities": [],
            "provider": "openai"
        });
        let err = validate_envelope(SCHEMA_CAPABILITIES.id, &value).unwrap_err();
        assert_eq!(
            err,
            SchemaValidationError::NestNotAnObject {
                id: "impetus.capabilities.v1".into(),
                nest: NEST_PROVIDER.into(),
            }
        );
    }

    #[test]
    fn unknown_critical_field_still_rejected() {
        let value = json!({
            "schema_version": 1,
            "capabilities": [],
            "totally_unknown": true
        });
        let err = validate_envelope(SCHEMA_CAPABILITIES.id, &value).unwrap_err();
        match err {
            SchemaValidationError::UnknownCriticalField { id, field } => {
                assert_eq!(id, "impetus.capabilities.v1");
                assert_eq!(field, "totally_unknown");
            }
            other => panic!("expected UnknownCriticalField, got {other:?}"),
        }
    }

    #[test]
    fn capabilities_envelope_accepts_gather_shape() {
        let report = crate::CapabilityTruthReport::gather(&[]);
        let value = serde_json::to_value(&report).expect("serialize");
        let spec = validate_envelope(SCHEMA_CAPABILITIES.id, &value).expect("valid");
        assert_eq!(spec.id, SCHEMA_CAPABILITIES.id);
        assert!(!value.to_string().contains("sk-"));
        assert!(!value.to_string().contains("Bearer "));
    }

    #[test]
    fn unknown_schema_id_rejected() {
        let err = validate_envelope("impetus.nope.v1", &json!({"schema_version": 1})).unwrap_err();
        assert_eq!(
            err,
            SchemaValidationError::UnknownSchema {
                id: "impetus.nope.v1".into(),
            }
        );
    }
}
