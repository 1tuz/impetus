# Agentic Terminal для macOS

Нативный терминал на Rust, в котором обычные shell-команды и задачи, написанные человеческим языком, живут в одном окне. Источник действия всегда явный: команда человека идёт в уже открытый им PTY, а предложение агента проходит `Policy → Allow | NeedsApproval | Deny`; только `Allow` или подтверждённый `NeedsApproval` продолжаются через `Sandbox → Capability → Execution`. Решения и эффекты записываются в локальную историю.

Это небольшой, проверяемый каркас текущего этапа v0.1. Он запускает окно GPUI-CE, использует SQLite WAL для event store и содержит policy/approval и валидируемый каталог будущих capabilities. Настоящий PTY, LLM, SSH, tmux и SFTP появляются только на следующих последовательных этапах.

Будущий [Safe Auto mode](docs/SAFE_AUTO_MODE.md) не является bypass: hard-deny и human-only действия не разрешает классификатор, а любой сбой reviewer закрывается блокировкой. [Скриншоты и файлы](docs/ATTACHMENTS.md) будут передаваться только после preview, outbound policy и capability negotiation; сырые bytes не хранятся в event log.

## Принципы

- **macOS-first:** Rust + GPUI-CE + Metal. Нет Electron, Chromium, Tauri/WebView и локального web-интерфейса.
- **local-first:** история сессий и настройки лежат на Mac; удалённые подключения включаются человеком через профиль.
- **Ограниченная RAM:** отображаются только видимые Blocks/строки; PTY-вывод будет храниться чанками на диске с маленьким горячим окном в памяти.
- **Терминал без агента:** прямой ввод команды не проходит через модель и работает без сетевого провайдера.
- **Безопасность по умолчанию:** модель может предложить действие, но не может сама его одобрить.

## Что уже есть

| Слой | Состояние v0.1 |
| --- | --- |
| Нативное окно GPUI-CE | минимально реализовано |
| События / runtime | реализованы базовые типы; resume и execution lifecycle ещё не входят в v0.1 |
| SQLite | WAL event store подключён к приложению; reopen покрыт тестом |
| Policy / approval | различает user/agent origin, проверяет file scope и покрыт unit-тестами |
| Capability manifests | каталог валидируется; все пять implementation помечены `planned` |
| PTY, ANSI, LLM, SSH, tmux, SFTP | спроектированы, но сознательно отложены |

## Требования macOS

1. Xcode и Command Line Tools: GPUI-CE использует Metal.
2. Rust `1.98.0`: фиксируется в `rust-toolchain.toml`.
3. Сеть нужна только для первого скачивания Cargo-зависимостей и для явно одобренных remote capabilities.

Быстрая проверка окружения:

```zsh
./scripts/bootstrap-macos.sh
```

## Запуск и проверка

```zsh
cargo fmt --all -- --check
cargo test -p agentic-terminal-core
cargo check --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo run -p agentic-terminal-app
```

Повторяемые команды собраны в [Taskfile.yml](Taskfile.yml):

```zsh
task setup     # Xcode/Metal/Rust и Git hooks
task verify    # fmt, core tests, workspace check, clippy
task run       # native GPUI приложение
task --list    # все доступные задачи
```

Нужен [Task v3](https://taskfile.dev/docs/installation); на macOS официальный способ — `brew install go-task`. Git subject проверяется repository-owned `commit-msg` hook и командой `task commit:check MESSAGE='ATM-123 docs: Описана схема'`.

## Документация

- [Архитектура и границы ответственности](docs/ARCHITECTURE.md)
- [Наглядная схема архитектуры](docs/architecture.html)
- [Детальная схема Safe Auto](docs/safe-auto-architecture.html)
- [GUI/UX-спецификация](docs/GUI_UX.md)
- [ACP, Auth Center и внешние coding-agents](docs/ACP_AND_AUTH.md)
- [Safe Auto mode и threat model](docs/SAFE_AUTO_MODE.md)
- [Скриншоты, файлы и model context](docs/ATTACHMENTS.md)
- [Roadmap v0.1–v0.7 и критерии готовности](docs/ROADMAP.md)
- [Измерения и ограничения v0.1](docs/benchmarks/v0.1.md)
- [Ближайший исполнимый список задач](TODO.md)
- [Промт для coding-агента](docs/CODING_AGENT_PROMPT_RU.md)
- [Правила для coding-агентов](AGENTS.md)
- [Референсы и статус их использования](docs/REFERENCES.md)

## Структура

```text
crates/agentic-terminal-app/   GPUI-CE представление и macOS процесс
crates/agentic-terminal-core/  события, policy, approvals, SQLite, manifests
config/capabilities.json       декларативный каталог встроенных capabilities
docs/                          русская архитектурная и продуктовая документация
scripts/                       проверка/подготовка macOS окружения
```

## Версии UI-стека

`gpui` и `gpui_platform` зафиксированы на одном GPUI-CE commit `9949f8b2d27bb1d6dbc1efe90be039634cf1fb6b`. Их нельзя обновлять порознь: смешивание опубликованного UI-crate и git-платформы создаёт несовместимые типы. Для разработки включён `runtime_shaders`; перед релизом нужно сравнить старт приложения с предкомпилированными shader-ами.

По умолчанию event store находится в `~/Library/Application Support/Agentic Terminal/events.sqlite3`. Для изолированного smoke-теста путь можно переопределить через `AGENTIC_TERMINAL_DATA_DIR`; секреты туда записывать запрещено.

## Лицензия

Apache-2.0, см. [LICENSE](LICENSE). Перед публичной публикацией проверь владельца copyright и сторонние notices.
