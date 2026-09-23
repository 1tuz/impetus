# Impetus

> **Local AI-agent runtime/harness:** маленький Trusted Kernel + заменяемые
> слои вокруг него. Terminal-first, local-first, Rust.

[English version](README.md)

**Канон (EN):** [README.md](README.md) + [ARCHITECTURE.md](ARCHITECTURE.md).
RU-страница — краткий обзор; при расхождении побеждает EN.

Impetus — local agent harness: durable сессии, orchestration, safety,
credentials и execution authority в `impetusd`. Клиенты — typed IPC, не
владеют SQLite / policy / secrets.

**`impetus-core` ≠ Trusted Kernel.** Crate шире (kernel + runtime libs).
**`impetusd`** — process boundary; обычный `impetus` **lazy-start** (не
ручной daemon).

**Без root / sudo / password в нормальном режиме.** Userspace под `$HOME`
(или `IMPETUS_DATA_DIR`). Seatbelt = `sandbox-exec`. Keychain silent.

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

**CURRENT.** `impetusd` + CLI `impetus` через versioned Unix-socket IPC и
`HarnessClient`; клиенты: CLI/TUI, Desktop, ACP, экспериментальный Zap.
Также: `impetus doctor`, `impetus ui` (Ratatui), Extension host / package SDK
(детали — EN). Primary CLI — `impetus`; `impetus-cli` — legacy/secondary
surface (migration note, не deletion).

**TARGET.** Модульный harness: `impetus` — first-class CLI/TUI; Zap — ещё один
`HarnessClient` consumer. Честный adapter checklist: [Architecture — Zap path
(#5)](ARCHITECTURE.md#zap-path-vs-standalone-clitui-5). См.
[Architecture](ARCHITECTURE.md).

Диаграммы (канон EN): [system-architecture.svg](assets/readme/system-architecture.svg),
[execution-flow.svg](assets/readme/execution-flow.svg).

## Что работает сейчас

Честный статус (детали: [ARCHITECTURE.md](ARCHITECTURE.md)):

- Durable sessions и упорядоченные audit events в SQLite WAL.
- Versioned local Unix-socket negotiation перед действиями клиента.
- Typed actions через policy, approval, **path-scope** sandbox, capability и
  execution (fail-closed). На macOS process spawn также обернут Seatbelt
  (`sandbox-exec`); non-macOS — только path-scope.
- Keychain references или local no-secret provider endpoint; profiles не
  хранят raw tokens.
- Typed Rust client transport, CLI, TUI (`impetus ui`), ACP gateway library и
  экспериментальный Zap adapter.
- Agent-loop vertical: filesystem reads + approval-gated writes/shell; большие
  tool/web/paste bodies — durable content-addressed artifacts
  (`DurableArtifactStore`); approval diffs — ephemeral in-memory attachments.
- Context HOT/WARM/COLD, lazy tool/instruction descriptions, session
  shared-prefix fork и checkpoints.
- Extension **import** adapters (Skills, MCP, Claude/Codex/Cursor layouts).
  Lifecycle CLI keep (`impetus extension plan|install|…`); marketplace нет.
  Production MCP SoT: `impetusd` autoload **только** `$IMPETUS_DATA_DIR/mcp/*.json`
  + live `ReloadMcpServers`; `ListMcpServers` / `ListModels` IPC
  (`connected=false` до first tool use). Explore + Workflow Explore — один
  AgentLoop bridge. MemoryStore control-plane IPC — **Implemented** + AgentLoop
  project-scope context inject on Prompt/FollowUp/ResolveApproval resume
  (approval-resume). Browser daemon health/negotiate — **Partial** (honest
  Absent; CDP Parked).
- Daemon-owned PTY (`portable-pty`, IPC v12): owner-session binding, cwd
  containment; Agent Seatbelt на macOS; optional Sqlite metadata; live PTY не
  restart-durable; TUI passthrough (`Ctrl+\` / `/pty`).
- Session model IPC (`ListProviders` / Get/SetSessionModel) + OpenAI Chat
  Completions SSE default (`--provider-profile`); Anthropic не default path.
  JSON Schema tool-arg validation — до policy на builtins.

## Установка

Landing: [1tuz.github.io/impetus](https://1tuz.github.io/impetus/).

### Быстрая установка

Платформы: macOS Apple Silicon, Linux x86_64

```zsh
curl -fsSL https://raw.githubusercontent.com/1tuz/impetus/main/scripts/install.sh | zsh
```

Бинарники → `~/.local/bin`. Добавь в PATH:

```zsh
export PATH="$HOME/.local/bin:$PATH"
```

### Из исходников

```zsh
git clone https://github.com/1tuz/impetus.git
cd impetus
task setup
task verify
cargo build --release -p impetus -p impetusd
```

## Использование

Обычный UX — **без ручного `impetusd`** (CLI сам поднимает daemon):

```zsh
impetus create
impetus prompt <session-id> "Опиши этот репозиторий"
impetus stream <session-id>
# Когда stream показывает pending approval:
impetus approve <session-id> <approval-id>
# Или reject — модель продолжит с denial observation:
impetus approve <session-id> <approval-id> --reject
```

TUI: `impetus ui`. Ручной `impetusd` — только debug / admin-профили.

Конфиг провайдера: [configuration docs](docs/guides/configuration.md).

Архитектура (EN SoT): [README.md](README.md) · [ARCHITECTURE.md](ARCHITECTURE.md).

## Удаление

Бинарники:

```zsh
rm -f ~/.local/bin/impetus ~/.local/bin/impetusd
```

Данные и сессии:

```zsh
rm -rf ~/Library/Application\ Support/Impetus  # macOS
```

Credentials из macOS Keychain — через **Keychain Access.app** или
`security delete-generic-password`.

Подробнее: [getting started](docs/guides/getting-started.md#uninstall).

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
