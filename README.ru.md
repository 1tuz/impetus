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

**CURRENT.** `impetusd` + CLI `impetus` через versioned Unix-socket IPC и
`HarnessClient`, provider registry foundation, экспериментальный Zap adapter.
Также доступны: `impetus doctor`, `impetus ui` (Ratatui TUI) и foundations
Module Runtime. Primary CLI — `impetus`; `impetus-cli` — legacy/secondary
surface (migration note, не deletion).

**TARGET.** Модульный harness: `impetus` — first-class CLI/TUI; Zap — ещё один
`HarnessClient` consumer. Честный adapter checklist: [Architecture — Zap path
(#5)](ARCHITECTURE.md#zap-path-vs-standalone-clitui-5). См.
[Architecture](ARCHITECTURE.md).

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
  Production MCP: `impetusd` autoload `$IMPETUS_DATA_DIR/mcp/*.json` в
  `ToolProviderRuntime` (fail-closed на bad config; live connect на first tool
  use). Read-only catalog IPC: `ListMcpServers` / `ListModels` (labels/status;
  no secrets). Pickers UI ещё polish — см. [TODO.md](TODO.md).
- Daemon-owned PTY (`portable-pty`, IPC v12): owner-session binding, cwd
  containment; Agent origin — Seatbelt на macOS; TUI passthrough
  (`Ctrl+\` / `/pty`).
- Production model path: Mock или native OpenAI Chat Completions SSE
  (`--provider-profile`); Anthropic library есть, но не default daemon path.
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

Daemon:

```zsh
impetusd
```

В другом terminal:

```zsh
impetus create
impetus prompt <session-id> "Опиши этот репозиторий"
impetus stream <session-id>
# Когда stream показывает pending approval:
impetus approve <session-id> <approval-id>
# Или reject — модель продолжит с denial observation:
impetus approve <session-id> <approval-id> --reject
```

Конфиг провайдера: [configuration docs](docs/guides/configuration.md).

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
