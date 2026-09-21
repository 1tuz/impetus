//! Context Builder integration: chunked artifact materialization under budget.

use impetus_core::{
    ARTIFACT_CHUNK_SIZE, ArtifactRangeSource, ContextBuilder, ContextBuilderError,
    DurableArtifactMeta, DurableArtifactRef, DurableArtifactStore, MaterializedArtifact,
    TokenBudget,
};
use std::sync::atomic::{AtomicUsize, Ordering};

/// Spy that records full vs range reads. Context Builder must only use range.
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

    #[allow(dead_code)]
    fn read(&self, id: &str) -> anyhow::Result<Vec<u8>> {
        self.full_reads.fetch_add(1, Ordering::SeqCst);
        self.inner.read(id)
    }
}

impl ArtifactRangeSource for SpyStore {
    fn metadata(&self, id: &str) -> Result<Option<DurableArtifactMeta>, ContextBuilderError> {
        self.inner
            .metadata(id)
            .map_err(|error| ContextBuilderError::Store(error.to_string()))
    }

    fn read_range(
        &self,
        id: &str,
        start: usize,
        len: usize,
    ) -> Result<Vec<u8>, ContextBuilderError> {
        self.range_reads.fetch_add(1, Ordering::SeqCst);
        self.inner
            .read_range(id, start, len)
            .map_err(|error| ContextBuilderError::Store(error.to_string()))
    }
}

fn temp_store() -> (tempfile::TempDir, DurableArtifactStore) {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = DurableArtifactStore::open(dir.path()).expect("open");
    (dir, store)
}

#[test]
fn small_artifact_single_chunk() {
    let (_dir, store) = temp_store();
    let art = store.store(b"short body").expect("store");
    let spy = SpyStore::new(store);
    let out = ContextBuilder::new(&spy, TokenBudget { max_tokens: 500 })
        .materialize(&art)
        .expect("materialize");
    assert_eq!(out.chunks_read, 1);
    assert_eq!(spy.range_reads.load(Ordering::SeqCst), 1);
    assert_eq!(spy.full_reads.load(Ordering::SeqCst), 0);
    assert!(out.content.contains("short body"));
    assert!(!out.truncated);
}

#[test]
fn large_synthetic_stays_under_budget_without_full_read() {
    let (_dir, store) = temp_store();
    let body = {
        let mut buf = Vec::with_capacity(120 * 1024);
        for i in 0..(120 * 1024 / 16) {
            buf.extend_from_slice(format!("line-{i:08}\n").as_bytes());
        }
        buf
    };
    assert!(body.len() > 100 * 1024);
    let art = store.store(&body).expect("store");
    let spy = SpyStore::new(store);
    let budget = TokenBudget { max_tokens: 128 };
    let out: MaterializedArtifact = ContextBuilder::new(&spy, budget)
        .with_chunk_size(ARTIFACT_CHUNK_SIZE)
        .materialize(&art)
        .expect("materialize");

    assert!(
        out.chunks_read > 1,
        "expected multiple chunks, got {}",
        out.chunks_read
    );
    assert!(out.truncated);
    assert!(
        out.reduced_tokens <= budget.max_tokens,
        "reduced_tokens={} budget={}",
        out.reduced_tokens,
        budget.max_tokens
    );
    assert_eq!(spy.full_reads.load(Ordering::SeqCst), 0);
    assert_eq!(spy.range_reads.load(Ordering::SeqCst), out.chunks_read);
    assert_eq!(out.byte_count, body.len());
}

#[test]
fn missing_artifact_returns_structured_error() {
    let (_dir, store) = temp_store();
    let missing = DurableArtifactRef {
        id: "missing-artifact-id".into(),
        byte_count: 12,
    };
    let err = ContextBuilder::new(&store, TokenBudget::default())
        .materialize(&missing)
        .expect_err("missing");
    assert_eq!(
        err,
        ContextBuilderError::MissingArtifact {
            id: "missing-artifact-id".into()
        }
    );
}

#[test]
fn materialize_deterministic_and_redacting() {
    let (_dir, store) = temp_store();
    let body = format!(
        "API_TOKEN=super-secret\n{}\n",
        (0..4_000)
            .map(|i| format!("row-{i}"))
            .collect::<Vec<_>>()
            .join("\n")
    );
    let art = store.store(body.as_bytes()).expect("store");
    let builder = ContextBuilder::new(&store, TokenBudget { max_tokens: 100 });
    let a = builder.materialize(&art).expect("a");
    let b = builder.materialize(&art).expect("b");
    assert_eq!(a, b);
    assert!(a.content.contains("[REDACTED]"));
    assert!(!a.content.contains("super-secret"));
}

#[test]
fn utf8_safe_across_chunk_boundary() {
    let (_dir, store) = temp_store();
    let mut body = Vec::new();
    body.extend_from_slice(b"xy");
    body.extend_from_slice("あ".as_bytes()); // 3-byte UTF-8
    body.extend_from_slice(b"z");
    let art = store.store(&body).expect("store");
    let out = ContextBuilder::new(&store, TokenBudget { max_tokens: 500 })
        .with_chunk_size(4)
        .materialize(&art)
        .expect("materialize");
    assert!(out.content.contains('あ'), "content={:?}", out.content);
    assert!(out.content.contains("xy"));
    assert!(out.content.contains('z'));
}
