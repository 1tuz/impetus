# Что сейчас делать

Подробный план: [docs/ROADMAP.md](docs/ROADMAP.md). Этот файл — только
очередь ближайших работ.

## Статус продукта

| Версия | Состояние | Что это значит |
| --- | --- | --- |
| v0.1 | Фундамент готов | events, SQLite, policy, approvals, mock runtime и базовый GPUI preview |
| v0.2 | Готов | standalone headless harness с real provider и безопасным execution path |
| v0.3 | Готов | structured clients и external agents |
| v0.4 | Готов | long-session context, compaction, immutable fork/checkpoint |
| v0.5–v0.7 | Запланировано | capabilities, remote profiles, MVP UI |

> Native-window smoke для GPUI на чистом Mac остаётся открытым техническим
> хвостом v0.1. Он не блокирует текущую работу.

## Завершённые релизы

### v0.1 — Фундамент ✓

- [x] Durable events (SQLite WAL)
- [x] Policy engine + approval system
- [x] Mock runtime + supervisor
- [x] GPUI reference client (опциональный)

### v0.2 — Standalone headless harness ✓

- [x] Real provider integration (OpenAI-compatible streaming)
- [x] Execution seam: Policy → Sandbox → Capability → Execution
- [x] Measured limits + resource baselines
- [x] Session survives restart без дубликатов
- [x] Secret redaction (не попадают в SQLite/logs)

### v0.3 — Structured clients и external agents ✓

**Шаг 1 — IPC extension:**
- [x] Typed approval payload: diff preview, affected files, estimated scope
- [x] Attachment references: artifact/output content по ID, не inline dump
- [x] Backend/auth state events: provider health, keychain availability, token expiry warning
- [x] Negotiated `Incompatible`: client/harness version mismatch handling

**Шаг 2 — Zap integration:**
- [x] CLI baseline (create/stream/cancel) работает в обычной Zap tab
- [x] Zap adapter binary: подписывается на harness events, рендерит typed blocks
- [x] OSC escape sequences: harness → Zap notification hooks
- [x] Structured blocks protocol: diff, approval, output, attachment, status, error
- [x] Live session status bar: Running / Idle / NeedsApproval

**Шаг 3 — ACP gateway:**
- [x] Manual executable profile: user указывает путь к agent CLI
- [x] Mock agent: initialize/session/stream/cancel/permission/exit smoke
- [x] Agent-owned login: ACP backend не хранит credentials, только forwards prompts

**Шаг 4 — Auth Center contract:**
- [x] Keychain reference profile для API keys
- [x] System-browser OAuth: URL открывается действием пользователя, callback handling
- [x] Local no-secret profile для localhost/mock providers

### v0.4 — Long-session context ✓

**Gate:** restart/fork даёт deterministic projection и bounded memory.

- [x] CompactionPolicy и separate compaction model
- [x] Auto-compaction на token threshold
- [x] Интеграция budget в SessionSupervisor
- [x] Budget state events в IPC (для TUI/Zap live display)
- [x] Immutable fork/checkpoint механизм
- [x] Deterministic projection после restart/fork
- [x] Bounded memory tests

**Дополнительно:**
- [x] BudgetConfig и BudgetState типы (max_turns, max_tokens, max_wall_time, reasoning_effort)
- [x] BudgetChecker enforcement (turn/token/wall time limits)
- [x] Unit-тесты budget logic

## Текущий релиз: v0.5 — Local effects и capability SDK ✓

**Цель (из ROADMAP):** безопасные local effects с exact approval, fail-closed sandbox и policy replay.

**Gate:** exact approval, sandbox/reviewer fail closed, policy replay. ✓

### Задачи v0.5

- [x] Capability SDK для безопасных local effects
- [x] Exact approval механизм с версионированием действий
- [x] Sandbox fail-closed enforcement
- [x] Policy replay для аудита и compliance
- [x] Effect execution tests с sandbox validation

**v0.5 завершён:** mutating effect требует exact approval или explicit Allow; sandbox denial блокирует unsafe capability; policy replay даёт identical decision для исторического события.

## Следующий релиз: v0.6 — Remote profiles

**Gate:** host-key/target/file approval переживают restart.

- [ ] SSH profiles с host-key verification
- [ ] Controlled process/PTY execution
- [ ] tmux integration для persistent remote sessions
- [ ] SFTP для remote file access
- [ ] Durable approval для remote targets

### v0.7 — MVP финализация

**Gate:** task проходит intent → evidence → approval → effect → resume/fork.

- [ ] Session management UI
- [ ] Search по сессиям и событиям
- [ ] Notifications система
- [ ] Export/delete сессий
- [ ] Chosen client path (Zap/GPUI/TUI decision)
- [ ] End-to-end MVP smoke test

## Не сейчас

- GPUI native-window smoke и CI pane smoke — отдельные client checks, не блокеры.
- Custom terminal/TUI — только после Zap decision и зафиксированного неудовлетворённого requirement.
- Cloud sync, marketplace, multi-user auth, Windows/Linux parity — вне MVP scope.
