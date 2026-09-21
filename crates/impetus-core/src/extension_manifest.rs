//! Minimal extension contract (`impetus.extension.v1`).
//!
//! Vertical slice: validated manifest for Skill / MCP install paths.
//! Not a marketplace registry.

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::schema::{SCHEMA_EXTENSION, SchemaValidationError, require_version, validate_envelope};

/// Documented schema id for the extension manifest contract.
pub const EXTENSION_SCHEMA_ID: &str = SCHEMA_EXTENSION.id;

/// Current extension manifest schema version.
pub const EXTENSION_SCHEMA_VERSION: u16 = SCHEMA_EXTENSION.version;

fn default_extension_schema_version() -> u16 {
    EXTENSION_SCHEMA_VERSION
}

/// Kind of local extension source covered by this contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExtensionManifestKind {
    /// Agent Skills `SKILL.md` (file or skill directory).
    Skill,
    /// MCP server config JSON (parse only; no server spawn).
    McpConfig,
}

/// Minimal validated extension manifest.
///
/// Critical envelope fields match [`SCHEMA_EXTENSION`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtensionManifest {
    /// Version of this contract. Omitted JSON defaults to v1.
    #[serde(default = "default_extension_schema_version")]
    pub schema_version: u16,
    /// Stable module id (sanitized).
    pub id: String,
    pub kind: ExtensionManifestKind,
    /// Semver-ish or source-declared version string.
    pub version: String,
    /// Content digest of the source artifact (`sha256:` + 64 hex).
    pub digest: String,
    /// Declared capability tokens (non-empty, lowercase tokens).
    pub capabilities: Vec<String>,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ExtensionManifestError {
    #[error("extension id must be non-empty")]
    EmptyId,
    #[error("extension version must be non-empty")]
    EmptyVersion,
    #[error("invalid digest `{digest}`: expected sha256: + 64 lowercase hex")]
    InvalidDigest { digest: String },
    #[error("capabilities must contain at least one token")]
    EmptyCapabilities,
    #[error("invalid capability token `{token}`")]
    InvalidCapability { token: String },
    #[error("duplicate capability token `{token}`")]
    DuplicateCapability { token: String },
    #[error(transparent)]
    Schema(#[from] SchemaValidationError),
}

impl ExtensionManifest {
    /// Build a manifest and validate id / digest / capabilities / schema envelope.
    pub fn new(
        id: impl Into<String>,
        kind: ExtensionManifestKind,
        version: impl Into<String>,
        digest: impl Into<String>,
        capabilities: Vec<String>,
    ) -> Result<Self, ExtensionManifestError> {
        let manifest = Self {
            schema_version: EXTENSION_SCHEMA_VERSION,
            id: id.into(),
            kind,
            version: version.into(),
            digest: digest.into(),
            capabilities,
        };
        manifest.validate()?;
        Ok(manifest)
    }

    /// Validate field rules and the registered critical envelope.
    pub fn validate(&self) -> Result<(), ExtensionManifestError> {
        if self.id.trim().is_empty() {
            return Err(ExtensionManifestError::EmptyId);
        }
        if self.version.trim().is_empty() {
            return Err(ExtensionManifestError::EmptyVersion);
        }
        validate_digest(&self.digest)?;
        validate_capabilities(&self.capabilities)?;
        require_version(&SCHEMA_EXTENSION, self.schema_version)?;
        let value = serde_json::to_value(self).expect("ExtensionManifest serializes");
        validate_envelope(EXTENSION_SCHEMA_ID, &value)?;
        Ok(())
    }
}

/// Accept only `sha256:` + exactly 64 lowercase hex digits (matches [`crate::ownership::content_digest`]).
pub fn validate_digest(digest: &str) -> Result<(), ExtensionManifestError> {
    let Some(hex) = digest.strip_prefix("sha256:") else {
        return Err(ExtensionManifestError::InvalidDigest {
            digest: digest.to_string(),
        });
    };
    if hex.len() != 64 || !hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
        return Err(ExtensionManifestError::InvalidDigest {
            digest: digest.to_string(),
        });
    }
    Ok(())
}

/// Capability tokens: non-empty, unique, `^[a-z][a-z0-9_:-]{0,63}$`.
pub fn validate_capabilities(capabilities: &[String]) -> Result<(), ExtensionManifestError> {
    if capabilities.is_empty() {
        return Err(ExtensionManifestError::EmptyCapabilities);
    }
    let mut seen = std::collections::BTreeSet::new();
    for token in capabilities {
        if !is_valid_capability_token(token) {
            return Err(ExtensionManifestError::InvalidCapability {
                token: token.clone(),
            });
        }
        if !seen.insert(token.as_str()) {
            return Err(ExtensionManifestError::DuplicateCapability {
                token: token.clone(),
            });
        }
    }
    Ok(())
}

fn is_valid_capability_token(token: &str) -> bool {
    let mut chars = token.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !first.is_ascii_lowercase() {
        return false;
    }
    let rest_ok = chars
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-' || c == ':');
    rest_ok && token.len() <= 64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ownership::content_digest;
    use serde_json::json;

    fn valid_digest() -> String {
        content_digest(b"extension-contract-fixture")
    }

    #[test]
    fn valid_manifest_round_trips_and_validates() {
        let digest = valid_digest();
        let manifest = ExtensionManifest::new(
            "demo-skill",
            ExtensionManifestKind::Skill,
            "0.1.0",
            digest.clone(),
            vec!["instructions".into(), "triggers".into()],
        )
        .expect("valid");
        assert_eq!(manifest.schema_version, EXTENSION_SCHEMA_VERSION);
        assert_eq!(EXTENSION_SCHEMA_ID, "impetus.extension.v1");

        let value = serde_json::to_value(&manifest).expect("serialize");
        let back: ExtensionManifest = serde_json::from_value(value.clone()).expect("deserialize");
        assert_eq!(back, manifest);
        back.validate().expect("round-trip still valid");

        let blob = value.to_string();
        assert!(!blob.contains("sk-"));
        assert!(!blob.contains("Bearer "));
        assert!(!blob.contains("api_key"));
    }

    #[test]
    fn digest_must_be_sha256_lowercase_hex() {
        assert!(validate_digest(&valid_digest()).is_ok());
        assert!(matches!(
            validate_digest("md5:deadbeef"),
            Err(ExtensionManifestError::InvalidDigest { .. })
        ));
        assert!(matches!(
            validate_digest(&format!("sha256:{}", "A".repeat(64))),
            Err(ExtensionManifestError::InvalidDigest { .. })
        ));
        assert!(matches!(
            validate_digest(&format!("sha256:{}", "ab".repeat(16))),
            Err(ExtensionManifestError::InvalidDigest { .. })
        ));
    }

    #[test]
    fn capabilities_reject_empty_invalid_and_duplicate() {
        assert!(matches!(
            validate_capabilities(&[]),
            Err(ExtensionManifestError::EmptyCapabilities)
        ));
        assert!(matches!(
            validate_capabilities(&["".into()]),
            Err(ExtensionManifestError::InvalidCapability { .. })
        ));
        assert!(matches!(
            validate_capabilities(&["Tools".into()]),
            Err(ExtensionManifestError::InvalidCapability { .. })
        ));
        assert!(matches!(
            validate_capabilities(&["tools".into(), "tools".into()]),
            Err(ExtensionManifestError::DuplicateCapability { .. })
        ));
        assert!(validate_capabilities(&["tools".into(), "stdio".into()]).is_ok());
    }

    #[test]
    fn unknown_critical_field_rejected_via_envelope() {
        let value = json!({
            "schema_version": 1,
            "id": "x",
            "kind": "skill",
            "version": "1.0.0",
            "digest": valid_digest(),
            "capabilities": ["instructions"],
            "api_token": "should-never-appear"
        });
        let err = validate_envelope(EXTENSION_SCHEMA_ID, &value).unwrap_err();
        match err {
            SchemaValidationError::UnknownCriticalField { id, field } => {
                assert_eq!(id, "impetus.extension.v1");
                assert_eq!(field, "api_token");
            }
            other => panic!("expected UnknownCriticalField, got {other:?}"),
        }
    }

    #[test]
    fn empty_id_or_version_rejected() {
        let digest = valid_digest();
        assert!(matches!(
            ExtensionManifest::new(
                "",
                ExtensionManifestKind::Skill,
                "1.0.0",
                digest.clone(),
                vec!["instructions".into()]
            ),
            Err(ExtensionManifestError::EmptyId)
        ));
        assert!(matches!(
            ExtensionManifest::new(
                "x",
                ExtensionManifestKind::McpConfig,
                "  ",
                digest,
                vec!["mcp".into()]
            ),
            Err(ExtensionManifestError::EmptyVersion)
        ));
    }
}
