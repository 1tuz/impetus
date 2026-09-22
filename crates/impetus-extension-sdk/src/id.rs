//! Extension identifier newtype matching core allowlist rules.
//!
//! IDs must match `[a-z0-9][a-z0-9_-]{0,63}` (length 1..=64). Same rules as
//! `impetus_core::extension_id::is_valid_extension_id` — this crate does not
//! depend on core.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use thiserror::Error;

/// Extension identifier validation failure.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ExtensionIdError {
    #[error("extension id must be non-empty")]
    Empty,
    #[error(
        "extension id `{id}` is invalid: must match [a-z0-9][a-z0-9_-]{{0,63}} (length 1..=64)"
    )]
    InvalidFormat { id: String },
}

/// Validated extension package id.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ExtensionId(String);

impl ExtensionId {
    /// Validate and wrap `id`.
    pub fn new(id: impl Into<String>) -> Result<Self, ExtensionIdError> {
        let id = id.into();
        if id.is_empty() {
            return Err(ExtensionIdError::Empty);
        }
        if !is_valid_extension_id(&id) {
            return Err(ExtensionIdError::InvalidFormat { id });
        }
        Ok(Self(id))
    }

    /// Borrow the inner id string.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// True iff `id` matches `[a-z0-9][a-z0-9_-]{0,63}` (len 1..=64).
pub fn is_valid_extension_id(id: &str) -> bool {
    let bytes = id.as_bytes();
    if !(1..=64).contains(&bytes.len()) {
        return false;
    }
    let first = bytes[0];
    if !first.is_ascii_lowercase() && !first.is_ascii_digit() {
        return false;
    }
    bytes[1..]
        .iter()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'_' || *b == b'-')
}

impl AsRef<str> for ExtensionId {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl fmt::Display for ExtensionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for ExtensionId {
    type Err = ExtensionIdError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::new(s)
    }
}

impl Serialize for ExtensionId {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for ExtensionId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        Self::new(s).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_valid_ids() {
        assert!(ExtensionId::new("a").is_ok());
        assert!(ExtensionId::new("demo-skill").is_ok());
        assert!(ExtensionId::new("demo_skill").is_ok());
        assert!(ExtensionId::new("0x").is_ok());
    }

    #[test]
    fn rejects_invalid_ids() {
        assert!(ExtensionId::new("").is_err());
        assert!(ExtensionId::new("Bad").is_err());
        assert!(ExtensionId::new("has space").is_err());
        assert!(ExtensionId::new("dot.bad").is_err());
        assert!(ExtensionId::new("a".repeat(65)).is_err());
    }

    #[test]
    fn from_str_and_display() {
        let id: ExtensionId = "my-ext".parse().unwrap();
        assert_eq!(id.to_string(), "my-ext");
    }
}
