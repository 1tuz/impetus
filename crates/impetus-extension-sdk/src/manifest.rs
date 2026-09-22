//! Extension package manifest (`impetus.extension_package.v1`).
//!
//! Preferred on-disk form: `extension.toml` (or `extension.json`). Distinct from
//! legacy install digest contract `impetus.extension.v1` in core.

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::capability::ExtensionCapabilityKind;
use crate::compat::{CompatError, check_compatibility};
use crate::config::{ConfigSchemaError, ExtensionConfigSchema};
use crate::entrypoint::{EntrypointError, ExtensionEntrypoint};
use crate::id::{ExtensionId, ExtensionIdError};
use crate::permissions::{ExtensionPermission, PermissionsError, validate_permissions};
use crate::version::{CURRENT_SUPPORTED_RANGE, ExtensionApiVersion, SupportedApiRange};

/// Schema id for the SDK package manifest envelope.
pub const EXTENSION_PACKAGE_SCHEMA_ID: &str = "impetus.extension_package.v1";

/// Current package manifest schema version.
pub const EXTENSION_PACKAGE_SCHEMA_VERSION: u16 = 1;

fn default_schema_version() -> u16 {
    EXTENSION_PACKAGE_SCHEMA_VERSION
}

/// Validated extension package manifest for external authors.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExtensionPackageManifest {
    /// Version of this package contract. Omitted defaults to v1.
    #[serde(default = "default_schema_version")]
    pub schema_version: u16,
    pub id: ExtensionId,
    pub name: String,
    /// Semver package version string (non-empty, must parse as semver).
    pub version: String,
    pub description: String,
    pub author: String,
    /// Extension API major required by this package (not Impetus app version).
    pub extension_api_version: u32,
    pub entrypoint: ExtensionEntrypoint,
    pub capabilities: Vec<ExtensionCapabilityKind>,
    #[serde(default)]
    pub permissions: Vec<ExtensionPermission>,
    #[serde(default)]
    pub configuration: Option<ExtensionConfigSchema>,
    #[serde(default)]
    pub dependencies: Vec<ExtensionId>,
}

/// Package manifest validation / parse errors.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ExtensionPackageManifestError {
    #[error(transparent)]
    Id(#[from] ExtensionIdError),
    #[error("extension name must be non-empty")]
    EmptyName,
    #[error("extension version must be non-empty")]
    EmptyVersion,
    #[error("extension version `{version}` is not valid semver: {detail}")]
    InvalidSemver { version: String, detail: String },
    #[error(transparent)]
    Permissions(#[from] PermissionsError),
    #[error(transparent)]
    Entrypoint(#[from] EntrypointError),
    #[error("unsupported schema_version {got}; expected {expected}")]
    UnsupportedSchemaVersion { got: u16, expected: u16 },
    #[error("TOML parse error: {0}")]
    TomlParse(String),
    #[error("TOML serialize error: {0}")]
    TomlSerialize(String),
    #[error("JSON parse error: {0}")]
    JsonParse(String),
    #[error("capabilities must contain at least one entry")]
    EmptyCapabilities,
    #[error("duplicate capability `{0}`")]
    DuplicateCapability(String),
    #[error(transparent)]
    Compat(#[from] CompatError),
    #[error(transparent)]
    Config(#[from] ConfigSchemaError),
}

impl ExtensionPackageManifest {
    /// Validate fields against the host's current supported API range.
    pub fn validate(&self) -> Result<(), ExtensionPackageManifestError> {
        self.validate_against(CURRENT_SUPPORTED_RANGE)
    }

    /// Validate including compatibility against a host-supported API range.
    pub fn validate_against(
        &self,
        supported: SupportedApiRange,
    ) -> Result<(), ExtensionPackageManifestError> {
        if self.schema_version != EXTENSION_PACKAGE_SCHEMA_VERSION {
            return Err(ExtensionPackageManifestError::UnsupportedSchemaVersion {
                got: self.schema_version,
                expected: EXTENSION_PACKAGE_SCHEMA_VERSION,
            });
        }
        // `id` already validated when constructed; re-check for hand-built values.
        if !crate::id::is_valid_extension_id(self.id.as_str()) {
            return Err(ExtensionIdError::InvalidFormat {
                id: self.id.as_str().to_string(),
            }
            .into());
        }
        if self.name.trim().is_empty() {
            return Err(ExtensionPackageManifestError::EmptyName);
        }
        if self.version.trim().is_empty() {
            return Err(ExtensionPackageManifestError::EmptyVersion);
        }
        if let Err(err) = semver::Version::parse(self.version.trim()) {
            return Err(ExtensionPackageManifestError::InvalidSemver {
                version: self.version.clone(),
                detail: err.to_string(),
            });
        }
        self.entrypoint.validate()?;
        if self.capabilities.is_empty() {
            return Err(ExtensionPackageManifestError::EmptyCapabilities);
        }
        let mut seen_caps = std::collections::HashSet::new();
        for cap in &self.capabilities {
            if !seen_caps.insert(*cap) {
                return Err(ExtensionPackageManifestError::DuplicateCapability(
                    cap.as_str().to_string(),
                ));
            }
        }
        validate_permissions(&self.permissions)?;
        if let Some(cfg) = &self.configuration {
            cfg.validate()?;
        }
        for dep in &self.dependencies {
            if !crate::id::is_valid_extension_id(dep.as_str()) {
                return Err(ExtensionIdError::InvalidFormat {
                    id: dep.as_str().to_string(),
                }
                .into());
            }
        }
        check_compatibility(ExtensionApiVersion(self.extension_api_version), supported)?;
        Ok(())
    }

    /// Parse and validate from TOML (`extension.toml`).
    pub fn from_toml_str(s: &str) -> Result<Self, ExtensionPackageManifestError> {
        let manifest: Self = toml::from_str(s)
            .map_err(|e| ExtensionPackageManifestError::TomlParse(e.to_string()))?;
        manifest.validate()?;
        Ok(manifest)
    }

    /// Serialize to TOML after validation.
    pub fn to_toml_string(&self) -> Result<String, ExtensionPackageManifestError> {
        self.validate()?;
        toml::to_string_pretty(self)
            .map_err(|e| ExtensionPackageManifestError::TomlSerialize(e.to_string()))
    }

    /// Parse and validate from JSON (`extension.json`).
    pub fn from_json_str(s: &str) -> Result<Self, ExtensionPackageManifestError> {
        let manifest: Self = serde_json::from_str(s)
            .map_err(|e| ExtensionPackageManifestError::JsonParse(e.to_string()))?;
        manifest.validate()?;
        Ok(manifest)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ConfigScope;
    use crate::version::{CURRENT_SUPPORTED_RANGE, ExtensionApiVersion};
    use crate::{CompatError, check_compatibility, validate_permissions};

    fn sample_toml() -> &'static str {
        r#"
schema_version = 1
id = "demo-pack"
name = "Demo Pack"
version = "1.2.3"
description = "example"
author = "authors"
extension_api_version = 1
capabilities = ["skill_provider", "tool"]
permissions = ["filesystem_read", "network"]
dependencies = ["base-pack"]

[entrypoint]
kind = "instruction_pack"
root = "skills"
"#
    }

    #[test]
    fn valid_manifest_toml_roundtrip() {
        let parsed = ExtensionPackageManifest::from_toml_str(sample_toml()).unwrap();
        assert_eq!(parsed.id.as_str(), "demo-pack");
        assert_eq!(parsed.version, "1.2.3");
        assert_eq!(
            parsed.entrypoint,
            ExtensionEntrypoint::InstructionPack {
                root: "skills".into()
            }
        );
        let toml_out = parsed.to_toml_string().unwrap();
        let again = ExtensionPackageManifest::from_toml_str(&toml_out).unwrap();
        assert_eq!(parsed, again);
    }

    #[test]
    fn valid_manifest_json_roundtrip() {
        let parsed = ExtensionPackageManifest::from_toml_str(sample_toml()).unwrap();
        let json = serde_json::to_string_pretty(&parsed).unwrap();
        let from_json = ExtensionPackageManifest::from_json_str(&json).unwrap();
        assert_eq!(parsed, from_json);
    }

    #[test]
    fn json_with_configuration() {
        let manifest = ExtensionPackageManifest {
            schema_version: 1,
            id: ExtensionId::new("cfg-ext").unwrap(),
            name: "Cfg".into(),
            version: "0.1.0".into(),
            description: String::new(),
            author: "a".into(),
            extension_api_version: 1,
            entrypoint: ExtensionEntrypoint::HostProcess {
                command: "run-ext".into(),
                args: vec!["--stdio".into()],
            },
            capabilities: vec![ExtensionCapabilityKind::Tool],
            permissions: vec![ExtensionPermission::ProcessSpawn],
            configuration: Some(ExtensionConfigSchema {
                schema: serde_json::json!({"type": "object"}),
                defaults: serde_json::json!({}),
                scope: ConfigScope::Workspace,
            }),
            dependencies: vec![],
        };
        manifest.validate().unwrap();
        let json = serde_json::to_string(&manifest).unwrap();
        let back = ExtensionPackageManifest::from_json_str(&json).unwrap();
        assert_eq!(manifest, back);
    }

    #[test]
    fn reject_invalid_id() {
        let bad = sample_toml().replace("demo-pack", "Bad Id");
        let err = ExtensionPackageManifest::from_toml_str(&bad).unwrap_err();
        assert!(matches!(
            err,
            ExtensionPackageManifestError::TomlParse(_) | ExtensionPackageManifestError::Id(_)
        ));
    }

    #[test]
    fn reject_empty_version() {
        let bad = sample_toml().replace("1.2.3", "");
        let err = ExtensionPackageManifest::from_toml_str(&bad).unwrap_err();
        assert!(matches!(
            err,
            ExtensionPackageManifestError::EmptyVersion
                | ExtensionPackageManifestError::InvalidSemver { .. }
        ));
    }

    #[test]
    fn reject_duplicate_permissions() {
        let bad = sample_toml().replace(
            r#"permissions = ["filesystem_read", "network"]"#,
            r#"permissions = ["network", "network"]"#,
        );
        let err = ExtensionPackageManifest::from_toml_str(&bad).unwrap_err();
        assert!(matches!(
            err,
            ExtensionPackageManifestError::Permissions(PermissionsError::Duplicate { .. })
        ));
    }

    #[test]
    fn reject_unknown_capability_via_serde() {
        let bad = sample_toml().replace(
            r#"capabilities = ["skill_provider", "tool"]"#,
            r#"capabilities = ["not_a_capability"]"#,
        );
        let err = ExtensionPackageManifest::from_toml_str(&bad).unwrap_err();
        assert!(matches!(err, ExtensionPackageManifestError::TomlParse(_)));
    }

    #[test]
    fn permissions_uniqueness_helper() {
        assert!(
            validate_permissions(&[ExtensionPermission::Mcp, ExtensionPermission::Git]).is_ok()
        );
        assert!(
            validate_permissions(&[ExtensionPermission::Mcp, ExtensionPermission::Mcp]).is_err()
        );
    }

    #[test]
    fn compat_in_range_and_out() {
        assert!(check_compatibility(ExtensionApiVersion(1), CURRENT_SUPPORTED_RANGE).is_ok());
        let below =
            check_compatibility(ExtensionApiVersion(0), CURRENT_SUPPORTED_RANGE).unwrap_err();
        assert!(below.to_string().contains("below"));
        let above =
            check_compatibility(ExtensionApiVersion(99), CURRENT_SUPPORTED_RANGE).unwrap_err();
        assert!(above.to_string().contains("above"));
    }

    #[test]
    fn reject_outdated_extension_api_version_on_validate() {
        let mut parsed = ExtensionPackageManifest::from_toml_str(sample_toml()).unwrap();
        parsed.extension_api_version = 99;
        let err = parsed.validate().unwrap_err();
        assert!(matches!(
            err,
            ExtensionPackageManifestError::Compat(CompatError::AboveMax { .. })
        ));
    }

    #[test]
    fn schema_id_constant() {
        assert_eq!(EXTENSION_PACKAGE_SCHEMA_ID, "impetus.extension_package.v1");
    }
}
