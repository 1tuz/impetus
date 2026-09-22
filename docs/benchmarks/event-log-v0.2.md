# v0.2 baseline: SqliteEventStore event log queries

Date: 2026-09-22 (chunk ingest + Stream frame bounds; Stream batch trim under
`IPC_EVENTS_FRAME_BUDGET`; prior timing rows from 2026-09-21).

Serial numbers, hardware UUID, and other device identifiers are not recorded.

## Environment

- Apple Silicon MacBook Pro (`Mac17,8` family), Apple M5 Pro, 48 GB RAM.
- macOS 27.0 (`26A428`).
- Rust 1.98.0 (`88d9e12ae`).
- Criterion 0.8.2 via `task bench` / `cargo bench -p impetus-core --bench event_log`.
- Temporary on-disk SQLite WAL DB via `tempfile` (not tmpfs-only).

## How to reproduce

```zsh
task bench
# or:
cargo bench -p impetus-core --bench event_log
# compile / smoke only (no timing gate):
cargo bench -p impetus-core --bench event_log -- --test
```

Criterion history lives under `target/criterion/` (gitignored build artifact).
**Do not** fail PR CI on these numbers — disk/CPU noise makes hard gates flaky.
`task verify` does not run benches.

## Bounds vs 64 KiB IPC line

Line-delimited JSON IPC (`impetusd`, `impetus-client`, TUI) rejects a single
request/response line above **`MAX_IPC_LINE_BYTES = 64 KiB`**. Durable Chunk
spill and upload chunking keep **one** payload under that wire cap.
`Stream` / Subscribe `Events` batches are trimmed by
`trim_events_to_ipc_frame` to **`IPC_EVENTS_FRAME_BUDGET = 60 KiB`**; clients
resume with `after_sequence`.

| Constant | Value | Role vs 64 KiB line |
| --- | --- | --- |
| `MAX_IPC_LINE_BYTES` | 64 KiB | Hard read/write line cap (daemon + clients) |
| `IPC_EVENTS_FRAME_BUDGET` | 60 KiB | Soft cap for Stream/Subscribe `Events` JSON |
| `MAX_AGENT_CHUNK_EVENT_BYTES` | 16 KiB | Max inline `AgentEvent::Chunk` text; larger → DurableArtifactStore preview (`AGENT_CHUNK_PREVIEW_BYTES` = 256) + ArtifactRef |
| `AGENT_CHUNK_COALESCE_BYTES` | 512 | Flush threshold for tiny stream deltas (fewer event rows) |
| `MAX_ARTIFACT_UPLOAD_CHUNK_BYTES` | 24 KiB | Raw upload chunk before base64; keeps one upload IPC line under 64 KiB |

Measured **unbounded** `IpcResponse::Events` JSON sizes (raw encode cost in
`event_log_stream_frame`; harness now paginates before write):

| Frame | Encoded bytes | vs 64 KiB (raw) |
| --- | --- | --- |
| 1 × 16 KiB Chunk | ~16755 | under |
| 3 × 16 KiB Chunk | ~50083 | under |
| 4 × 16 KiB Chunk | ~66747 | over (would exceed without trim) |
| 8 × 512 B Chunk | ~6427 | under |
| 100 × 512 B Chunk | ~79475 | over (would exceed without trim) |

**EVT Stream line:** Implemented — `trim_events_to_ipc_frame` + daemon Subscribe
drain loop; resume via `after_sequence` / `list_after` cursor unchanged.

## Operations measured

| Bench group | API surface | Notes |
| --- | --- | --- |
| `event_log_append_next` | `EventStore::append_next` | Runtime write path; each call re-lists history to pick next sequence |
| `event_log_list` | `EventStore::list` | Full session materialization (fork ancestry included) |
| `event_log_cursor_backfill` | `list` + `sequence > after_sequence` | Same filter as IPC `Stream` / client reconnect cursor |
| `event_log_chunk_ingest` | `append_next` of `AgentEvent::Chunk` | Coalesce (512 B), mid (4 KiB), max inline (16 KiB) batches |
| `event_log_stream_frame` | `serde_json` of `IpcResponse::Events` | Frame byte size + encode cost; asserts 1×16 KiB under / 4×16 KiB over 64 KiB |

Seed for list/backfill uses `append` with known sequences so setup stays O(n).

## Results (median / Criterion mid estimate)

Times are wall-clock for the whole operation (batch append of N, or one list/backfill).

### append_next (batch of N after `create_session`)

| N | time (approx.) |
| --- | --- |
| 50 | ~8.5 ms |
| 100 | ~21 ms |
| 200 | ~35 ms |

Growth is worse than linear: `append_next` reads full history on every call.

### list (session size = Created + N intents)

| events | time (approx.) |
| --- | --- |
| 101 | ~41 µs |
| 1001 | ~388 µs |
| 10001 | ~3.8 ms |

### cursor backfill (`after_sequence = event_count / 2`)

| events | time (approx.) |
| --- | --- |
| 101 | ~48 µs |
| 1001 | ~457 µs |
| 10001 | ~4.0 ms |

Backfill cost tracks `list` today: filter runs in memory after full materialization.
A future ranged SQL/`sequence > ?` API would cut reconnect cost without changing
the public cursor model.

### chunk ingest (`AgentEvent::Chunk` via `append_next`)

Throughput group `event_log_chunk_ingest`. Smoke-checked with `-- --test`
(coalesce×100, mid×50, max×25). Re-run `task bench` locally for median timings;
disk noise dominates absolute numbers — use Criterion history for regressions.

### Stream frame encode

Group `event_log_stream_frame` black-boxes `serde_json::to_string` of
`IpcResponse::Events`. Absolute encode time is µs-scale; the important signal is
the encoded **byte** id (see bounds table above).

## Query plan / indexes

Existing unique index:

```sql
CREATE UNIQUE INDEX events_session_sequence_unique ON events(session_id, sequence);
```

`EXPLAIN QUERY PLAN` (SQLite) for the list shapes:

- `WHERE session_id = ? ORDER BY sequence` →
  `SEARCH … USING INDEX events_session_sequence_unique (session_id=?)`
- `WHERE session_id = ? AND sequence <= ? ORDER BY sequence` →
  `SEARCH … USING INDEX events_session_sequence_unique (session_id=? AND sequence<?)`

No additional index added in #16: the covering unique index already serves list and
ancestor prefix scans. Done since: `append_next` uses COUNT head (no payload re-list);
`list_after(session, after, limit)` uses `sequence > after LIMIT` on root sessions.

## Interpretation

- List/backfill at 10k events stay in low-ms range on this machine — acceptable for
  reconnect/stream backfill at current scale.
- Write-path bottleneck for bulk ingest was `append_next` history re-scan; now
  COUNT head + indexed insert. Inline Chunk body is capped at 16 KiB (+ artifact spill).
- Stream `Events` batches are trimmed to `IPC_EVENTS_FRAME_BUDGET` (60 KiB) so
  wire lines stay under `MAX_IPC_LINE_BYTES`; resume with `after_sequence`.
- Local Criterion comparison (`cargo bench` vs prior `target/criterion` runs) is the
  regression signal; PR CI stays timing-free.

## Related

- Issue: https://github.com/1tuz/impetus/issues/16
- Bench source: `crates/impetus-core/benches/event_log.rs`
- Historical idle RSS snapshot: [benchmarks-v0.1-gpui-preview.md](../archive/benchmarks-v0.1-gpui-preview.md)
