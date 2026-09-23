# Binary topology

Target roles of the two Rust binaries in this repository.
Canonical layer model: [ARCHITECTURE.md](../../ARCHITECTURE.md),
[reader-guide.md](reader-guide.md).

## `impetus` — client (primary UX)

- CLI / TUI on top of `impetus-client::HarnessClient` (Unix socket transport).
- **Lazy-starts** `impetusd` when the socket is down (`daemon::ensure_daemon_running`).
- Does not open SQLite, does not store secrets, does not run sandbox directly —
  only typed IPC requests to `impetusd`.
- Launch: `cargo run -p impetus -- <subcommand>` or installed `impetus …`.
- Includes `impetus doctor`, `impetus ui`, and `impetus components` (static catalog).

## `impetusd` — daemon (process boundary)

- Owns `SqliteEventStore` (durable event log, SQLite WAL).
- Listens on Unix Domain Socket (`IMPETUS_SOCKET`, else `$data_root/harness.sock`).
- Owns policy engine, sandbox, secrets (macOS Keychain), ProviderRegistry,
  runtime services + Trusted Kernel hosted via `impetus-core`.
- Ordinary users should not need to launch this manually; use for development,
  debugging, and advanced `--provider-profile` / `--acp-profile` / `--policy-config`.
- Launch: `cargo run -p impetusd` (`task daemon`).

Default data root:

- macOS: `~/Library/Application Support/Impetus`
- Linux: `$XDG_DATA_HOME/impetus` or `~/.local/share/impetus`
- Override: `IMPETUS_DATA_DIR`

## Dual CLI

`impetus` — **primary** user-facing CLI/TUI.

`impetus-cli` — legacy/secondary reference CLI. Migrate callers toward `impetus`;
do **not** delete `impetus-cli`.
