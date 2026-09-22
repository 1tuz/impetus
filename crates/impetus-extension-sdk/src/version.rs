//! Extension API version and supported range advertised by the host.

use serde::{Deserialize, Serialize};

/// Current extension API major version shipped with this SDK.
pub const EXTENSION_API_VERSION: u32 = 1;

/// Typed extension API version (semver major; not Impetus app version).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ExtensionApiVersion(pub u32);

impl ExtensionApiVersion {
    /// Construct from a raw major version.
    pub const fn new(major: u32) -> Self {
        Self(major)
    }

    /// Raw major version number.
    pub const fn get(self) -> u32 {
        self.0
    }
}

impl From<u32> for ExtensionApiVersion {
    fn from(value: u32) -> Self {
        Self(value)
    }
}

impl std::fmt::Display for ExtensionApiVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Inclusive range of API majors a host accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SupportedApiRange {
    pub min: ExtensionApiVersion,
    pub max: ExtensionApiVersion,
}

impl SupportedApiRange {
    /// Build an inclusive `[min, max]` range.
    pub const fn new(min: ExtensionApiVersion, max: ExtensionApiVersion) -> Self {
        Self { min, max }
    }

    /// True when `version` falls inside `[min, max]` inclusive.
    pub fn contains(self, version: ExtensionApiVersion) -> bool {
        version >= self.min && version <= self.max
    }
}

/// Range advertised by hosts built against this SDK revision.
pub const CURRENT_SUPPORTED_RANGE: SupportedApiRange = SupportedApiRange {
    min: ExtensionApiVersion(1),
    max: ExtensionApiVersion(1),
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_range_contains_api_version() {
        assert!(CURRENT_SUPPORTED_RANGE.contains(ExtensionApiVersion(EXTENSION_API_VERSION)));
    }
}
