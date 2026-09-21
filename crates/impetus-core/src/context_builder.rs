//! Context Builder: materialize durable artifacts into prompt-sized text.
//!
//! Large tool bodies stay on disk as [`DurableArtifactRef`]. This module reads
//! them in [`ARTIFACT_CHUNK_SIZE`] windows via [`ArtifactRangeSource::read_range`],
//! redacts, and incrementally reduces with [`OutputReducer`] so model context
//! never grows with the full artifact.
//!
//! # Ceiling
//! [`DurableArtifactStore::read_range`] still loads the whole file into RAM
//! before slicing. Peak process memory for one range call is therefore O(file
//! size) until the store grows true seek/partial IO. Context Builder still
//! avoids concatenating every decoded chunk before reduce, and never calls
//! full [`DurableArtifactStore::read`] for large bodies.

use crate::durable_artifacts::{ArtifactMeta, ArtifactRef, DurableArtifactStore};
use crate::output_reducer::{OutputReducer, TokenBudget};
use crate::tools::{ARTIFACT_CHUNK_SIZE, redact_text};
use thiserror::Error;

/// Source of artifact metadata and byte ranges. Deliberately omits full-body
/// `read` so callers cannot accidentally dump large artifacts into context.
pub trait ArtifactRangeSource {
    fn metadata(&self, id: &str) -> Result<Option<ArtifactMeta>, ContextBuilderError>;
    fn read_range(
        &self,
        id: &str,
        start: usize,
        len: usize,
    ) -> Result<Vec<u8>, ContextBuilderError>;
}

impl ArtifactRangeSource for DurableArtifactStore {
    fn metadata(&self, id: &str) -> Result<Option<ArtifactMeta>, ContextBuilderError> {
        DurableArtifactStore::metadata(self, id)
            .map_err(|error| ContextBuilderError::Store(error.to_string()))
    }

    fn read_range(
        &self,
        id: &str,
        start: usize,
        len: usize,
    ) -> Result<Vec<u8>, ContextBuilderError> {
        DurableArtifactStore::read_range(self, id, start, len)
            .map_err(|error| ContextBuilderError::Store(error.to_string()))
    }
}

/// Bounded materialization of an artifact for provider messages.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MaterializedArtifact {
    pub content: String,
    pub truncated: bool,
    pub chunks_read: usize,
    pub byte_count: usize,
    pub original_tokens: usize,
    pub reduced_tokens: usize,
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ContextBuilderError {
    #[error("artifact not found: {id}")]
    MissingArtifact { id: String },
    #[error("artifact store error: {0}")]
    Store(String),
}

/// Chunked artifact reader + token-budget summarizer for prompt inclusion.
pub struct ContextBuilder<'a, S: ArtifactRangeSource + ?Sized> {
    source: &'a S,
    reducer: OutputReducer,
    budget_tokens: usize,
    chunk_size: usize,
}

impl<'a, S: ArtifactRangeSource + ?Sized> ContextBuilder<'a, S> {
    pub fn new(source: &'a S, budget: TokenBudget) -> Self {
        Self {
            source,
            // Tests and prompt paths must stay deterministic without RTK.
            reducer: OutputReducer::new_without_rtk(budget),
            budget_tokens: budget.max_tokens,
            chunk_size: ARTIFACT_CHUNK_SIZE,
        }
    }

    pub fn with_chunk_size(mut self, chunk_size: usize) -> Self {
        self.chunk_size = chunk_size.max(1);
        self
    }

    /// Materialize `artifact_ref` into redacted text within the token budget.
    ///
    /// Uses only [`ArtifactRangeSource::read_range`] (never a full-body read).
    /// When `byte_count > chunk_size`, reads multiple windows and reduces after
    /// each append so the working string stays near the budget.
    pub fn materialize(
        &self,
        artifact_ref: &ArtifactRef,
    ) -> Result<MaterializedArtifact, ContextBuilderError> {
        let meta = self.source.metadata(&artifact_ref.id)?.ok_or_else(|| {
            ContextBuilderError::MissingArtifact {
                id: artifact_ref.id.clone(),
            }
        })?;

        let total = meta.byte_count;
        let chunk_size = self.chunk_size;
        let mut offset = 0usize;
        let mut chunks_read = 0usize;
        let mut carry = Vec::new();
        let mut assembled = String::new();
        let mut last_original_tokens = 0usize;
        let mut truncated = false;

        while offset < total {
            let want = chunk_size.min(total.saturating_sub(offset));
            let mut bytes = self.source.read_range(&artifact_ref.id, offset, want)?;
            let read_len = bytes.len();
            chunks_read = chunks_read.saturating_add(1);
            if read_len == 0 {
                break;
            }
            offset = offset.saturating_add(read_len);

            if !carry.is_empty() {
                let mut merged = std::mem::take(&mut carry);
                merged.append(&mut bytes);
                bytes = merged;
            }

            let (valid, rest) = split_utf8_prefix(&bytes);
            carry.extend_from_slice(rest);
            // `valid` is a complete UTF-8 prefix by construction.
            let chunk_text = String::from_utf8(valid)
                .unwrap_or_else(|_| unreachable!("split_utf8_prefix guarantees UTF-8"));
            assembled.push_str(&redact_text(&chunk_text));

            let reduced = self.reducer.reduce(&assembled);
            last_original_tokens = reduced.original_tokens;
            if reduced.truncated {
                truncated = true;
                assembled = reduced.content.into_owned();
            }
        }

        if !carry.is_empty() {
            let lossy = String::from_utf8_lossy(&carry);
            assembled.push_str(&redact_text(&lossy));
            let reduced = self.reducer.reduce(&assembled);
            last_original_tokens = reduced.original_tokens;
            if reduced.truncated {
                truncated = true;
                assembled = reduced.content.into_owned();
            }
        }

        // Final pass + hard ceiling (builtin reducer suffix can slightly overshoot).
        let final_reduced = self.reducer.reduce(&assembled);
        let (content, reduced_tokens, hard_truncated) =
            enforce_token_ceiling(final_reduced.content.as_ref(), self.budget_tokens);
        Ok(MaterializedArtifact {
            content,
            truncated: truncated || final_reduced.truncated || hard_truncated,
            chunks_read,
            byte_count: total,
            original_tokens: final_reduced.original_tokens.max(last_original_tokens),
            reduced_tokens,
        })
    }
}

/// Hard ceiling so prompt inclusion never exceeds `max_tokens`.
fn enforce_token_ceiling(text: &str, max_tokens: usize) -> (String, usize, bool) {
    let max_chars = max_tokens.saturating_mul(4);
    if text.len() <= max_chars {
        return (text.to_string(), text.len().div_ceil(4), false);
    }
    let truncated: String = text.chars().take(max_chars).collect();
    let tokens = truncated.len().div_ceil(4).min(max_tokens);
    (truncated, tokens, true)
}

/// Split `bytes` into the longest valid UTF-8 prefix and a short incomplete suffix.
fn split_utf8_prefix(bytes: &[u8]) -> (Vec<u8>, &[u8]) {
    match std::str::from_utf8(bytes) {
        Ok(_) => (bytes.to_vec(), &[]),
        Err(error) => {
            let valid_up_to = error.valid_up_to();
            let incomplete = &bytes[valid_up_to..];
            // Incomplete sequence at end → carry; invalid mid-stream → skip bad byte.
            if error.error_len().is_none() {
                (bytes[..valid_up_to].to_vec(), incomplete)
            } else {
                // Replace invalid sequence with U+FFFD via lossy on the bad slice,
                // then continue from after the error.
                let bad_len = error.error_len().unwrap_or(1);
                let mut out = bytes[..valid_up_to].to_vec();
                out.extend_from_slice("\u{FFFD}".as_bytes());
                let rest = &bytes[valid_up_to.saturating_add(bad_len)..];
                let (more, carry) = split_utf8_prefix(rest);
                out.extend_from_slice(&more);
                (out, carry)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct SpyStore {
        inner: DurableArtifactStore,
        full_reads: AtomicUsize,
        range_reads: AtomicUsize,
    }

    impl SpyStore {
        fn new(inner: DurableArtifactStore) -> Self {
            Self {
                inner,
                full_reads: AtomicUsize::new(0),
                range_reads: AtomicUsize::new(0),
            }
        }

        fn read(&self, id: &str) -> Result<Vec<u8>, ContextBuilderError> {
            self.full_reads.fetch_add(1, Ordering::SeqCst);
            self.inner
                .read(id)
                .map_err(|error| ContextBuilderError::Store(error.to_string()))
        }
    }

    impl ArtifactRangeSource for SpyStore {
        fn metadata(&self, id: &str) -> Result<Option<ArtifactMeta>, ContextBuilderError> {
            DurableArtifactStore::metadata(&self.inner, id)
                .map_err(|error| ContextBuilderError::Store(error.to_string()))
        }

        fn read_range(
            &self,
            id: &str,
            start: usize,
            len: usize,
        ) -> Result<Vec<u8>, ContextBuilderError> {
            self.range_reads.fetch_add(1, Ordering::SeqCst);
            DurableArtifactStore::read_range(&self.inner, id, start, len)
                .map_err(|error| ContextBuilderError::Store(error.to_string()))
        }
    }

    fn temp_store() -> (tempfile::TempDir, DurableArtifactStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = DurableArtifactStore::open(dir.path()).unwrap();
        (dir, store)
    }

    #[test]
    fn small_artifact_one_range_read() {
        let (_dir, store) = temp_store();
        let art = store.store(b"hello artifact").unwrap();
        let spy = SpyStore::new(store);
        let builder = ContextBuilder::new(&spy, TokenBudget { max_tokens: 2000 });
        let out = builder.materialize(&art).unwrap();
        assert_eq!(out.chunks_read, 1);
        assert_eq!(spy.range_reads.load(Ordering::SeqCst), 1);
        assert_eq!(spy.full_reads.load(Ordering::SeqCst), 0);
        assert!(out.content.contains("hello artifact"));
        assert!(!out.truncated);
        // Prove spy still has a full-read path that was never used.
        let _ = spy.read(&art.id);
        assert_eq!(spy.full_reads.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn large_artifact_never_calls_full_read() {
        let (_dir, store) = temp_store();
        let body = vec![b'x'; 100 * 1024];
        let art = store.store(&body).unwrap();
        let spy = SpyStore::new(store);
        let builder = ContextBuilder::new(&spy, TokenBudget { max_tokens: 64 })
            .with_chunk_size(ARTIFACT_CHUNK_SIZE);
        let out = builder.materialize(&art).unwrap();
        assert!(out.chunks_read > 1);
        assert!(out.truncated);
        assert!(out.reduced_tokens <= 64);
        assert_eq!(spy.full_reads.load(Ordering::SeqCst), 0);
        assert_eq!(
            spy.range_reads.load(Ordering::SeqCst),
            out.chunks_read,
            "only range reads"
        );
    }

    #[test]
    fn missing_artifact_is_structured_error() {
        let (_dir, store) = temp_store();
        let builder = ContextBuilder::new(&store, TokenBudget::default());
        let missing = ArtifactRef {
            id: "deadbeef".into(),
            byte_count: 0,
        };
        let err = builder.materialize(&missing).unwrap_err();
        assert_eq!(
            err,
            ContextBuilderError::MissingArtifact {
                id: "deadbeef".into()
            }
        );
    }

    #[test]
    fn materialize_is_deterministic() {
        let (_dir, store) = temp_store();
        let body = (0..80_000)
            .map(|i| format!("line-{i}\n"))
            .collect::<String>();
        let art = store.store(body.as_bytes()).unwrap();
        let builder = ContextBuilder::new(&store, TokenBudget { max_tokens: 80 });
        let a = builder.materialize(&art).unwrap();
        let b = builder.materialize(&art).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn utf8_boundary_across_chunks_is_safe() {
        let (_dir, store) = temp_store();
        // '€' is 3 bytes (E2 82 AC). Place it across a 4-byte chunk boundary.
        let mut body = Vec::new();
        body.extend_from_slice(b"ab");
        body.extend_from_slice("€".as_bytes());
        body.extend_from_slice(b"cd");
        let art = store.store(&body).unwrap();
        let builder =
            ContextBuilder::new(&store, TokenBudget { max_tokens: 2000 }).with_chunk_size(4);
        let out = builder.materialize(&art).unwrap();
        assert!(out.content.contains('€'), "got {:?}", out.content);
        assert!(out.content.contains("ab"));
        assert!(out.content.contains("cd"));
    }

    #[test]
    fn redacts_secrets_in_materialized_text() {
        let (_dir, store) = temp_store();
        let art = store.store(b"API_TOKEN=raw-secret\nvisible=ok\n").unwrap();
        let out = ContextBuilder::new(&store, TokenBudget::default())
            .materialize(&art)
            .unwrap();
        assert!(out.content.contains("[REDACTED]"));
        assert!(!out.content.contains("raw-secret"));
        assert!(out.content.contains("visible=ok"));
    }
}
