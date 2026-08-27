use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use thiserror::Error;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityAvailability {
    Available,
    Planned,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CapabilityManifest {
    pub id: String,
    pub version: String,
    pub description: String,
    pub availability: CapabilityAvailability,
    #[serde(default)]
    pub roadmap_phase: Option<String>,
    #[serde(default)]
    pub permissions: Vec<String>,
    #[serde(default)]
    pub entrypoint: Option<String>,
}

#[derive(Debug, Error)]
pub enum PluginError {
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("capability id is empty")]
    EmptyId,
    #[error("capability id `{0}` has an invalid format")]
    InvalidId(String),
    #[error("capability `{id}` has invalid semantic version `{version}`")]
    InvalidVersion { id: String, version: String },
    #[error("capability `{id}` requests unknown permission `{permission}`")]
    UnknownPermission { id: String, permission: String },
    #[error("planned capability `{0}` must name its roadmap phase")]
    MissingRoadmapPhase(String),
    #[error("capability `{0}` has an empty entrypoint")]
    EmptyEntrypoint(String),
    #[error("capability `{0}` is already registered")]
    Duplicate(String),
}

#[derive(Debug, Default)]
pub struct CapabilityRegistry {
    manifests: BTreeMap<String, CapabilityManifest>,
}

impl CapabilityRegistry {
    pub fn from_json(source: &str) -> Result<Self, PluginError> {
        let manifests: Vec<CapabilityManifest> = serde_json::from_str(source)?;
        let mut registry = Self::default();
        for manifest in manifests {
            registry.register(manifest)?;
        }
        Ok(registry)
    }

    pub fn register(&mut self, manifest: CapabilityManifest) -> Result<(), PluginError> {
        if manifest.id.trim().is_empty() {
            return Err(PluginError::EmptyId);
        }
        if !manifest.id.chars().all(|character| {
            character.is_ascii_lowercase()
                || character.is_ascii_digit()
                || matches!(character, '.' | '_' | '-')
        }) {
            return Err(PluginError::InvalidId(manifest.id));
        }
        if semver::Version::parse(&manifest.version).is_err() {
            return Err(PluginError::InvalidVersion {
                id: manifest.id,
                version: manifest.version,
            });
        }
        if manifest.availability == CapabilityAvailability::Planned
            && manifest.roadmap_phase.as_deref().is_none_or(str::is_empty)
        {
            return Err(PluginError::MissingRoadmapPhase(manifest.id));
        }
        if manifest.entrypoint.as_deref().is_some_and(str::is_empty) {
            return Err(PluginError::EmptyEntrypoint(manifest.id));
        }
        if let Some(permission) = manifest
            .permissions
            .iter()
            .find(|permission| !is_known_permission(permission))
        {
            return Err(PluginError::UnknownPermission {
                id: manifest.id,
                permission: permission.clone(),
            });
        }
        if self.manifests.contains_key(&manifest.id) {
            return Err(PluginError::Duplicate(manifest.id));
        }
        self.manifests.insert(manifest.id.clone(), manifest);
        Ok(())
    }

    pub fn get(&self, id: &str) -> Option<&CapabilityManifest> {
        self.manifests.get(id)
    }
    pub fn all(&self) -> impl Iterator<Item = &CapabilityManifest> {
        self.manifests.values()
    }
}

fn is_known_permission(permission: &str) -> bool {
    matches!(
        permission,
        "agent.session"
            | "filesystem.read"
            | "filesystem.write"
            | "network.sftp"
            | "network.ssh"
            | "process.spawn"
            | "secrets.ssh_key"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_duplicate_capability() {
        let manifest = CapabilityManifest {
            id: "terminal.pty".into(),
            version: "0.1.0".into(),
            description: "PTY".into(),
            availability: CapabilityAvailability::Planned,
            roadmap_phase: Some("v0.6".into()),
            permissions: vec![],
            entrypoint: None,
        };
        let mut registry = CapabilityRegistry::default();
        registry.register(manifest.clone()).unwrap();
        assert!(matches!(
            registry.register(manifest),
            Err(PluginError::Duplicate(_))
        ));
    }

    #[test]
    fn repository_catalog_is_valid_and_explicitly_planned() {
        let registry =
            CapabilityRegistry::from_json(include_str!("../../../config/capabilities.json"))
                .expect("valid capability catalog");
        assert_eq!(registry.all().count(), 5);
        assert!(
            registry
                .all()
                .all(|manifest| manifest.availability == CapabilityAvailability::Planned)
        );
        assert_eq!(
            registry
                .get("terminal.pty")
                .and_then(|manifest| manifest.roadmap_phase.as_deref()),
            Some("v0.6")
        );
    }

    #[test]
    fn rejects_unknown_permission() {
        let manifest = CapabilityManifest {
            id: "unsafe.example".into(),
            version: "0.1.0".into(),
            description: "unsafe".into(),
            availability: CapabilityAvailability::Available,
            roadmap_phase: None,
            permissions: vec!["machine.everything".into()],
            entrypoint: Some("builtin:unsafe".into()),
        };
        let mut registry = CapabilityRegistry::default();
        assert!(matches!(
            registry.register(manifest),
            Err(PluginError::UnknownPermission { .. })
        ));
    }
}
