//! Chunked artifact upload sessions for large paste / client payloads.
//!
//! Pending uploads stay in memory until `finish` commits them into
//! [`DurableArtifactStore`]. Chunks and errors never enter the durable event
//! log — only the returned [`ArtifactRef`] is meant for later prompt use.

use crate::{DurableArtifactRef, DurableArtifactStore};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use thiserror::Error;
use uuid::Uuid;

pub use impetus_protocol::{MAX_ARTIFACT_UPLOAD_BYTES, MAX_ARTIFACT_UPLOAD_CHUNK_BYTES};

/// Concurrent pending uploads per harness process.
const MAX_CONCURRENT_UPLOADS: usize = 16;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ArtifactUploadError {
    #[error("upload {0} not found")]
    NotFound(Uuid),
    #[error("upload exceeds maximum size of {MAX_ARTIFACT_UPLOAD_BYTES} bytes")]
    TooLarge,
    #[error("chunk exceeds maximum size of {MAX_ARTIFACT_UPLOAD_CHUNK_BYTES} bytes")]
    ChunkTooLarge,
    #[error("too many concurrent uploads")]
    TooManyUploads,
    #[error("expected sequential chunk {expected}, got {got}")]
    SequenceMismatch { expected: u64, got: u64 },
    #[error("invalid base64 chunk")]
    InvalidBase64,
    #[error("declared byte count {declared} exceeded by received {received}")]
    DeclaredSizeExceeded { declared: usize, received: usize },
    #[error("declared byte count {declared} does not match received {received}")]
    DeclaredSizeMismatch { declared: usize, received: usize },
    #[error("upload store lock poisoned")]
    Poisoned,
    #[error("artifact store error")]
    Store,
}

#[derive(Debug)]
struct PendingUpload {
    declared_bytes: Option<usize>,
    content_type: Option<String>,
    next_seq: u64,
    buffer: Vec<u8>,
}

/// In-flight chunked uploads that finalize into a durable artifact store.
#[derive(Debug, Clone)]
pub struct ArtifactUploadStore {
    inner: Arc<Mutex<HashMap<Uuid, PendingUpload>>>,
    artifact_root: PathBuf,
}

impl ArtifactUploadStore {
    pub fn new(artifact_root: impl Into<PathBuf>) -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
            artifact_root: artifact_root.into(),
        }
    }

    pub fn artifact_root(&self) -> &Path {
        &self.artifact_root
    }

    pub fn begin(
        &self,
        _session_id: Uuid,
        declared_bytes: Option<usize>,
        content_type: Option<String>,
    ) -> Result<Uuid, ArtifactUploadError> {
        if let Some(declared) = declared_bytes
            && declared > MAX_ARTIFACT_UPLOAD_BYTES
        {
            return Err(ArtifactUploadError::TooLarge);
        }

        let mut pending = self
            .inner
            .lock()
            .map_err(|_| ArtifactUploadError::Poisoned)?;
        if pending.len() >= MAX_CONCURRENT_UPLOADS {
            return Err(ArtifactUploadError::TooManyUploads);
        }

        let upload_id = Uuid::new_v4();
        let capacity = declared_bytes.unwrap_or(0).min(MAX_ARTIFACT_UPLOAD_BYTES);
        pending.insert(
            upload_id,
            PendingUpload {
                declared_bytes,
                content_type,
                next_seq: 0,
                buffer: Vec::with_capacity(capacity),
            },
        );
        Ok(upload_id)
    }

    pub fn append_b64(
        &self,
        upload_id: Uuid,
        seq: u64,
        data_b64: &str,
    ) -> Result<usize, ArtifactUploadError> {
        let raw = BASE64
            .decode(data_b64.as_bytes())
            .map_err(|_| ArtifactUploadError::InvalidBase64)?;
        self.append(upload_id, seq, &raw)
    }

    pub fn append(
        &self,
        upload_id: Uuid,
        seq: u64,
        chunk: &[u8],
    ) -> Result<usize, ArtifactUploadError> {
        if chunk.len() > MAX_ARTIFACT_UPLOAD_CHUNK_BYTES {
            return Err(ArtifactUploadError::ChunkTooLarge);
        }

        let mut pending = self
            .inner
            .lock()
            .map_err(|_| ArtifactUploadError::Poisoned)?;
        let upload = pending
            .get_mut(&upload_id)
            .ok_or(ArtifactUploadError::NotFound(upload_id))?;

        if seq != upload.next_seq {
            return Err(ArtifactUploadError::SequenceMismatch {
                expected: upload.next_seq,
                got: seq,
            });
        }

        let new_len = upload.buffer.len().saturating_add(chunk.len());
        if new_len > MAX_ARTIFACT_UPLOAD_BYTES {
            return Err(ArtifactUploadError::TooLarge);
        }
        if let Some(declared) = upload.declared_bytes
            && new_len > declared
        {
            return Err(ArtifactUploadError::DeclaredSizeExceeded {
                declared,
                received: new_len,
            });
        }

        upload.buffer.extend_from_slice(chunk);
        upload.next_seq = upload.next_seq.saturating_add(1);
        Ok(upload.buffer.len())
    }

    pub fn finish(&self, upload_id: Uuid) -> Result<DurableArtifactRef, ArtifactUploadError> {
        let upload = {
            let mut pending = self
                .inner
                .lock()
                .map_err(|_| ArtifactUploadError::Poisoned)?;
            pending
                .remove(&upload_id)
                .ok_or(ArtifactUploadError::NotFound(upload_id))?
        };

        if let Some(declared) = upload.declared_bytes
            && declared != upload.buffer.len()
        {
            return Err(ArtifactUploadError::DeclaredSizeMismatch {
                declared,
                received: upload.buffer.len(),
            });
        }

        let store = DurableArtifactStore::open(&self.artifact_root)
            .map_err(|_| ArtifactUploadError::Store)?;
        store
            .store_with_content_type(&upload.buffer, upload.content_type.as_deref())
            .map_err(|_| ArtifactUploadError::Store)
    }

    pub fn abort(&self, upload_id: Uuid) -> Result<(), ArtifactUploadError> {
        let mut pending = self
            .inner
            .lock()
            .map_err(|_| ArtifactUploadError::Poisoned)?;
        if pending.remove(&upload_id).is_none() {
            return Err(ArtifactUploadError::NotFound(upload_id));
        }
        Ok(())
    }
}

/// Redacted IPC-safe message: no chunk/body content.
pub fn upload_error_message(error: &ArtifactUploadError) -> String {
    match error {
        ArtifactUploadError::NotFound(_) => "upload not found".into(),
        ArtifactUploadError::TooLarge => {
            format!("upload exceeds maximum size of {MAX_ARTIFACT_UPLOAD_BYTES} bytes")
        }
        ArtifactUploadError::ChunkTooLarge => {
            format!("chunk exceeds maximum size of {MAX_ARTIFACT_UPLOAD_CHUNK_BYTES} bytes")
        }
        ArtifactUploadError::TooManyUploads => "too many concurrent uploads".into(),
        ArtifactUploadError::SequenceMismatch { expected, got } => {
            format!("expected sequential chunk {expected}, got {got}")
        }
        ArtifactUploadError::InvalidBase64 => "invalid base64 chunk".into(),
        ArtifactUploadError::DeclaredSizeExceeded { .. } => {
            "received bytes exceed declared size".into()
        }
        ArtifactUploadError::DeclaredSizeMismatch { .. } => {
            "received bytes do not match declared size".into()
        }
        ArtifactUploadError::Poisoned => "upload store unavailable".into(),
        ArtifactUploadError::Store => "artifact store unavailable".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};

    fn temp_uploads() -> (tempfile::TempDir, ArtifactUploadStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = ArtifactUploadStore::new(dir.path().join("artifacts"));
        (dir, store)
    }

    #[test]
    fn begin_append_finish_matches_sha256() {
        let (_dir, uploads) = temp_uploads();
        let session = Uuid::new_v4();
        let body = b"large paste body for artifact upload";
        let upload_id = uploads.begin(session, Some(body.len()), None).unwrap();

        let received = uploads.append(upload_id, 0, body).unwrap();
        assert_eq!(received, body.len());

        let art = uploads.finish(upload_id).unwrap();
        assert_eq!(art.byte_count, body.len());

        let expected = format!("{:x}", Sha256::digest(body));
        assert_eq!(art.id, expected);

        let durable = DurableArtifactStore::open(uploads.artifact_root()).unwrap();
        assert_eq!(durable.read(&art.id).unwrap(), body);
    }

    #[test]
    fn multi_chunk_assembles_correctly() {
        let (_dir, uploads) = temp_uploads();
        let session = Uuid::new_v4();
        let upload_id = uploads.begin(session, None, None).unwrap();
        uploads.append(upload_id, 0, b"hello ").unwrap();
        uploads.append(upload_id, 1, b"world").unwrap();
        let art = uploads.finish(upload_id).unwrap();
        let durable = DurableArtifactStore::open(uploads.artifact_root()).unwrap();
        assert_eq!(durable.read(&art.id).unwrap(), b"hello world");
    }

    #[test]
    fn finish_persists_content_type() {
        let (_dir, uploads) = temp_uploads();
        let session = Uuid::new_v4();
        let body = b"typed upload";
        let upload_id = uploads
            .begin(session, Some(body.len()), Some("text/plain".into()))
            .unwrap();
        uploads.append(upload_id, 0, body).unwrap();
        let art = uploads.finish(upload_id).unwrap();
        let durable = DurableArtifactStore::open(uploads.artifact_root()).unwrap();
        let meta = durable.metadata(&art.id).unwrap().unwrap();
        assert_eq!(meta.content_type.as_deref(), Some("text/plain"));
    }

    #[test]
    fn rejects_oversized_declared_and_chunk() {
        let (_dir, uploads) = temp_uploads();
        let session = Uuid::new_v4();
        assert_eq!(
            uploads.begin(session, Some(MAX_ARTIFACT_UPLOAD_BYTES + 1), None),
            Err(ArtifactUploadError::TooLarge)
        );
        let upload_id = uploads.begin(session, None, None).unwrap();
        let big = vec![0u8; MAX_ARTIFACT_UPLOAD_CHUNK_BYTES + 1];
        assert_eq!(
            uploads.append(upload_id, 0, &big),
            Err(ArtifactUploadError::ChunkTooLarge)
        );
    }

    #[test]
    fn base64_append_and_abort() {
        let (_dir, uploads) = temp_uploads();
        let session = Uuid::new_v4();
        let upload_id = uploads.begin(session, None, None).unwrap();
        let b64 = BASE64.encode(b"secret-token-should-not-leak");
        uploads.append_b64(upload_id, 0, &b64).unwrap();
        uploads.abort(upload_id).unwrap();
        assert_eq!(
            uploads.finish(upload_id),
            Err(ArtifactUploadError::NotFound(upload_id))
        );
    }

    #[test]
    fn error_messages_omit_payload() {
        let msg = upload_error_message(&ArtifactUploadError::InvalidBase64);
        assert!(!msg.contains("secret"));
        assert_eq!(msg, "invalid base64 chunk");
    }
}
