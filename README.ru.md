# Impetus

[English version](README.md)

![Impetus](docs/impetus-banner.png)

Impetus — local-first harness для coding-агентов на macOS. Он сохраняет сессии и события аудита, проводит каждое действие через явную policy и не зависит от конкретного terminal-клиента или GUI.

> Ранняя стадия. Ядро подходит для локальной разработки, но публичные интерфейсы и интеграции будут меняться.

## Зачем Impetus

Модель не должна получать доступ только потому, что она его запросила. Impetus отделяет durable runtime от клиентов и применяет единый путь принятия решения к каждому типизированному действию:

`Policy → Allow | Deny | Needs approval → Sandbox → Capability → Execution`

- **Долговечное состояние.** SQLite WAL хранит сессии, события, approvals и projections: отключение клиента не теряет состояние сессии.
- **Явный контроль.** У действия есть origin `user` или `agent`; действие агента не может само себе выдать approval.
- **Граница секретов.** Учётные данные остаются в macOS Keychain; в событиях, SQLite, логах и IPC — только ссылки или отредактированные данные.
- **Сменные клиенты.** Headless runtime, Unix-socket protocol, CLI и optional native client разделены по ответственности.

## Архитектура

```
Clients (CLI, Zap adapter, TUI, GPUI)
  │ typed IPC protocol
  ▼
Unix socket daemon
  │ versioned capability negotiation
  ▼
Harness
  │ per-session coordination (A3)
  ├─ trusted origin derivation (A2)
  ├─ policy → approval → sandbox
  ├─ admitted operation enforcement (A1)
  └─ capability → execution
  ▼
EventStore (SQLite WAL)
  └─ durable events, ordered projection
```

Подробности: [ARCHITECTURE.md](docs/ARCHITECTURE.md)

## Что есть сейчас

**Готово (v0.1–v0.6, A1–A3):**
- Headless daemon с versioned Unix-socket IPC
- Reference CLI (create/attach/stream/cancel sessions)
- Durable event store (SQLite WAL)
- Policy engine + approval system
- OpenAI-compatible provider streaming
- Keychain-backed credentials
- Bounded workspace read-only tools
- Controlled process/PTY execution (type-level admission enforcement)
- SSH profiles с host-key verification
- Per-session coordination (global lock удалён)
- Server-side origin derivation
- Deferred effect storage для approval continuation

**В работе (B1):**
- Typed client SDK (не raw IpcResponse enum matching)
- Event-driven push subscription (не poll loops)

**Запланировано:**
- Complete DTOs (attachment/diff/detail endpoints)
- Provider registry/metadata
- Durable budgets
- Real remote executor (SSH/SFTP/PTY/tmux)
- MVP UI (session management, search, notifications)

План с gate criteria: [ROADMAP.md](docs/ROADMAP.md)

## Быстрый старт

Требования: macOS, Rust `1.98.0`, Xcode Command Line Tools.

```zsh
task setup
task verify
```

Запусти harness:

```zsh
cargo run -p impetus
```

Создай сессию в другом terminal:

```zsh
cargo run -p impetus-cli -- create
```

Доступные команды: `cargo run -p impetus-cli -- --help`

## Интеграция с Zap

Zap не обязателен для Impetus и не владеет его policy, состоянием или секретами. Сейчас Impetus можно запускать в обычной вкладке Zap. Планируется выделенный adapter с structured blocks и typed approvals.

## Тесты

```zsh
cargo test --workspace  # 247 тестов
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
```

Integration tests покрывают:
- Process execution (12 тестов)
- Remote profiles (26 тестов: SSH, PTY, tmux, SFTP)
- Policy replay и fail-closed sandbox
- A1/A2/A3 regression gates

## Источники идей

Impetus использует отдельные идеи из нескольких проектов:

- [Zap](https://github.com/zerx-lab/zap): local-first terminal UX
- [DeepSeek Harness](https://github.com/deepseek-ai/deepseek-harness): capability seams, append-only traces
- [Agent Client Protocol](https://agentclientprotocol.com/): external agent adapters
- [Claude Code](https://code.claude.com/): explicit permission modes
- [GPUI-CE](https://github.com/gpui-ce/gpui-ce): optional native macOS client

Детали: [REFERENCES.md](docs/REFERENCES.md)

## Документация

- [Architecture](docs/ARCHITECTURE.md) — дизайн системы и компоненты
- [Roadmap](docs/ROADMAP.md) — поэтапный план с gate criteria
- [Current Architecture Audit](docs/current-architecture-audit.md) — status snapshot
- [Implementation History](docs/IMPLEMENTATION_HISTORY.md) — завершённые фазы (A1-A3, v0.6)
- [Agent Rules](AGENTS.md) — правила репо для coding-агентов

## Лицензия

[Apache-2.0](LICENSE)
