# Правила для coding-агентов

## Сначала прочитать

Перед изменением кода прочитай `README.md`, `docs/ARCHITECTURE.md`, `docs/ROADMAP.md`, `docs/GUI_UX.md` и `docs/ACP_AND_AUTH.md`. Затем назови один текущий этап roadmap и его критерии готовности. Не расширяй одновременно несколько этапов.

## Неподвижные границы

- macOS-first: Rust + GPUI-CE + Metal; не добавлять Electron, WebView, локальный HTTP UI или Node runtime.
- UI не владеет SQLite connection, секретами, PTY, SSH или policy. `agentic-terminal-core` не зависит от GPUI.
- Каждый typed action имеет `origin=user|agent` и проходит `Policy → Deny | Allow | NeedsApproval`; только `Allow` либо принятое человеком approval продолжаются через `Sandbox → Capability → Execution`. Модель не может выдать себе `origin=user` или approval.
- Секреты хранятся только в macOS Keychain. В SQLite, JSONL, tracing, Blocks и тестах — лишь reference-метки, никогда token/private key/passphrase.
- Не использовать `latest` и непинованные git dependency. `gpui` и `gpui_platform` обновляются только одним commit.

## ACP и модели

- ACP — протокол между клиентом и внешним coding-agent, а не универсальный provider API и не хранилище авторизации.
- Для ACP backend авторизация принадлежит выбранному agent CLI; приложение запускает его только после явного user action и отображает его профиль/статус.
- `agent-client-protocol = 2.x` означает major Rust SDK crate; draft protocol v2 feature не включать без отдельного RFC и compatibility tests.
- Для direct provider auth использовать ровно один из вариантов: Keychain API-key reference, system-browser OAuth или local/no-secret. Никакого поля пароля/токена в GPUI и никакой передачи секрета модели.
- URL-mode OAuth открывается только с подтверждением пользователя в системном браузере; URL виден целиком. Не использовать WebView.
- Поддерживаемость конкретного Codex/Claude/Cursor/Gemini/Qwen backend определяется установленной версией и ACP registry/discovery, не предположением о CLI-флаге.

## Проверка

После Rust-изменения обязательно выполнить:

```zsh
cargo fmt --all -- --check
cargo test -p agentic-terminal-core
cargo check --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

Для изменений ACP/auth добавить тест без секрета: profile validation, policy decision и redaction/export. Для GPUI сверить exact pinned API с исходником/официальным примером до кода.

`task verify` является коротким эквивалентом четырёх обязательных Rust-команд. `task setup` проверяет окружение и подключает repository-owned hooks.

## Git и коммиты

- Делить работу на атомарные коммиты по одной причине изменения; не смешивать tooling, продуктовый код и независимую документацию без необходимости.
- До commit выполнить `task verify`. Для docs-only изменения дополнительно проверить ссылки/диаграммы применимым локальным validator-ом.
- Subject обязателен в формате `KEY-123 type: Результат`; ключ соответствует задаче, разрешены `feat`, `fix`, `docs`, `refactor`, `perf`, `test`, `build`, `ci`, `chore`, `revert`.
- Русское описание начинается с заглавной буквы, формулирует получившийся результат, не инфинитив, не заканчивается точкой и вместе с prefix занимает не более 72 символов.
- Перед предложением или созданием commit subject прогнать `task commit:check MESSAGE='ATM-123 docs: Описана схема'` и глобальный `commit-subject-guard`.
- Не использовать `--no-verify`, не коммитить secrets, `.env`, локальные БД, provider credentials, browser caches, `target/` и generated runtime state.
- Не делать amend/rebase/force-push и не настраивать remote без прямого указания пользователя.
