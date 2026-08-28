# Orbit — Agentic Harness для macOS

Local-first Rust harness для coding-agents: долговечные сессии, typed events, provider/ACP adapters и контролируемые эффекты. Harness не зависит от конкретного terminal emulator или GUI. Основной пользовательский путь — запуск из [Zap](https://github.com/zerx-lab/zap); CLI, IDE adapter и существующий GPUI preview остаются сменными клиентами одной session model.

Репозиторий исторически называется `agentic-terminal`, поэтому имена crates и data directory пока сохранены. Смена стратегии не выдаёт запланированное за готовое: сейчас реализованы core-фундамент, SQLite event store, policy/approval, capability manifests, headless daemon/CLI с deterministic mock stream, workspace-scoped read-only tools, client contract и экспериментальный GPUI CI pane. Model-backed agent loop, provider profiles, ACP и local effects ещё предстоят.

## Принципы

- **Harness-first:** agent loop, sessions, tools, policy и audit строятся раньше собственного terminal UI.
- **Сменные клиенты:** Zap — основной клиент и допустимый личный fork; CLI уже использует versioned protocol, будущие IDE/structured GPUI surfaces обязаны использовать тот же contract и не владеть policy state.
- **macOS-first и local-first:** Rust runtime, SQLite WAL, Keychain references; удалённые подключения включаются только явным профилем.
- **Ограниченная RAM:** bounded channels/output, durable chunks и измеримый harness RSS; память конкретного terminal emulator считается отдельно.
- **Явный origin:** модель создаёт только `origin=agent`; действие проходит `Policy → Allow | NeedsApproval | Deny`, затем `Sandbox → Capability → Execution`.
- **Терминал не равен harness:** controlled process/PTY может быть capability, но ANSI renderer, tabs и scrollback UI не обязательны.

## Клиентская стратегия

1. v0.2: долгоживущий Harness, versioned Unix socket IPC и headless CLI запускаются из обычной Zap tab без модификации Zap.
2. v0.3: stable IPC расширяется typed approvals/diffs/attachments и ACP/backend states.
3. Для structured Blocks/diff/approval допускается adapter или личный fork Zap.
4. Собственный GPUI terminal продолжается только после зафиксированного требования, которое Zap/fork не закрывает.

## Что уже есть

| Слой | Текущее состояние |
| --- | --- |
| `orbit-core` | базовые events/runtime types, policy/approval, SQLite WAL, manifest validation и CI projection |
| GPUI reference client | native Metal window, темы preview и экспериментальный CI pane; durable session state не хранит |
| GitLab CI slice | общий local/remote `PipelineModel`; native smoke требует установленные `gitlab-ci-local` и `glab` |
| Standalone harness | v0.2: typed projections, supervisor, daemon/IPC, CLI, mock provider, bounded/redacted read-only tools и один OpenAI-compatible direct-provider adapter |
| Client IPC | Unix socket base входит в v0.2; structured extensions спроектированы для v0.3 |
| Zap bridge / ACP | v0.3: спроектированы, но не реализованы |
| Собственный PTY/ANSI terminal | optional backlog после Zap go/no-go |
| Local effects, SSH, tmux, SFTP | последующие этапы после policy/sandbox gates |

## Требования macOS

1. Rust `1.98.0`, зафиксированный в `rust-toolchain.toml`.
2. Для существующего GPUI reference client — Xcode и Command Line Tools с Metal.
3. Сеть нужна только для первого скачивания dependencies и явно выбранных provider/remote scopes.

Проверка окружения существующего workspace:

```zsh
./scripts/bootstrap-macos.sh
```

## Запуск и проверка

Headless daemon и CLI уже покрывают базовый session lifecycle. Запусти daemon в одной обычной terminal tab, затем CLI в другой:

```zsh
cargo fmt --all -- --check
cargo test --workspace
cargo check --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo run -p orbit
cargo run -p agentic-terminal-cli -- create
cargo run -p agentic-terminal-app
```

CLI умеет `create`, `list`, `attach SESSION_ID`, `stream SESSION_ID [AFTER_SEQUENCE]`, `prompt SESSION_ID TEXT`, `cancel SESSION_ID` и workspace-scoped `tool`. `stream` читает durable typed events после указанной sequence; SQLite connection клиенту не отдаётся. Wire contract `IPC_VERSION=2`: перед запросами клиент и daemon выполняют version/capability handshake. Пока `prompt` запускает deterministic mock provider: он записывает Agent chunks, переживает один имитированный restart и не использует модель, сеть или секреты.

Для явного local/no-secret OpenAI-compatible endpoint daemon принимает отдельный
profile JSON без token-полей и только из local loopback scope:

```json
{"id":"local","endpoint":"http://127.0.0.1:11434","model":"model-name","credential_strategy":{"kind":"none"}}
```

Запуск: `cargo run -p orbit -- --provider-profile /absolute/path/profile.json`.
Profile с неизвестными полями (включая raw token) отклоняется. HTTPS profile
может содержать только opaque Keychain `service`/`account` reference; daemon
читает generic-password item через macOS Security Framework только перед
provider request. Credential не сохраняется в SQLite/events/logs и не проходит
через client IPC. Отсутствующий или недоступный item завершает run redacted
ошибкой без деталей Keychain.

Повторяемые команды:

```zsh
task setup
task verify
task run       # существующий GPUI reference client
task --list
```

GitLab pipeline в `.gitlab-ci.yml` содержит stage `verify` (`fmt`, `test`, `check`, `clippy`) и stage `security` (`cargo-audit`, `cargo-deny`). Для проверки dependency policy отдельно есть `task security`. Для trusted local workspace: `task ci:list`, затем `task ci:local`; это использует shell executor с уже установленным Rust и не меняет container image pipeline.

Нужен [Task v3](https://taskfile.dev/docs/installation). Git subject проверяется repository-owned `commit-msg` hook и командой `task commit:check MESSAGE='ATM-123 docs: Описана схема'`.

## Документация

- [Архитектура: hierarchy, зависимости, ownership и границы](ARCHITECTURE.md)
- [Наглядная схема architecture](docs/architecture.html)
- [Roadmap harness-first](docs/ROADMAP.md)
- [Аудит второй итерации](docs/iteration-2-audit.md)
- [Историческая детализация gates v0.2](docs/iteration-2-roadmap.md)
- [Ближайший исполнимый TODO](TODO.md)
- [Клиентский UX: Zap, CLI и optional GPUI](docs/GUI_UX.md)
- [ACP, client IPC и auth](docs/ACP_AND_AUTH.md)
- [Safe Auto mode и threat model](docs/SAFE_AUTO_MODE.md)
- [Скриншоты, файлы и model context](docs/ATTACHMENTS.md)
- [GitLab CI experimental slice](docs/GITLAB_CI.md)
- [Baseline v0.1 GPUI preview](docs/benchmarks/v0.1.md)
- [Промт для coding-агента](docs/CODING_AGENT_PROMPT_RU.md)
- [Правила для coding-агентов](AGENTS.md)
- [Референсы и статус их использования](docs/REFERENCES.md)

## Структура

```text
crates/orbit-core/  события, policy, approvals, SQLite, manifests
crates/agentic-terminal-app/   optional GPUI reference client и CI preview
crates/orbit/ headless daemon и Unix socket IPC
crates/agentic-terminal-client/ transport-neutral client contract
crates/agentic-terminal-cli/   reference CLI для обычной terminal/Zap tab
config/capabilities.json       декларативный каталог capabilities
docs/                          архитектура, roadmap и client contracts
scripts/                       проверка/подготовка macOS окружения
```

Headless runtime и client protocol живут в отдельных crates, зависят от core и не зависят от `agentic-terminal-app`.

## GPUI reference client

`gpui` и `gpui_platform` зафиксированы на одном GPUI-CE commit `9949f8b2d27bb1d6dbc1efe90be039634cf1fb6b`. Их нельзя обновлять порознь. Этот pin относится к optional reference client и не должен попадать в dependency graph headless harness.

По умолчанию текущий event store находится в `~/Library/Application Support/Agentic Terminal/events.sqlite3`. `AGENTIC_TERMINAL_DATA_DIR` существует для изолированного smoke/test запуска. Секреты туда записывать запрещено.

## Лицензия

Apache-2.0, см. [LICENSE](LICENSE). Zap используется как отдельный клиент или личный fork; вопрос распространения производного продукта оценивается только если появится намерение его публиковать.
