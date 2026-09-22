//! Event log baselines for SqliteEventStore hot paths.
//!
//! Local-only: numbers move with disk/CPU noise. Do not gate PR CI on timings.
//!
//! Ops covered:
//! - `append_next` (runtime write path; re-lists history per call)
//! - `list` (full session / range materialization)
//! - cursor backfill (`list` + `sequence > after_sequence`, same filter as IPC Stream)
//! - agent Chunk ingest (`append_next` of `AgentEvent::Chunk` at coalesce / max sizes)
//! - Stream frame encode (`IpcResponse::Events` JSON line size vs 64 KiB IPC cap)

use std::time::Duration;

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use impetus_core::{
    AGENT_CHUNK_COALESCE_BYTES, AgentEvent, Event, EventPayload, EventStore, IntentEvent,
    IpcResponse, MAX_AGENT_CHUNK_EVENT_BYTES, MAX_IPC_LINE_BYTES, SqliteEventStore,
};
use tempfile::TempDir;
use uuid::Uuid;

struct SeededStore {
    _dir: TempDir,
    store: std::sync::Arc<SqliteEventStore>,
    session_id: Uuid,
    /// Total durable events including Session::Created from `create_session`.
    event_count: u64,
}

fn open_store() -> (TempDir, std::sync::Arc<SqliteEventStore>) {
    let dir = TempDir::new().expect("tempdir");
    let path = dir.path().join("events.sqlite3");
    let store = SqliteEventStore::open(&path).expect("open sqlite");
    (dir, store)
}

/// Seed via `append` with known sequences (avoids O(n²) `append_next` re-list).
fn seed_events(extra_intents: u64) -> SeededStore {
    let (dir, store) = open_store();
    let session_id = store.create_session().expect("create session");
    for sequence in 2..=(extra_intents + 1) {
        let event = Event::new(
            session_id,
            sequence,
            EventPayload::Intent(IntentEvent::new(format!("bench-{sequence}"))),
        );
        store.append(&event).expect("append");
    }
    SeededStore {
        _dir: dir,
        store,
        session_id,
        event_count: extra_intents + 1,
    }
}

fn append_next_batch(c: &mut Criterion) {
    let mut group = c.benchmark_group("event_log_append_next");
    group.warm_up_time(Duration::from_millis(500));
    group.measurement_time(Duration::from_secs(2));
    group.sample_size(15);

    // Keep N modest: each append_next re-lists full history.
    for n in [50u64, 100, 200] {
        group.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, &n| {
            b.iter_with_setup(
                || {
                    let (dir, store) = open_store();
                    let session_id = store.create_session().expect("create session");
                    (dir, store, session_id)
                },
                |(_dir, store, session_id)| {
                    for i in 0..n {
                        store
                            .append_next(
                                session_id,
                                EventPayload::Intent(IntentEvent::new(format!("a-{i}"))),
                            )
                            .expect("append_next");
                    }
                },
            );
        });
    }
    group.finish();
}

fn list_full_session(c: &mut Criterion) {
    let mut group = c.benchmark_group("event_log_list");
    group.warm_up_time(Duration::from_millis(500));
    group.measurement_time(Duration::from_secs(2));
    group.sample_size(20);

    for extra in [100u64, 1_000, 10_000] {
        let seeded = seed_events(extra);
        group.bench_with_input(
            BenchmarkId::from_parameter(seeded.event_count),
            &seeded,
            |b, seeded| {
                b.iter(|| {
                    let events = seeded.store.list(seeded.session_id).expect("list");
                    std::hint::black_box(events.len())
                });
            },
        );
    }
    group.finish();
}

fn cursor_backfill(c: &mut Criterion) {
    let mut group = c.benchmark_group("event_log_cursor_backfill");
    group.warm_up_time(Duration::from_millis(500));
    group.measurement_time(Duration::from_secs(2));
    group.sample_size(20);

    for extra in [100u64, 1_000, 10_000] {
        let seeded = seed_events(extra);
        let after_sequence = seeded.event_count / 2;
        group.bench_with_input(
            BenchmarkId::new("after_mid", seeded.event_count),
            &(seeded, after_sequence),
            |b, (seeded, after_sequence)| {
                b.iter(|| {
                    let events = seeded
                        .store
                        .list(seeded.session_id)
                        .expect("list")
                        .into_iter()
                        .filter(|event| event.sequence > *after_sequence)
                        .collect::<Vec<_>>();
                    std::hint::black_box(events.len())
                });
            },
        );
    }
    group.finish();
}

fn chunk_event(session_id: Uuid, sequence: u64, chunk_id: u64, text: String) -> Event {
    Event::new(
        session_id,
        sequence,
        EventPayload::Agent(AgentEvent::Chunk {
            run_id: Uuid::nil(),
            chunk_id,
            text,
            artifact: None,
        }),
    )
}

fn stream_events_frame(session_id: Uuid, events: Vec<Event>) -> String {
    serde_json::to_string(&IpcResponse::Events { session_id, events })
        .expect("encode Stream Events frame")
}

/// Durable Chunk write path at coalesce / mid / max inline sizes.
fn chunk_ingest(c: &mut Criterion) {
    let mut group = c.benchmark_group("event_log_chunk_ingest");
    group.warm_up_time(Duration::from_millis(500));
    group.measurement_time(Duration::from_secs(2));
    group.sample_size(15);

    // (label, text_len, batch count) — keep batch modest; append_next is hot.
    let cases: [(&str, usize, u64); 3] = [
        ("coalesce_512", AGENT_CHUNK_COALESCE_BYTES, 100),
        ("mid_4kib", 4 * 1024, 50),
        ("max_16kib", MAX_AGENT_CHUNK_EVENT_BYTES, 25),
    ];

    for (label, text_len, n) in cases {
        group.throughput(Throughput::Bytes((text_len as u64).saturating_mul(n)));
        group.bench_with_input(
            BenchmarkId::new(label, n),
            &(text_len, n),
            |b, &(text_len, n)| {
                let body = "x".repeat(text_len);
                b.iter_with_setup(
                    || {
                        let (dir, store) = open_store();
                        let session_id = store.create_session().expect("create session");
                        (dir, store, session_id, body.clone())
                    },
                    |(_dir, store, session_id, body)| {
                        for i in 0..n {
                            store
                                .append_next(
                                    session_id,
                                    EventPayload::Agent(AgentEvent::Chunk {
                                        run_id: Uuid::nil(),
                                        chunk_id: i,
                                        text: body.clone(),
                                        artifact: None,
                                    }),
                                )
                                .expect("append_next chunk");
                        }
                    },
                );
            },
        );
    }
    group.finish();
}

/// IPC Stream `Events` JSON line size vs `MAX_IPC_LINE_BYTES` (64 KiB).
///
/// One max Chunk event stays under the line cap. Raw unbounded encode of many
/// max chunks exceeds it — harness `trim_events_to_ipc_frame` paginates before write.
fn stream_frame_size(c: &mut Criterion) {
    let mut group = c.benchmark_group("event_log_stream_frame");
    group.warm_up_time(Duration::from_millis(300));
    group.measurement_time(Duration::from_secs(1));
    group.sample_size(20);

    let session_id = Uuid::nil();

    // (n_chunks, text_len) — sizes chosen to straddle the 64 KiB wire limit.
    let cases: [(u64, usize); 5] = [
        (1, MAX_AGENT_CHUNK_EVENT_BYTES),
        (3, MAX_AGENT_CHUNK_EVENT_BYTES),
        (4, MAX_AGENT_CHUNK_EVENT_BYTES),
        (8, AGENT_CHUNK_COALESCE_BYTES),
        (100, AGENT_CHUNK_COALESCE_BYTES),
    ];

    for (n, text_len) in cases {
        let body = "y".repeat(text_len);
        let events: Vec<Event> = (1..=n)
            .map(|i| chunk_event(session_id, i, i, body.clone()))
            .collect();
        let encoded = stream_events_frame(session_id, events.clone());
        let encoded_len = encoded.len();
        // Sanity: single max Chunk must fit; ≥4×16KiB text will not.
        if n == 1 && text_len == MAX_AGENT_CHUNK_EVENT_BYTES {
            assert!(
                encoded_len < MAX_IPC_LINE_BYTES,
                "single max Chunk frame {encoded_len} must stay under {MAX_IPC_LINE_BYTES}"
            );
        }
        if n >= 4 && text_len == MAX_AGENT_CHUNK_EVENT_BYTES {
            assert!(
                encoded_len > MAX_IPC_LINE_BYTES,
                "batch of {n}×{text_len}B chunks encodes to {encoded_len}, expected > {MAX_IPC_LINE_BYTES}"
            );
        }

        group.throughput(Throughput::Bytes(encoded_len as u64));
        group.bench_with_input(
            BenchmarkId::new(format!("{n}x{text_len}B"), encoded_len),
            &events,
            |b, events| {
                b.iter(|| {
                    let line = stream_events_frame(session_id, events.clone());
                    std::hint::black_box(line.len())
                });
            },
        );
    }
    group.finish();
}

criterion_group!(
    benches,
    append_next_batch,
    list_full_session,
    cursor_backfill,
    chunk_ingest,
    stream_frame_size
);
criterion_main!(benches);
