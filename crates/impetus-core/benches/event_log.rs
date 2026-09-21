//! Event log baselines for SqliteEventStore hot paths.
//!
//! Local-only: numbers move with disk/CPU noise. Do not gate PR CI on timings.
//!
//! Ops covered:
//! - `append_next` (runtime write path; re-lists history per call)
//! - `list` (full session / range materialization)
//! - cursor backfill (`list` + `sequence > after_sequence`, same filter as IPC Stream)

use std::time::Duration;

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use impetus_core::{Event, EventPayload, EventStore, IntentEvent, SqliteEventStore};
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

criterion_group!(
    benches,
    append_next_batch,
    list_full_session,
    cursor_backfill
);
criterion_main!(benches);
