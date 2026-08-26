# TODO — Agentic Terminal для macOS

## Сейчас: фундамент v0.1

- [x] Rust workspace: `app` и независимый `core`.
- [x] GPUI-CE native window на macOS.
- [x] Rust `1.98.0` в `rust-toolchain.toml`.
- [x] SQLite WAL event store, policy, approval и capability manifests.
- [x] Русские README, архитектура, GUI/UX, референсы и `AGENTS.md`.
- [x] Схема архитектуры в `docs/architecture.html` и PNG preview.
- [x] Проверка Xcode/Metal/Rust в `scripts/bootstrap-macos.sh`.
- [x] Добавить SQLite reopen test и записать первый RSS baseline в `docs/benchmarks/v0.1.md`.
- [ ] Выполнить воспроизводимый native-window smoke на чистом Mac; локальный Apple Silicon запуск уже подтверждён.

## Далее: v0.2 — настоящий local terminal

- [x] Добавить встроенный набор переключаемых терминальных тем: Dracula, One Dark, Nord, Tokyo Night, Gruvbox Dark, Catppuccin Mocha, Solarized Light, One Light, GitHub Light и Catppuccin Latte.
- [ ] Исследовать и зафиксировать `portable-pty` + terminal parser (`alacritty_terminal`): версия, API, лицензия, небольшой spike.
- [ ] Реализовать PTY capability: zsh, process lifecycle, resize, Ctrl-C, корректный tab close.
- [ ] Сделать bounded disk-backed scrollback и горячее окно не более 8 MiB на tab.
- [ ] Добавить терминальные tabs, focus, selection/copy и terminal Block projection.
- [ ] Прогнать Unicode / 24-bit color / resize / long-output soak тесты.

## Затем: v0.3 — агент, ACP и модели

- [ ] Добавить `agentic-terminal-acp` на официальном Rust SDK `agent-client-protocol = 2`.
- [ ] Первый внешний ACP backend: initialize, session, text stream, cancel, exit/reconnect, Blocks.
- [ ] Добавить mock ACP agent для CI без модели и без секрета.
- [ ] Создать Auth Center UI: agent-owned CLI, Keychain API key, system-browser OAuth, local model.
- [ ] Реализовать Keychain adapter; в SQLite хранить только opaque reference.
- [ ] Первый direct OpenAI-compatible adapter: streaming + cancellation + DeepSeek/OpenRouter/custom endpoint profile.
- [ ] Подключить Codex / Claude / Cursor / Gemini / Qwen только через registry/discovery и установленный CLI; не хардкодить несуществующие флаги.
- [ ] Проверить, что ACP permission/tool request всегда проходит `Policy → Approval → Sandbox`.
- [ ] Добавить `Manual | Safe Auto` state, mock safety reviewer и fail-closed typed verdict без host effects.
- [ ] Добавить attachment refs, composer preview и ACP `image` / `embeddedContext` negotiation без bytes в events.

## v0.4 — длинные сессии

- [ ] Token-budget context builder.
- [ ] Versioned compaction: source event range, prompt/version, summary и восстановление.
- [ ] Resume после restart.
- [ ] Fork с immutable parent prefix и видимым fork point.
- [ ] Метрики: RSS, queued events, hot terminal bytes, Blocks, compaction ratio.

## v0.5 — локальные эффекты и capabilities

- [ ] Approval card с точным diff, command и target.
- [ ] Workspace sandbox с time/resource limits.
- [ ] Capability host: manifest/version/permission validation.
- [ ] Внешние capabilities только out-of-process по versioned IPC.
- [ ] Включить Safe Auto enforcement: hard-deny, human-only, cache invalidation, block thresholds и input probe.
- [ ] Добавить outbound attachment policy, MIME/size/secret checks и provider upload adapter.

## v0.6 — SSH, tmux и SFTP

- [ ] SSH profile manager и Keychain references.
- [ ] Host-key verification и явный first-connect экран.
- [ ] Remote PTY в выбранном profile.
- [ ] Controlled tmux: list / create / attach.
- [ ] SFTP browser с file-level upload/download approval и resume.

## Перед рабочим MVP

- [ ] Multi-session, search, notifications, export/delete и crash recovery.
- [ ] Diagnostics bundle с redaction.
- [ ] Apple Silicon и Intel smoke tests.
- [ ] Проверить RAM baseline для четырёх tabs: local PTY, long scrollback, agent stream, SSH.
- [ ] Packaging, update и notarization plan.
