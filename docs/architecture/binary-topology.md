# Binary topology

Target roles of the two Rust binaries in this repository.

## `impetusd` — daemon

- Owns `SqliteEventStore` (durable event log, SQLite WAL).
- Listens on Unix Domain Socket (`IMPETUS_SOCKET`, defaults to
  `~/Library/Application Support/Impetus/harness.sock`).
- Owns policy engine, sandbox, secrets (macOS Keychain), ProviderRegistry.
- Launch: `cargo run -p impetusd` (`task harness`).

## `impetus` — client

- CLI / TUI on top of `impetus-client::HarnessClient` (Unix socket transport).
- Does not open SQLite, does not store secrets, does not run sandbox directly —
  only typed IPC requests to `impetusd`.
- Connects to the same `IMPETUS_SOCKET` as daemon.
- Launch: `cargo run -p impetus -- <subcommand>`.
- Includes `impetus doctor` / `impetus doctor --json`, `impetus ui` (Ratatui),
  and `impetus components` (static built-in tool catalog; not a live module
  registry browser).

## Dual CLI

`impetus` — **primary** user-facing CLI/TUI (doctor, ui, skills, extension, …).

`impetus-cli` — earlier reference CLI before role separation (`ATM-001`);
**legacy/secondary**. Both crates remain in tree: migrate callers toward
`impetus` over time; do **not** delete `impetus-cli`. Dual CLI is intentional
during migration.

## Status

- Release artifact ships both binaries with explicit roles in install script help.
- `impetus` auto-discovers the socket and can safely spawn `impetusd` when needed
  (see [getting-started](../guides/getting-started.md) /
  [troubleshooting](../guides/troubleshooting.md)).
