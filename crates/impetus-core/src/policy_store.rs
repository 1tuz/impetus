//! Governed instruction references — operator-curated policy surface.
//!
//! Trust split (see also [`crate::memory_store`]):
//!
//! ```text
//! Runtime State (EventStore) ≠ Memory (MemoryStore) ≠ Policy (PolicyEngine / PolicyConfig)
//! ≠ PolicyStore (governed instruction refs)
//! ```
//!
//! [`PolicyStore`] holds stable ids and human labels for workspace instructions
//! that operators mark as governed. It is **not** [`crate::PolicyConfig`] action
//! overrides, not durable runtime state, and not untrusted memory. No secrets —
//! only reference labels. [`crate::InstructionResolver`] output can be intersected
//! via [`PolicyStore::governed_ids_in`].

use crate::instructions::{InstructionKind, ResolvedInstructions};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use thiserror::Error;

/// Supported policy-store schema version.
pub const POLICY_STORE_VERSION: u32 = 1;

/// One governed instruction reference (label + optional workspace instruction id).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GovernedInstructionRef {
    pub id: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instruction_kind: Option<InstructionKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instruction_id: Option<String>,
}

/// Operator-curated governed-instruction catalog.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct PolicyStore {
    pub version: u32,
    #[serde(default)]
    pub instructions: Vec<GovernedInstructionRef>,
}

#[derive(Debug, Error)]
pub enum PolicyStoreError {
    #[error("failed to read policy store: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid policy store JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("unsupported policy store version: {0} (expected {POLICY_STORE_VERSION})")]
    UnsupportedVersion(u32),
    #[error("duplicate governed instruction id: {0}")]
    DuplicateId(String),
    #[error("governed instruction id must not be empty")]
    EmptyId,
    #[error("governed instruction label must not be empty")]
    EmptyLabel,
}

impl PolicyStore {
    pub fn parse(json: &str) -> Result<Self, PolicyStoreError> {
        let store: Self = serde_json::from_str(json)?;
        store.validate()?;
        Ok(store)
    }

    pub fn load_from_path(path: impl AsRef<Path>) -> Result<Self, PolicyStoreError> {
        let raw = fs::read_to_string(path)?;
        Self::parse(&raw)
    }

    /// Missing file → `None`; present but invalid → error (fail closed).
    pub fn load_optional(path: impl AsRef<Path>) -> Result<Option<Self>, PolicyStoreError> {
        let path = path.as_ref();
        if !path.exists() {
            return Ok(None);
        }
        Self::load_from_path(path).map(Some)
    }

    pub fn export_json(&self) -> Result<String, PolicyStoreError> {
        self.validate()?;
        serde_json::to_string_pretty(self).map_err(PolicyStoreError::from)
    }

    pub fn instructions(&self) -> &[GovernedInstructionRef] {
        &self.instructions
    }

    /// Governed catalog ids (stable operator labels).
    pub fn governed_ids(&self) -> Vec<&str> {
        self.instructions
            .iter()
            .map(|entry| entry.id.as_str())
            .collect()
    }

    /// Workspace instruction ids from `resolved` that appear in this store.
    pub fn governed_ids_in(&self, resolved: &ResolvedInstructions) -> Vec<String> {
        let governed_instruction_ids: Vec<&str> = self
            .instructions
            .iter()
            .filter_map(|entry| entry.instruction_id.as_deref())
            .collect();
        crate::instructions::governed_instruction_ids(resolved, &governed_instruction_ids)
    }

    fn validate(&self) -> Result<(), PolicyStoreError> {
        if self.version != POLICY_STORE_VERSION {
            return Err(PolicyStoreError::UnsupportedVersion(self.version));
        }
        let mut seen = BTreeSet::new();
        for entry in &self.instructions {
            if entry.id.trim().is_empty() {
                return Err(PolicyStoreError::EmptyId);
            }
            if entry.label.trim().is_empty() {
                return Err(PolicyStoreError::EmptyLabel);
            }
            if !seen.insert(entry.id.clone()) {
                return Err(PolicyStoreError::DuplicateId(entry.id.clone()));
            }
            for text in [&entry.id, &entry.label] {
                if text.contains("sk-") || text.to_ascii_lowercase().contains("password") {
                    return Err(PolicyStoreError::EmptyLabel);
                }
            }
        }
        Ok(())
    }
}

/// Conventional path under a daemon data root.
pub fn default_policy_store_path(data_root: &Path) -> PathBuf {
    data_root.join("policy_store.json")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::instructions::{
        InstructionKind, InstructionReference, InstructionScope, InstructionTokenEstimate,
    };
    use std::path::PathBuf;

    fn sample_resolved(ids: &[&str]) -> ResolvedInstructions {
        ResolvedInstructions {
            references: ids
                .iter()
                .map(|id| InstructionReference {
                    id: (*id).into(),
                    kind: InstructionKind::ProjectRules,
                    scope: InstructionScope::Workspace,
                    relative_path: PathBuf::from(format!("rules/{id}.md")),
                    content_hash: "hash".into(),
                    text: format!("rules for {id}"),
                })
                .collect(),
            estimated_tokens: InstructionTokenEstimate::default(),
        }
    }

    #[test]
    fn parse_and_export_roundtrip() {
        let store = PolicyStore::parse(
            r#"{
              "version": 1,
              "instructions": [
                {
                  "id": "gov-security",
                  "label": "Security review gate",
                  "instruction_kind": "ProjectRules",
                  "instruction_id": "security-rules"
                }
              ]
            }"#,
        )
        .expect("parse");
        let exported = store.export_json().expect("export");
        let again = PolicyStore::parse(&exported).expect("re-parse");
        assert_eq!(store, again);
    }

    #[test]
    fn governed_ids_in_intersects_resolver_output() {
        let store = PolicyStore::parse(
            r#"{
              "version": 1,
              "instructions": [
                {"id": "gov-1", "label": "Rules", "instruction_id": "security-rules"},
                {"id": "gov-2", "label": "Style", "instruction_id": "missing-id"}
              ]
            }"#,
        )
        .expect("parse");
        let resolved = sample_resolved(&["security-rules", "other-rules"]);
        assert_eq!(
            store.governed_ids_in(&resolved),
            vec!["security-rules".to_string()]
        );
    }

    #[test]
    fn load_optional_missing_is_none() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("policy_store.json");
        assert_eq!(PolicyStore::load_optional(&path).expect("optional"), None);
    }

    #[test]
    fn load_optional_rejects_invalid_present_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("policy_store.json");
        std::fs::write(&path, r#"{"version":99}"#).expect("write");
        let err = PolicyStore::load_optional(&path).expect_err("bad version");
        assert!(matches!(err, PolicyStoreError::UnsupportedVersion(99)));
    }

    #[test]
    fn reject_duplicate_governed_ids() {
        let err = PolicyStore::parse(
            r#"{
              "version": 1,
              "instructions": [
                {"id": "dup", "label": "one"},
                {"id": "dup", "label": "two"}
              ]
            }"#,
        )
        .expect_err("duplicate");
        assert!(matches!(err, PolicyStoreError::DuplicateId(id) if id == "dup"));
    }
}
