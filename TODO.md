# TODO — Agentic Harness для macOS

## Завершить фундамент v0.1

- [x] Rust workspace: независимый `core` и отдельный GPUI reference client.
- [x] GPUI-CE native preview на macOS.
- [x] Rust `1.98.0` в `rust-toolchain.toml`.
- [x] SQLite WAL event store, policy, approval и capability manifests.
- [x] Русские README, архитектура, GUI/UX, референсы и `AGENTS.md`.
- [x] Редактируемая архитектурная схема и PNG preview.
- [x] SQLite reopen test и первый RSS baseline диагностического клиента.
- [ ] Выполнить воспроизводимый native-window smoke на чистом Mac; это последний gate старого v0.1, не зависимость harness v0.2.

## Сейчас: v0.2 — standalone harness core

Подробный порядок, gates и риски: [аудит](docs/iteration-2-audit.md) и [roadmap второй итерации](docs/iteration-2-roadmap.md).

### Gate 0.1 — готово

- [x] Заменить свободный `Event.body` на versioned typed payloads для Session/Run/Intent/Plan/Tool/Agent/Approval/Notice lifecycle.
- [x] Добавить pure event → projection слой без GPUI типов.
- [x] Добавить SQLite migration/read compatibility без потери старых rows.
- [x] Покрыть round-trip, deterministic replay, unknown version, malformed payload и reopen tests.

### Gate 0.2 — готово

- [x] Сделать session identity, sequence и pending approvals восстанавливаемыми после restart.
- [x] Реализовать session supervisor: start, stream, soft interrupt, hard cancel, failure и provider restart.
- [x] Добавить mock streaming provider для CI без модели и секрета.

### Следующие gates v0.2
- [x] Выделить долгоживущий headless Harness process с versioned Unix socket IPC, handshake и socket mode `0600`.
- [x] Добавить CLI reference client: create/attach/list/prompt/status/cancel из Zap и Terminal.app.
- [x] Добавить durable event stream в daemon/CLI: attach/reconnect видит те же event IDs без SQLite доступа.
- [x] Подключить mock stream/restart к daemon и CLI: attach/reconnect видит durable Agent chunks без дубликата после restart.
- [x] Добавить read-only workspace tools: list/read/search с provenance, bounded output и artifact references.
- [ ] Реализовать первый OpenAI-compatible adapter: streaming + cancellation + DeepSeek/OpenRouter/custom/local endpoint profile.
- [ ] Реализовать Keychain credential reference и local/no-secret profile; token не попадает в events/log/export.
- [ ] Ввести единый normalized policy/capability execution seam; не включать unrestricted effects без OS sandbox proof.
- [ ] Зафиксировать harness RSS, queue/artifact limits, restart/cancel и context/token baseline.
- [ ] Доказать отсутствующую зависимость headless runtime от GPUI, Metal, PTY и ANSI renderer.

## Параллельный незавершённый клиентский срез

- [ ] GitLab CI native smoke: установленными `gitlab-ci-local` и `glab` подтвердить общий `PipelineModel`, compact error fragment и keyboard disclosure лога.
- [ ] Не считать 600-line client buffer bounded Harness output: заменить unbounded channel/full log только после общего artifact/execution seam.
- [ ] Не расширять CI pane в отдельный runner/dashboard и не делать его gate harness v0.2.

## Затем: v0.3 — clients, Zap, ACP и auth

- [ ] Расширить v0.2 IPC: approvals, diffs, attachment refs и backend/auth states.
- [ ] Сохранить capability negotiation и явный `Incompatible` state при расширении protocol.
- [ ] Провести Zap baseline smoke: v0.2 harness CLI в обычной tab без модификации Zap.
- [ ] Выбрать structured Zap path: adapter или личный fork; typed Blocks/approval обязательны, OSC-only недостаточно.
- [ ] Добавить `agentic-terminal-acp` на официальном Rust SDK `agent-client-protocol = 2`.
- [ ] Добавить mock ACP agent: initialize, session, stream, cancel, permission, malformed stdout, exit/reconnect.
- [ ] Подключать Codex / Claude / Cursor / Gemini / Qwen только через registry/discovery и установленный CLI.
- [ ] Реализовать Auth Center contract: agent-owned CLI, system-browser OAuth и расширенные states поверх v0.2 Keychain/local profiles.
- [ ] Проверить, что client/ACP permission всегда проходит `Policy → Approval → Sandbox` внутри harness.
- [ ] Добавить `Manual | Safe Auto` mock state и attachment capability negotiation без host effects.

## v0.4 — длинные сессии

- [ ] Token-budget context builder.
- [ ] Versioned compaction: source event range, prompt/version, summary и восстановление.
- [ ] Resume с compacted context после restart без изменения durable projection.
- [ ] Fork с immutable parent prefix и видимым fork point.
- [ ] Метрики: RSS, queued events, bounded output bytes, Blocks и compaction ratio.

## v0.5 — локальные эффекты и capabilities

- [ ] Exact approval: diff, command и target.
- [ ] Workspace sandbox с time/resource limits.
- [ ] Capability host: manifest/version/permission validation.
- [ ] Внешние capabilities только out-of-process по versioned IPC.
- [ ] Safe Auto enforcement: hard-deny, human-only, cache invalidation, block thresholds и input probe.
- [ ] Outbound attachment policy, MIME/size/secret checks и provider upload adapter.

## v0.6 — remote capabilities

- [ ] SSH profile manager и Keychain references.
- [ ] Host-key verification и явный first-connect flow в выбранном клиенте.
- [ ] Controlled remote process/PTY в выбранном profile.
- [ ] Controlled tmux: list/create/attach.
- [ ] SFTP с file-level upload/download approval и resume.

## Перед рабочим MVP

- [ ] Multi-session, search, notifications, export/delete и crash recovery.
- [ ] Diagnostics bundle с redaction.
- [ ] Apple Silicon и Intel harness smoke tests.
- [ ] Packaging/update plan для harness и выбранного клиента.
- [ ] Проверить end-to-end в Zap: prompt → plan → approval → effect → resume/fork.

## Optional backlog: собственный GPUI terminal

- [ ] После Zap spike записать конкретный неудовлетворённый requirement; без него не продолжать terminal emulator.
- [ ] Если принято go: исследовать `portable-pty` + `alacritty_terminal`, lifecycle, bounded scrollback, tabs/focus/selection/copy и soak.
- [ ] Если Zap закрывает требования: оставить GPUI app reference client, не превращать его в второй terminal product.
