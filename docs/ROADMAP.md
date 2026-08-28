# Roadmap

Актуальный продуктовый путь с gate criteria. История в [IMPLEMENTATION_HISTORY.md](IMPLEMENTATION_HISTORY.md).

## Статус версий

| Версия | Состояние | Gate |
| --- | --- | --- |
| v0.1 | ✅ Готов | core, durable events/SQLite, policy/approvals, mock runtime |
| v0.2 | ✅ Готов | standalone headless harness, real provider, execution path |
| v0.3 | ✅ Готов | structured clients, external agents (IPC, Zap, ACP, Auth) |
| v0.4 | ✅ Готов | long-session context, compaction, fork/checkpoint, budget |
| v0.5 | ✅ Готов | capability SDK, exact approval, fail-closed sandbox, policy replay |
| v0.6 | ✅ Готов | remote profiles (SSH, PTY stub, tmux stub, SFTP stub) |
| A1 | ✅ Готов | safe local execution authority (admission token, type-level enforcement) |
| A2 | ✅ Готов | origin derivation, deferred effect storage |
| A3 | ✅ Готов | per-session coordination (global lock удалён) |
| B1 | 🚧 Частично | push subscription ✅, typed methods TODO |
| B2 | Запланировано | complete DTOs (attachment/diff/detail) |
| C1 | Запланировано | provider registry/metadata |
| C2 | Запланировано | router + durable budgets |
| v0.7 | Запланировано | MVP UI |

## Завершённые релизы

### v0.1 — Фундамент ✅

- Durable events (SQLite WAL)
- Policy engine + approval system
- Mock runtime + supervisor
- GPUI reference client (опциональный)

### v0.2 — Standalone headless harness ✅

- Real provider integration (OpenAI-compatible streaming)
- Execution seam: Policy → Sandbox → Capability → Execution
- Measured limits + resource baselines
- Session survives restart без дубликатов
- Secret redaction (не попадают в SQLite/logs)

### v0.3 — Structured clients и external agents ✅

**IPC extension:**
- Typed approval payload: diff preview, affected files, scope
- Attachment references (по ID, не inline dump)
- Backend/auth state events
- Negotiated `Incompatible` version mismatch handling

**Zap integration:**
- CLI baseline (create/stream/cancel) в обычной Zap tab
- Zap adapter binary: structured blocks protocol
- OSC escape sequences для notifications
- Live session status bar

**ACP gateway:**
- Manual executable profile
- Mock agent smoke
- Agent-owned login (не хранит credentials)

**Auth Center:**
- Keychain reference для API keys
- System-browser OAuth
- Local no-secret profiles

### v0.4 — Long-session context ✅

**Gate:** restart/fork даёт deterministic projection и bounded memory. ✅

- CompactionPolicy + auto-compaction на token threshold
- Budget integration (SessionSupervisor + IPC events)
- Immutable fork/checkpoint механизм
- Deterministic projection после restart
- Bounded memory tests
- BudgetConfig/BudgetChecker enforcement (turns/tokens/wall_time)

### v0.5 — Local effects и capability SDK ✅

**Gate:** exact approval, sandbox fail closed, policy replay. ✅

- Capability SDK (WorkspaceRead, WorkspaceWrite, ProcessSpawn, NetworkConnect)
- CapabilityVersion для exact approval matching
- Sandbox fail-closed enforcement
- PolicySnapshot и replay для аудита
- ActionFingerprint включает capability version
- Integration tests для всех gate criteria

### v0.6 — Remote profiles ✅

**Gate:** host-key/target/file approval переживают restart. ✅

- SSH profiles с host-key verification
- SSHProfile struct с host_key_fingerprint
- Keychain integration для SSH private keys (SSHKeyReference)
- PolicyCheck для SSH connection
- Durable SSH approval в SQLite (SqliteSSHApprovalStore)
- NormalizedEffect::ssh_connect()
- PTY/tmux/SFTP stubs с lifecycle и storage
- Integration tests (26 remote tests, 12 process/PTY tests)

**Note:** Real SSH/SFTP/PTY/tmux executor — фаза F (после architectural work).

### A1 — Safe local execution authority ✅

**Gate:** No public spawn without admission; exact approval when needed; unavailable Seatbelt fails closed. ✅

- AdmittedOperation type-level token
- ProcessExecution::execute(&AdmittedOperation) signature
- Type-level enforcement: только harness создаёт admission
- Regression tests (unadmitted spawn impossible, agent requires approval)

### A2 — Origin и approval continuation ✅

**Gate:** Agent cannot use user-direct route; approved work resumes exact durable effect. ✅

- Server-side origin derivation (IPC tools = User)
- DeferredEffect storage в AgentRuntime
- store_deferred_effect / take_deferred_effect API
- Regression tests (origin derivation, deferred continuation)
- IPC integration pending (фаза B1)

### A3 — Per-session coordination ✅

**Gate:** Two independent sessions make progress concurrently with ordered durable events. ✅

- Global Harness lock удалён
- EventStore (SQLite WAL) гарантирует thread-safety + ordered events
- AgentRuntime per-session state
- Concurrent session attach/stream/run без serialization

## Текущая работа: B1 — Typed client & push subscription

**Gate:** Clients use typed methods; reconnect gets only events after cursor; no poll loop.

**Статус:** Частично выполнено (commit fc94819)

✅ **Выполнено:**
- Event store notification mechanism (tokio broadcast channel)
- Daemon push delivery (убран 25ms poll loop)
- InMemory client push (убран 10ms poll loop)
- Zap adapter cleanup (убран 100ms sleep)
- Reconnect с cursor (after_sequence в Subscribe/Stream)

❌ **Осталось:**
- Typed client methods (domain results вместо IpcResponse enum)
- High-level API: create_session, stream_events, run_tool
- Error types: ClientError вместо raw IPC enum

**Изменённые файлы (commit fc94819):**
- `crates/impetus-core/src/storage.rs` — broadcast notification в EventStore trait
- `crates/impetus/src/main.rs` — daemon notification_receiver вместо interval
- `crates/impetus-client/src/lib.rs` — InMemory notification_receiver
- `crates/impetus-zap-adapter/src/main.rs` — убран sleep(100ms)
- `crates/impetus-core/src/harness_api.rs` — store() accessor

**Tests:** 224 passed, cargo check clean

## Запланировано

### B2 — Complete existing typed DTOs

**Gate:** Attachment/diff/detail complete, bounded/redacted, or capability absent.

- GetAttachment endpoint (сейчас unavailable)
- Approval detail (сейчас empty fields)
- Durable SHA-256 metadata service (вместо in-memory FNV-1a index)
- Bounded/redacted range reads

### C1 — Provider registry/metadata

**Gate:** One provider interface; no central concrete provider branch.

**Проблема:** `ProviderBackend` enum (Mock | OpenAI) в Harness, нет ModelProvider trait.

**Требуется:**
- ModelProvider trait
- Provider registry + metadata
- Discovery mechanism
- Убрать concrete enum из harness

### C2 — Router и durable budgets

**Gate:** Bounded rules-based fallback; per-session/agent cost tracking.

- Rules-based model fallback
- Per-session steps/calls/tokens/cost/time budgets
- Durable budget state (сейчас in-memory)
- Rate-limit scheduler

### D — Context efficiency

- Deterministic reducers/artifacts
- Cache metrics with measured provider benefit

### E — Advanced sessions

- Shared-prefix DAG fork/checkpoints
- Restore from checkpoint
- Concurrency tests

### F — Remote executor

**Gate:** Real SSH/SFTP/PTY/tmux goes through proven local effect path and durable scoped approval.

- Real SSH connection через SSHProfile
- Live SFTP file operations
- PTY session с real process spawn
- tmux remote session management
- Integration с existing stubs

### v0.7 — MVP UI

**Gate:** Task проходит intent → evidence → approval → effect → resume/fork.

- Session management UI
- Search по сессиям и событиям
- Notifications система
- Export/delete сессий
- Chosen client path (Zap/GPUI/TUI decision)
- End-to-end MVP smoke test

### G — Optional extensions

- TUI (если Zap недостаточно)
- Swarm/profiles/learning (bounded components с measured benefit)

## Не roadmap

- GPUI CI pane — isolated client experiment
- Custom terminal/TUI — только после Zap decision
- Shared-prefix DAG — после E phase
- Multi-provider routing — после C1/C2
- Cloud sync, marketplace, multi-user auth — вне MVP

## Правило готовности

Feature считается готовой только при наличии:
- Tests (unit + integration где применимо)
- Runtime smoke (applicable scenarios работают)
- `cargo test --workspace && cargo clippy --workspace` pass
- Для Rust/CI/dependency changes: `cargo deny check` + local GitLab job
- Документация (IMPLEMENTATION_HISTORY.md или inline docs)

Planned interface, empty DTO, stub implementation не считаются готовой feature.
