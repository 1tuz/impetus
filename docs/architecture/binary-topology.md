# Binary topology

Целевые роли двух Rust-бинарников в этом репозитории.

## `impetusd` — daemon

- Владеет `SqliteEventStore` (durable event log, SQLite WAL).
- Слушает Unix Domain Socket (`IMPETUS_SOCKET`, по умолчанию
  `~/Library/Application Support/Impetus/harness.sock`).
- Владеет policy engine, sandbox, secrets (macOS Keychain), ProviderRegistry.
- Запуск: `cargo run -p impetusd` (`task harness`).

## `impetus` — client

- CLI поверх `impetus-client::HarnessClient` (Unix socket transport).
- Не открывает SQLite, не хранит секреты, не запускает sandbox напрямую —
  только typed IPC-запросы к `impetusd`.
- Подключается к тому же `IMPETUS_SOCKET`, что и daemon.
- Запуск: `cargo run -p impetus -- <subcommand>` (`task cli -- <subcommand>`).

## Устаревшее

`impetus-cli` — предыдущее имя reference CLI до разделения ролей
(`ATM-001`). В user-facing docs и `Taskfile.yml` заменено на `impetus`.
Crate остаётся в workspace до отдельного решения об удалении/переносе.

## Открыто (Phase 1, не реализовано)

- Release artifact с обоими binary и явными ролями в install script help.
- `impetus` auto-discovery сокета и safe spawn `impetusd` при отсутствии.
- `impetus doctor` / `impetus doctor --json`.
- `impetus components list` / `impetus components status`.
