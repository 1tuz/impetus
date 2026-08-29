# Impetus

> **Ультралёгкий all-in-one terminal-first, local-first Agent Harness for Engineering, написанный на Rust.**

[English version](README.md)

Impetus — ультралёгкий all-in-one local agent harness на Rust: durable сессии, model/tool
orchestration, safety decisions, credentials и execution authority собраны в
одном runtime за заменяемыми terminal и remote clients. Клиенты передают typed
requests и показывают durable events; они не владеют SQLite, policy, model/tool
runtime, credentials или authoritative session state.

## Что это и зачем

Engineering agent не должен делать terminal UI, provider или клиентское
приложение источником истины. Единственный authoritative owner durable
runtime/state — harness.

## CURRENT и TARGET

**Модель.**

```text
impetus   → пользовательский CLI (будущий TUI)
impetusd  → local-first daemon (authoritative runtime)
```

`impetusd` владеет durable sessions, Event Log, SQLite, policy, execution и
credential references. Клиенты не владеют authoritative state.

**CURRENT.** `impetusd` + CLI `impetus` через Unix-socket IPC и `HarnessClient`,
provider registry foundation, экспериментальный Zap adapter. TUI, `doctor`, Module
Runtime — не реализованы.

**TARGET.** Модульный harness: `impetus` — first-class CLI/TUI; Zap — ещё один
`HarnessClient` consumer. См. [Architecture](ARCHITECTURE.md).

## Что работает сейчас

- Durable sessions и упорядоченные audit events в SQLite WAL.
- Versioned Unix-socket negotiation.
- Путь typed action: Policy → Approval → Sandbox → Capability → Execution.
- macOS Keychain reference или local no-secret provider; raw token не хранится.
- Typed Rust client transport, reference CLI, ACP gateway library и
  экспериментальный Zap integration baseline.

## Текущая разработка

Пока доступен только developer checkout, без готового installer или prebuilt
binaries.

```zsh
git clone https://github.com/1tuz/impetus.git
cd impetus
task setup
task verify
```

В первом terminal — daemon:

```zsh
cargo run -p impetusd
```

Во втором — client CLI:

```zsh
cargo run -p impetus -- create
cargo run -p impetus -- prompt <session-id> "Опиши этот репозиторий"
cargo run -p impetus -- stream <session-id>
```

## Planned distribution

Целевой distribution path: prebuilt CLI, checksums, curl installer,
clean-machine smoke и update/uninstall docs. Это roadmap, а не текущая команда
установки.

## Design lineage

Impetus не является port/fork одного coding agent. Он собирает отдельные
проверенные механизмы Codex, Claude Code, OpenClaude, jcode, DeepSeek Harness,
Qwen Code, Pi, OpenCode, Aider, Kimi Code и RTK в собственной local-first Rust
architecture. См. [References](docs/REFERENCES.md).

## Документация

- [Architecture](ARCHITECTURE.md) — canonical CURRENT/TARGET architecture.
- [Roadmap](docs/ROADMAP.md) — реализованные foundations и planned gates.
- [References](docs/REFERENCES.md) — design lineage, protocols и libraries.
- [Getting started](docs/getting-started.md) — source-checkout setup.
- [Development](docs/development.md) — workspace checks и CI.
- [Implementation history](docs/IMPLEMENTATION_HISTORY.md) — historical record,
  не current architecture.

## Лицензия

[Apache-2.0](LICENSE).
