# Binary topology

Target roles of the two Rust binaries in this repository.

## `impetusd` — daemon

- Owns `SqliteEventStore` (durable event log, SQLite WAL).
- Listens on Unix Domain Socket (`IMPETUS_SOCKET`, defaults to
  `~/Library/Application Support/Impetus/harness.sock`).
- Owns policy engine, sandbox, secrets (macOS Keychain), ProviderRegistry.
- Launch: `cargo run -p impetusd` (`task harness`).

## `impetus` — client

- CLI on top of `impetus-client::HarnessClient` (Unix socket transport).
- Does not open SQLite, does not store secrets, does not run sandbox directly —
  only typed IPC requests to `impetusd`.
- Connects to the same `IMPETUS_SOCKET` as daemon.
- Launch: `cargo run -p impetus -- <subcommand>` (`task cli -- <subcommand>`).

## Deprecated

`impetus-cli` — previous name of reference CLI before role separation
(`ATM-001`). Replaced with `impetus` in user-facing docs and `Taskfile.yml`.
Crate remains in workspace until separate decision on removal/migration.

## Open (Phase 1, not implemented)

- Release artifact with both binaries and explicit roles in install script help.
- `impetus` auto-discovery of socket and safe spawn of `impetusd` if absent.
- `impetus doctor` / `impetus doctor --json`.
- `impetus components list` / `impetus components status`.
