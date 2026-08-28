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
| v0.5 | Готов | local effects и capability SDK |
| v0.6 | В работе | remote profiles (SSH, PTY, tmux, SFTP) |
| v0.7 | Запланировано | MVP UI |

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

### v0.5 — Local effects и capability SDK ✓

**Цель:** безопасные local effects с exact approval, fail-closed sandbox и policy replay.

**Gate:** exact approval, sandbox/reviewer fail closed, policy replay. ✓

- [x] Capability SDK для безопасных local effects
- [x] Exact approval механизм с версионированием действий
- [x] Sandbox fail-closed enforcement
- [x] Policy replay для аудита и compliance
- [x] Effect execution tests с sandbox validation

**v0.5 завершён:** mutating effect требует exact approval или explicit Allow; sandbox denial блокирует unsafe capability; policy replay даёт identical decision для исторического события.

## Текущий релиз: v0.6 — Remote profiles

**Цель (из ROADMAP):** SSH profiles, controlled process/PTY execution, tmux, SFTP.

**Gate:** host-key/target/file approval переживают restart.

### Задачи v0.6

- [x] SSH profiles с host-key verification
  - [x] SSHProfile struct с host, user, port, host_key_fingerprint
  - [x] Host-key verification перед connection (fail если mismatch)
  - [x] Keychain integration для SSH private keys (reference, не raw key)
  - [x] PolicyCheck для SSH connection (origin, target host, user)
  - [x] Durable SSH approval в SQLite (переживает restart)
  - [x] NormalizedEffect::ssh_connect() + NetworkConnect capability расширена на SshConnect
- [x] Controlled process/PTY execution
  - [x] ProcessExecutionRequest с policy check и bounded output
  - [x] ProcessOutput capture с timeout
  - [x] PtySession lifecycle: spawn, attach, detach, terminate
  - [x] PtySessionManager координирует policy, spawn, storage
  - [x] Durable PTY session state в SQLite (SqlitePtySessionStore)
  - [x] Integration tests для process и PTY
- [x] tmux integration для persistent remote sessions
  - [x] TmuxSession lifecycle: create, attach, detach, list, kill
  - [x] TmuxSessionManager координирует SSH, policy, storage
  - [x] SqliteTmuxSessionStore для durable session state
  - [x] Remote command execution через SSH + tmux
  - [x] Policy check для tmux session creation
  - [x] Integration tests для tmux sessions (9 тестов)
- [x] SFTP для remote file access
  - [x] SftpSession lifecycle: connect, disconnect
  - [x] SftpOperationRequest с policy check (Read, Write, Delete, List)
  - [x] SftpSessionManager координирует SSH, policy, operation execution
  - [x] NetworkConnect capability расширена на ActionKind::SftpTransfer
  - [x] Integration tests (4 теста: lifecycle, request, approval, manager)
  - [x] Документация: docs/v0.6-SFTP-IMPLEMENTATION.md

**Статус v0.6:** ✅ Завершён. SSH profiles, PTY/tmux stubs, SFTP stub готовы. Real SSH/SFTP/PTY/tmux executor — фаза F (после A/B/C).

**Gate v0.6:** ✅ host-key/target/file approval переживают restart через SSH profiles + durable store.

## Архитектурный аудит (параллельно с v0.6)

**Документ:** [docs/current-architecture-audit.md](docs/current-architecture-audit.md)

### Фаза A — Safe local execution

- [x] **A0:** Truthful audit и status documents (current-architecture-audit.md создан)
- [x] **A1:** Safe local execution authority ✅
  - [x] AdmittedOperation token для harness-issued work
  - [x] ProcessExecution::execute() требует admission token
  - [x] Regression tests: unadmitted spawn невозможен, agent origin требует approval
  - [x] Type-level enforcement: execute(&AdmittedOperation) signature
  - [x] Документация: docs/A1-IMPLEMENTATION.md
  - **Gate A1:** ✅ no public spawn without admission; exact approval when needed; unavailable Seatbelt fails closed
- [x] **A2:** Origin и approval continuation ✅
  - [x] Server-side origin derivation (IPC tools = User)
  - [x] DeferredEffect storage в AgentRuntime
  - [x] store_deferred_effect / take_deferred_effect API
  - [x] Regression tests: origin derivation, deferred continuation
  - [x] Документация: docs/A2-IMPLEMENTATION.md
  - **Gate A2:** ✅ Agent cannot use user-direct route; stale approval cannot run changed work; approved work resumes exact durable effect (IPC integration pending)
- [x] **A3:** Per-session coordination ✅
  - [x] Убрать global Harness lock
  - [x] Два независимых session делают прогресс concurrently
  - [x] Ordered durable events (EventStore гарантирует)
  - [x] Документация: docs/A3-IMPLEMENTATION.md
  - **Gate A3:** ✅ Two independent sessions make progress concurrently with ordered durable events

### Фаза B — Typed client и subscription

- [ ] **B1:** Typed client и push subscription
  - [ ] Typed domain methods (не IpcResponse pattern-match)
  - [ ] Cursor backfill + store notification (no poll loop)
  - [ ] Reconnect gets only events after cursor
  - [ ] Zap adapter переход на push subscription
- [ ] **B2:** Complete existing typed DTOs
  - [ ] Attachment/diff/detail complete, bounded/redacted
  - [ ] Или capability absent если не реализовано

### Фаза C — Provider registry

- [ ] **C1:** Provider registry/metadata
  - [ ] One provider interface (trait)
  - [ ] No central concrete provider branch
  - [ ] Provider discovery/metadata
- [ ] **C2:** Router и durable budgets
  - [ ] Rules-based fallback
  - [ ] Per-session/agent steps/calls/tokens/cost/time

**Gate A1:** ✅ no public spawn without admission; exact approval when needed; unavailable Seatbelt fails closed.

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
