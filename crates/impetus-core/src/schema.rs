//! Shared registry for versioned canonical Impetus payload schemas.
//!
//! Convention:
//! - Stable string id: `impetus.<name>.vN` (see [`SchemaSpec::id`])
//! - Numeric field on payloads: `schema_version` (u16)
//! - Evolution: bump version; reject mismatch and unknown critical fields

use serde_json::Value;
use thiserror::Error;

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
    ],
};

/// Capability truth snapshot (`doctor` / Diagnostics matrix).
pub const SCHEMA_CAPABILITIES: SchemaSpec = SchemaSpec {
    id: "impetus.capabilities.v1",
    version: 1,
    critical_fields: &["schema_version", "capabilities"],
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

/// All schemas known to this crate build. Order is stable for tests/docs.
pub const KNOWN_SCHEMAS: &[SchemaSpec] = &[
    SCHEMA_APPROVAL_DETAIL,
    SCHEMA_CAPABILITIES,
    SCHEMA_EXTENSION,
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

/// Validate a JSON value as a known critical schema envelope.
///
/// Checks: known id, version match, no unknown top-level fields.
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
    reject_unknown_critical_fields(spec, object)?;
    Ok(spec)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn known_schemas_include_approval_capabilities_and_extension() {
        assert_eq!(SCHEMA_APPROVAL_DETAIL.id, "impetus.approval_detail.v1");
        assert_eq!(SCHEMA_APPROVAL_DETAIL.version, 1);
        assert_eq!(SCHEMA_CAPABILITIES.id, "impetus.capabilities.v1");
        assert_eq!(SCHEMA_CAPABILITIES.version, 1);
        assert_eq!(SCHEMA_EXTENSION.id, "impetus.extension.v1");
        assert_eq!(SCHEMA_EXTENSION.version, 1);
        assert!(lookup(SCHEMA_APPROVAL_DETAIL.id).is_some());
        assert!(lookup(SCHEMA_CAPABILITIES.id).is_some());
        assert!(lookup(SCHEMA_EXTENSION.id).is_some());
        assert!(lookup("impetus.session.v1").is_none());
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
    fn unknown_critical_field_rejected() {
        let value = json!({
            "schema_version": 1,
            "capabilities": [],
            "api_token": "should-never-appear"
        });
        let err = validate_envelope(SCHEMA_CAPABILITIES.id, &value).unwrap_err();
        match err {
            SchemaValidationError::UnknownCriticalField { id, field } => {
                assert_eq!(id, "impetus.capabilities.v1");
                assert_eq!(field, "api_token");
            }
            other => panic!("expected UnknownCriticalField, got {other:?}"),
        }
        // Secrets must not be required for the test — only the field name is asserted.
        assert!(!value.to_string().contains("sk-"));
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
