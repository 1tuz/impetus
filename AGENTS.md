# Правила для coding-агентов

Стиль (caveman), YAGNI/ponytail, **RTK** и снижение токенов — в Codewhale constitution / `~/.codewhale/RTK.md`:

- глобально: `~/.codewhale/constitution.json` + `append_system_prompt` (RTK каждый shell)
- репо: `.codewhale/constitution.json`
- **каждый `bash`:** только через `rtk …` (`rtk cargo`, `rtk git`, `rtk rg`, …)

Здесь только продуктовые границы и проверка этого репо.

## Неподвижные границы

- Harness-first: текущий этап — standalone Rust runtime и CLI. Не начинать собственный PTY/ANSI terminal до Zap integration spike и зафиксированного неудовлетворённого требования.
- Zap — основной пользовательский terminal client; отдельный adapter или личный fork допустимы. Не копировать Zap/Warp client internals внутрь harness core.
- `agentic-terminal-core` и headless runtime не зависят от GPUI, Metal, terminal renderer или конкретного клиента. Существующий GPUI-CE app — optional reference client.
- Клиент не владеет SQLite connection, секретами, SSH transport или policy. Он отправляет typed request и отображает durable events/approvals harness-а.
- Каждый typed action имеет `origin=user|agent` и проходит `Policy → Deny | Allow | NeedsApproval`; только `Allow` либо принятое человеком approval продолжаются через `Sandbox → Capability → Execution`. Модель не может выдать себе `origin=user` или approval.
- Секреты хранятся только в macOS Keychain. В SQLite, JSONL, tracing, Blocks и тестах — лишь reference-метки, никогда token/private key/passphrase.
- Не использовать `latest` и непинованные git dependency. Если меняется optional GPUI client, `gpui` и `gpui_platform` обновляются только одним commit.

## Harness и клиентский протокол

- Controlled shell/process/PTY — capability исполнения. ANSI parser, tabs, scrollback и terminal renderer — клиентская функция; эти понятия не смешивать.
- Versioned local IPC обязан поддерживать capability negotiation, prompt/stream/status/cancel, typed approvals/diffs и явный `Incompatible` state.
- Disconnect или crash клиента не должен уничтожать durable session либо выдавать неизвестный outcome за `Completed`.
- Базовый Zap path — запуск headless CLI в обычной tab. Structured Zap integration строится отдельным adapter/fork; OSC/notification hooks не заменяют typed protocol.
- Local HTTP UI, Electron/WebView и Node runtime не добавлять в harness. Состав отдельного личного Zap fork не расширяет dependency/trust boundary harness-а.

## ACP и модели

- ACP — протокол между клиентом и внешним coding-agent, а не универсальный provider API и не хранилище авторизации.
- Для ACP backend авторизация принадлежит выбранному agent CLI; приложение запускает его только после явного user action и отображает его профиль/статус.
- `agent-client-protocol = 2.x` означает major Rust SDK crate; draft protocol v2 feature не включать без отдельного RFC и compatibility tests.
- Для direct provider auth использовать ровно один из вариантов: Keychain API-key reference, system-browser OAuth или local/no-secret. Никакого поля raw token в клиенте и никакой передачи секрета модели.
- URL-mode OAuth открывается только с подтверждением пользователя в системном браузере; URL виден целиком. Не использовать WebView.
- Поддерживаемость конкретного Codex/Claude/Cursor/Gemini/Qwen backend определяется установленной версией и ACP registry/discovery, не предположением о CLI-флаге.

## Проверка

После Rust-изменения обязательно выполнить:

```zsh
cargo fmt --all -- --check
cargo test --workspace
cargo check --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

Для изменений harness/provider/ACP/auth добавить тест без секрета: stream/cancel/restart, profile validation, policy decision и redaction/export. Для GPUI сверить exact pinned API с исходником/официальным примером до кода.

`task verify` является коротким эквивалентом четырёх обязательных Rust-команд. `task setup` проверяет окружение и подключает repository-owned hooks.

## GitLab CI

- `.gitlab-ci.yml`, когда он есть, — versioned contract проверки, а не UI-артефакт. При изменении Rust-пакетов, test/verify команд, toolchain, dependencies или CI-образа сверить затронутые jobs и актуализировать pipeline в том же изменении.
- До handoff Rust/CI-изменения выполнить `task verify`. Если в репозитории есть `.gitlab-ci.yml`, дополнительно выполнить `gitlab-ci-local --list-csv-all` и relevant local job либо весь pipeline; отсутствие Docker, runner image или другой внешний blocker сообщить как blocker, а не считать pipeline проверенным.
- Не помечать GitLab pipeline как проверенный, если `.gitlab-ci.yml` отсутствует. При создании pipeline включить обязательный `task verify`, закрепить CI image/tag и добавить job-specific smoke для изменяемого crate/contract.
- При изменении `Cargo.toml` или `Cargo.lock` выполнить `task security`; RustSec/CVE, license/source/bans findings не игнорировать без versioned записи в `deny.toml` с конкретной причиной.

## Git и коммиты

- Делить работу на атомарные коммиты по одной причине изменения; не смешивать tooling, продуктовый код и независимую документацию без необходимости.
- До commit выполнить `task verify`. Для Rust/CI-изменения при наличии `.gitlab-ci.yml` также выполнить `task ci:list` и relevant local job либо `task ci:local`; при изменении job/toolchain/dependency policy актуализировать pipeline в том же commit. Для docs-only изменения дополнительно проверить ссылки/диаграммы применимым локальным validator-ом.
- Subject обязателен в формате `KEY-123 type: Результат`; ключ соответствует задаче, разрешены `feat`, `fix`, `docs`, `refactor`, `perf`, `test`, `build`, `ci`, `chore`, `revert`.
- Русское описание начинается с заглавной буквы, формулирует получившийся результат, не инфинитив, не заканчивается точкой и вместе с prefix занимает не более 72 символов.
- Перед предложением или созданием commit subject прогнать `task commit:check MESSAGE='ATM-123 docs: Описана схема'` и глобальный `commit-subject-guard`.
- Не использовать `--no-verify`, не коммитить secrets, `.env`, локальные БД, provider credentials, browser caches, `target/` и generated runtime state.
- Не делать amend/rebase/force-push и не настраивать remote без прямого указания пользователя.
