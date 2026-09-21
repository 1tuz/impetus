# v0.2 baseline: SqliteEventStore event log queries

Date: 2026-09-21.

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
```

Criterion history lives under `target/criterion/` (gitignored build artifact).
**Do not** fail PR CI on these numbers — disk/CPU noise makes hard gates flaky.
`task verify` does not run benches.

## Operations measured

| Bench group | API surface | Notes |
| --- | --- | --- |
| `event_log_append_next` | `EventStore::append_next` | Runtime write path; each call re-lists history to pick next sequence |
| `event_log_list` | `EventStore::list` | Full session materialization (fork ancestry included) |
| `event_log_cursor_backfill` | `list` + `sequence > after_sequence` | Same filter as IPC `Stream` / client reconnect cursor |

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
ancestor prefix scans. Optimization backlog (not done here): avoid re-list inside
`append_next`; optional SQL-side `sequence > after` for cursor backfill.

## Interpretation

- List/backfill at 10k events stay in low-ms range on this machine — acceptable for
  reconnect/stream backfill at current scale.
- Write-path bottleneck for bulk ingest is `append_next` history re-scan, not the
  `(session_id, sequence)` index.
- Local Criterion comparison (`cargo bench` vs prior `target/criterion` runs) is the
  regression signal; PR CI stays timing-free.

## Related

- Issue: https://github.com/1tuz/impetus/issues/16
- Bench source: `crates/impetus-core/benches/event_log.rs`
- Historical idle RSS snapshot: [benchmarks-v0.1-gpui-preview.md](../archive/benchmarks-v0.1-gpui-preview.md)
