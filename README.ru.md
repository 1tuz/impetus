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
impetus   → пользовательский CLI / TUI (`impetus ui`)
impetusd  → local-first daemon (authoritative runtime)
```

`impetusd` владеет durable sessions, Event Log, SQLite, policy, execution и
credential references. Клиенты не владеют authoritative state.

**CURRENT.** `impetusd` + CLI `impetus` через Unix-socket IPC и `HarnessClient`,
provider registry foundation, экспериментальный Zap adapter. Также доступны:
`impetus doctor`, `impetus ui` (Ratatui TUI) и foundations Module Runtime.
Второй CLI `impetus-cli` остаётся поддерживаемым для своих workflow; `impetus` —
более полный surface (doctor, ui, skills, …). Dual CLI намеренно.

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
task daemon
# или напрямую:
cargo run -p impetusd
```

Во втором — client CLI:

```zsh
task client -- create
# или напрямую:
cargo run -p impetus -- create
cargo run -p impetus -- prompt <session-id> "Опиши этот репозиторий"
cargo run -p impetus -- stream <session-id>
```

## Planned distribution

Целевой distribution path: prebuilt CLI, checksums, curl installer,
clean-machine smoke и update/uninstall docs. Это roadmap, а не текущая команда
установки.

## Design stance

Impetus — не port/fork другого coding agent. Маленький trusted kernel
(events, artifacts, policy, approval, sandbox, executor) и replaceable layers.
См. [References](docs/reference/design-references.md).

## Документация

- [Docs map](docs/README.md) — guides, architecture, reference, archive.
- [Architecture](ARCHITECTURE.md) — capability matrix (code-backed).
- [TODO](TODO.md) — Now / Next / Later backlog.
- [Roadmap](docs/architecture/roadmap.md) — краткий priority narrative.
- [References](docs/reference/design-references.md) — protocols и libraries.
- [Getting started](docs/guides/getting-started.md) — source-checkout setup.
- [Development](docs/guides/development.md) — workspace checks и CI.

## Лицензия

[Apache-2.0](LICENSE).
