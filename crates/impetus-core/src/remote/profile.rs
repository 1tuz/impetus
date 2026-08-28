//! SSH profile definition with host-key verification and Keychain integration.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

/// SSH connection profile with host-key fingerprint for verification.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SSHProfile {
    pub host: String,
    pub user: String,
    pub port: u16,
    /// Expected host key fingerprint (SHA256). None for first connection.
    pub host_key_fingerprint: Option<HostKeyFingerprint>,
    /// Reference to SSH private key in Keychain (never the raw key).
    pub key_reference: SSHKeyReference,
}

impl SSHProfile {
    pub fn new(
        host: impl Into<String>,
        user: impl Into<String>,
        port: u16,
        key_reference: SSHKeyReference,
    ) -> Self {
        Self {
            host: host.into(),
            user: user.into(),
            port,
            host_key_fingerprint: None,
            key_reference,
        }
    }

    pub fn with_host_key(mut self, fingerprint: HostKeyFingerprint) -> Self {
        self.host_key_fingerprint = Some(fingerprint);
        self
    }
}

/// Reference to an SSH private key stored in Keychain.
/// Never contains the raw key material itself.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SSHKeyReference {
    /// macOS Keychain item (service + account name).
    KeychainItem { service: String, account: String },
    /// SSH agent socket path (for ssh-agent integration).
    SshAgent { socket_path: String },
}

/// SHA256 fingerprint of an SSH host public key.
/// Format: "SHA256:base64-encoded-hash"
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(transparent)]
pub struct HostKeyFingerprint(String);

impl HostKeyFingerprint {
    /// Compute SHA256 fingerprint from SSH public key bytes.
    pub fn from_public_key(key_bytes: &[u8]) -> Self {
        let digest = Sha256::digest(key_bytes);
        let encoded =
            base64::Engine::encode(&base64::engine::general_purpose::STANDARD_NO_PAD, digest);
        Self(format!("SHA256:{}", encoded))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for HostKeyFingerprint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum HostKeyVerificationError {
    #[error("host key mismatch: expected {expected}, but server presented {presented}")]
    Mismatch {
        expected: HostKeyFingerprint,
        presented: HostKeyFingerprint,
    },
    #[error("host key format invalid")]
    InvalidFormat,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_key_fingerprint_has_sha256_prefix() {
        let key_bytes = b"ssh-rsa AAAAB3NzaC1yc2EAAAADAQABAAABAQ";
        let fingerprint = HostKeyFingerprint::from_public_key(key_bytes);
        assert!(fingerprint.as_str().starts_with("SHA256:"));
    }

    #[test]
    fn same_key_produces_same_fingerprint() {
        let key_bytes = b"ssh-rsa AAAAB3NzaC1yc2EAAAADAQABAAABAQ";
        let fp1 = HostKeyFingerprint::from_public_key(key_bytes);
        let fp2 = HostKeyFingerprint::from_public_key(key_bytes);
        assert_eq!(fp1, fp2);
    }

    #[test]
    fn different_keys_produce_different_fingerprints() {
        let key1 = b"ssh-rsa AAAAB3NzaC1yc2EAAAADAQABAAABAQ";
        let key2 = b"ssh-rsa AAAAB3NzaC1yc2EXXXXXXXXXXXXXXXXX";
        let fp1 = HostKeyFingerprint::from_public_key(key1);
        let fp2 = HostKeyFingerprint::from_public_key(key2);
        assert_ne!(fp1, fp2);
    }

    #[test]
    fn ssh_profile_builder_pattern() {
        let profile = SSHProfile::new(
            "example.com",
            "user",
            22,
            SSHKeyReference::KeychainItem {
                service: "ssh".into(),
                account: "user".into(),
            },
        );

        assert_eq!(profile.host, "example.com");
        assert_eq!(profile.user, "user");
        assert_eq!(profile.port, 22);
        assert!(profile.host_key_fingerprint.is_none());

        let key_bytes = b"test-key";
        let fingerprint = HostKeyFingerprint::from_public_key(key_bytes);
        let profile = profile.with_host_key(fingerprint.clone());

        assert_eq!(profile.host_key_fingerprint, Some(fingerprint));
    }

    #[test]
    fn ssh_key_reference_serialization() {
        let keychain_ref = SSHKeyReference::KeychainItem {
            service: "ssh".into(),
            account: "testuser".into(),
        };

        let json = serde_json::to_string(&keychain_ref).expect("serialize");
        let deserialized: SSHKeyReference = serde_json::from_str(&json).expect("deserialize");

        assert_eq!(keychain_ref, deserialized);
    }

    #[test]
    fn host_key_fingerprint_display() {
        let key_bytes = b"test-key";
        let fingerprint = HostKeyFingerprint::from_public_key(key_bytes);
        let displayed = format!("{}", fingerprint);
        assert!(displayed.starts_with("SHA256:"));
        assert_eq!(displayed, fingerprint.as_str());
    }
}
