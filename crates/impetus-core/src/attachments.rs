//! Bounded, redacted attachment storage for approval details and diffs.
//!
//! Attachments are ephemeral in-memory artifacts tied to a session lifecycle.
//! They never enter SQLite or long-term storage. Secrets are redacted before
//! storage using simple pattern-based heuristics.
//!
//! Reads require the owning `session_id`: foreign sessions are denied; missing
//! and TTL-expired attachments return deterministic errors.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use thiserror::Error;
use uuid::Uuid;

/// Maximum size per attachment: 10 MB
const MAX_ATTACHMENT_BYTES: usize = 10 * 1024 * 1024;

/// Maximum total store size: 100 MB
const MAX_STORE_BYTES: usize = 100 * 1024 * 1024;

/// Ephemeral TTL (24h). Attachments are approval/diff previews, not durable.
const ATTACHMENT_TTL_MS: u64 = 24 * 60 * 60 * 1000;

#[derive(Debug, Error)]
pub enum AttachmentError {
    #[error("attachment {0} not found")]
    NotFound(Uuid),
    #[error("attachment {0} expired")]
    Expired(Uuid),
    #[error("attachment {0} denied: not owned by session")]
    Forbidden(Uuid),
    #[error("attachment exceeds maximum size of {MAX_ATTACHMENT_BYTES} bytes")]
    TooLarge,
    #[error("store capacity exceeded (max {MAX_STORE_BYTES} bytes)")]
    StoreFull,
    #[error("store lock poisoned")]
    Poisoned,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attachment {
    pub id: Uuid,
    pub session_id: Uuid,
    pub content_type: String,
    pub content: Vec<u8>,
    pub created_unix_ms: u64,
}

#[derive(Debug, Clone, Default)]
struct StoreInner {
    attachments: BTreeMap<Uuid, Attachment>,
    total_bytes: usize,
}

/// In-memory attachment store with bounded size and secret redaction.
#[derive(Debug, Clone)]
pub struct AttachmentStore {
    inner: Arc<Mutex<StoreInner>>,
}

impl Default for AttachmentStore {
    fn default() -> Self {
        Self::new()
    }
}

impl AttachmentStore {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(StoreInner::default())),
        }
    }

    /// Store content for `session_id` with automatic secret redaction.
    /// Returns the generated attachment ID.
    pub fn store(
        &self,
        session_id: Uuid,
        content_type: String,
        content: Vec<u8>,
    ) -> Result<Uuid, AttachmentError> {
        if content.len() > MAX_ATTACHMENT_BYTES {
            return Err(AttachmentError::TooLarge);
        }

        let redacted_content = redact_secrets(&content);
        let id = Uuid::new_v4();
        let attachment = Attachment {
            id,
            session_id,
            content_type,
            content: redacted_content,
            created_unix_ms: now_unix_ms(),
        };

        let mut inner = self.inner.lock().map_err(|_| AttachmentError::Poisoned)?;

        // Check capacity
        let new_total = inner.total_bytes + attachment.content.len();
        if new_total > MAX_STORE_BYTES {
            return Err(AttachmentError::StoreFull);
        }

        inner.total_bytes = new_total;
        inner.attachments.insert(id, attachment);

        Ok(id)
    }

    /// Retrieve an attachment by ID for the owning session.
    ///
    /// - Missing → [`AttachmentError::NotFound`]
    /// - TTL expired → remove + [`AttachmentError::Expired`]
    /// - Foreign `session_id` → [`AttachmentError::Forbidden`]
    pub fn get(&self, session_id: Uuid, id: Uuid) -> Result<Attachment, AttachmentError> {
        let mut inner = self.inner.lock().map_err(|_| AttachmentError::Poisoned)?;
        let Some(attachment) = inner.attachments.get(&id).cloned() else {
            return Err(AttachmentError::NotFound(id));
        };

        if attachment.session_id != session_id {
            return Err(AttachmentError::Forbidden(id));
        }

        if is_expired(attachment.created_unix_ms, now_unix_ms()) {
            if let Some(removed) = inner.attachments.remove(&id) {
                inner.total_bytes = inner.total_bytes.saturating_sub(removed.content.len());
            }
            return Err(AttachmentError::Expired(id));
        }

        Ok(attachment)
    }

    /// Remove an attachment and reclaim space.
    pub fn remove(&self, id: Uuid) -> Result<(), AttachmentError> {
        let mut inner = self.inner.lock().map_err(|_| AttachmentError::Poisoned)?;
        if let Some(attachment) = inner.attachments.remove(&id) {
            inner.total_bytes = inner.total_bytes.saturating_sub(attachment.content.len());
        }
        Ok(())
    }

    /// Get current store statistics.
    pub fn stats(&self) -> Result<StoreStats, AttachmentError> {
        let inner = self.inner.lock().map_err(|_| AttachmentError::Poisoned)?;
        Ok(StoreStats {
            count: inner.attachments.len(),
            total_bytes: inner.total_bytes,
            capacity_bytes: MAX_STORE_BYTES,
        })
    }

    /// Clear all attachments.
    pub fn clear(&self) -> Result<(), AttachmentError> {
        let mut inner = self.inner.lock().map_err(|_| AttachmentError::Poisoned)?;
        inner.attachments.clear();
        inner.total_bytes = 0;
        Ok(())
    }

    /// Test helper: rewrite `created_unix_ms` so TTL expiry can be asserted without sleep.
    #[cfg(test)]
    fn force_created_unix_ms(&self, id: Uuid, created_unix_ms: u64) {
        let mut inner = self.inner.lock().expect("lock");
        if let Some(attachment) = inner.attachments.get_mut(&id) {
            attachment.created_unix_ms = created_unix_ms;
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreStats {
    pub count: usize,
    pub total_bytes: usize,
    pub capacity_bytes: usize,
}

fn is_expired(created_unix_ms: u64, now_ms: u64) -> bool {
    now_ms.saturating_sub(created_unix_ms) > ATTACHMENT_TTL_MS
}

/// Redact common secret patterns from byte content.
/// Simple heuristic-based approach; not cryptographically secure.
fn redact_secrets(content: &[u8]) -> Vec<u8> {
    let text = String::from_utf8_lossy(content);
    let mut redacted = text.to_string();

    // Redact common secret patterns
    let patterns = [
        // API keys
        (r"(api[_-]?key\s*[:=]\s*)([^\s\n]+)", "$1[REDACTED]"),
        // Tokens
        (r"(token\s*[:=]\s*)([^\s\n]+)", "$1[REDACTED]"),
        // Passwords
        (r"(password\s*[:=]\s*)([^\s\n]+)", "$1[REDACTED]"),
        // AWS keys
        (r"(AKIA[0-9A-Z]{16})", "[REDACTED_AWS_KEY]"),
        // Private keys (PEM headers)
        (
            r"-----BEGIN [A-Z ]+PRIVATE KEY-----[^-]+-----END [A-Z ]+PRIVATE KEY-----",
            "-----BEGIN PRIVATE KEY-----\n[REDACTED]\n-----END PRIVATE KEY-----",
        ),
        // Bearer tokens
        (r"(Bearer\s+)([^\s\n]+)", "$1[REDACTED]"),
        // Generic base64-looking secrets (40+ chars)
        (
            r"(secret\s*[:=]\s*)([A-Za-z0-9+/]{40,}={0,2})",
            "$1[REDACTED]",
        ),
    ];

    for (pattern, replacement) in patterns {
        if let Ok(re) = regex::Regex::new(pattern) {
            redacted = re.replace_all(&redacted, replacement).to_string();
        }
    }

    redacted.into_bytes()
}

fn now_unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn store_and_retrieve_for_owner() {
        let store = AttachmentStore::new();
        let session_id = Uuid::new_v4();
        let content = b"test content".to_vec();
        let id = store
            .store(session_id, "text/plain".into(), content.clone())
            .expect("store");

        let attachment = store.get(session_id, id).expect("retrieve");
        assert_eq!(attachment.id, id);
        assert_eq!(attachment.session_id, session_id);
        assert_eq!(attachment.content_type, "text/plain");
        assert_eq!(attachment.content, content);
    }

    #[test]
    fn foreign_session_denied() {
        let store = AttachmentStore::new();
        let owner = Uuid::new_v4();
        let foreign = Uuid::new_v4();
        let id = store
            .store(owner, "text/plain".into(), b"secret-preview".to_vec())
            .expect("store");

        assert!(matches!(
            store.get(foreign, id),
            Err(AttachmentError::Forbidden(_))
        ));
        // Owner still reads; foreign deny must not remove.
        assert!(store.get(owner, id).is_ok());
    }

    #[test]
    fn missing_attachment_not_found() {
        let store = AttachmentStore::new();
        let session_id = Uuid::new_v4();
        let missing = Uuid::new_v4();
        assert!(matches!(
            store.get(session_id, missing),
            Err(AttachmentError::NotFound(_))
        ));
    }

    #[test]
    fn expired_attachment_errors_and_removes() {
        let store = AttachmentStore::new();
        let session_id = Uuid::new_v4();
        let id = store
            .store(session_id, "text/plain".into(), b"stale".to_vec())
            .expect("store");
        store.force_created_unix_ms(id, 1);

        assert!(matches!(
            store.get(session_id, id),
            Err(AttachmentError::Expired(_))
        ));
        assert!(matches!(
            store.get(session_id, id),
            Err(AttachmentError::NotFound(_))
        ));
        assert_eq!(store.stats().expect("stats").count, 0);
    }

    #[test]
    fn redaction_removes_api_keys() {
        let store = AttachmentStore::new();
        let session_id = Uuid::new_v4();
        let content = b"api_key: sk-1234567890abcdef\ndata: value".to_vec();
        let id = store
            .store(session_id, "text/plain".into(), content)
            .expect("store");

        let attachment = store.get(session_id, id).expect("retrieve");
        let text = String::from_utf8_lossy(&attachment.content);
        assert!(text.contains("[REDACTED]"));
        assert!(!text.contains("sk-1234567890abcdef"));
    }

    #[test]
    fn redaction_removes_bearer_tokens() {
        let content = b"Authorization: Bearer eyJhbGc...secret";
        let redacted = redact_secrets(content);
        let text = String::from_utf8_lossy(&redacted);
        assert!(text.contains("[REDACTED]"));
        assert!(!text.contains("eyJhbGc"));
    }

    #[test]
    fn bounded_size_per_attachment() {
        let store = AttachmentStore::new();
        let large = vec![0u8; MAX_ATTACHMENT_BYTES + 1];
        let result = store.store(Uuid::new_v4(), "application/octet-stream".into(), large);
        assert!(matches!(result, Err(AttachmentError::TooLarge)));
    }

    #[test]
    fn bounded_total_store_size() {
        let store = AttachmentStore::new();
        let session_id = Uuid::new_v4();
        let chunk_size = MAX_ATTACHMENT_BYTES / 2;
        let mut stored = 0;

        // Fill store to capacity
        loop {
            let content = vec![0u8; chunk_size];
            match store.store(session_id, "application/octet-stream".into(), content) {
                Ok(_) => stored += chunk_size,
                Err(AttachmentError::StoreFull) => break,
                Err(e) => panic!("unexpected error: {e}"),
            }
        }

        assert!(stored >= MAX_STORE_BYTES - chunk_size);
        assert!(stored <= MAX_STORE_BYTES);
    }

    #[test]
    fn remove_reclaims_space() {
        let store = AttachmentStore::new();
        let session_id = Uuid::new_v4();
        let content = vec![0u8; 1024];
        let id = store
            .store(session_id, "application/octet-stream".into(), content)
            .expect("store");

        let stats_before = store.stats().expect("stats");
        assert_eq!(stats_before.total_bytes, 1024);

        store.remove(id).expect("remove");

        let stats_after = store.stats().expect("stats");
        assert_eq!(stats_after.total_bytes, 0);
        assert_eq!(stats_after.count, 0);
    }

    #[test]
    fn clear_removes_all() {
        let store = AttachmentStore::new();
        let session_id = Uuid::new_v4();
        for _ in 0..5 {
            store
                .store(session_id, "text/plain".into(), b"test".to_vec())
                .expect("store");
        }

        let stats = store.stats().expect("stats");
        assert_eq!(stats.count, 5);

        store.clear().expect("clear");

        let stats = store.stats().expect("stats");
        assert_eq!(stats.count, 0);
        assert_eq!(stats.total_bytes, 0);
    }
}
