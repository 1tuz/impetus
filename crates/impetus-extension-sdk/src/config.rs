//! Extension configuration schema declaration.

use serde::{Deserialize, Serialize};

/// Where an extension config value applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfigScope {
    Global,
    Workspace,
}

/// Declared configuration contract for an extension package.
///
/// Host owns storage under daemon/user data dirs. Extensions must not invent
/// arbitrary secret/config locations outside this schema.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExtensionConfigSchema {
    /// JSON Schema document describing configuration fields.
    pub schema: serde_json::Value,
    /// Default values applied when keys are omitted.
    pub defaults: serde_json::Value,
    /// Scope for host-managed config persistence.
    pub scope: ConfigScope,
}

impl ExtensionConfigSchema {
    /// Reject empty/non-object schema documents.
    pub fn validate(&self) -> Result<(), ConfigSchemaError> {
        if !self.schema.is_object() {
            return Err(ConfigSchemaError::SchemaMustBeObject);
        }
        if !self.defaults.is_object() {
            return Err(ConfigSchemaError::DefaultsMustBeObject);
        }
        Ok(())
    }
}

/// Configuration schema validation failure.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ConfigSchemaError {
    #[error("configuration.schema must be a JSON object")]
    SchemaMustBeObject,
    #[error("configuration.defaults must be a JSON object")]
    DefaultsMustBeObject,
}
