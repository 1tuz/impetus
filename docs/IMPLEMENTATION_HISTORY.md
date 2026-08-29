# Implementation History

> Это журнал поставленных slices. Указанные ниже пути и числа тестов отражают
> состояние соответствующего commit и могут не совпадать с текущей структурой
> workspace. За текущей картой компонентов обращайся к
> [ARCHITECTURE.md](ARCHITECTURE.md).

Консолидированная история выполненных архитектурных фаз.

## A1 — Safe Local Execution Authority ✅

**Commit:** 509aab5 (2026-08-28)  
**Gate:** No public spawn without admission; exact approval when needed; unavailable Seatbelt fails closed.

### Изменения

**AdmittedOperation token:**
- `crates/impetus-effects/src/effects.rs` — type-level token для harness-issued work
- `ProcessExecution::execute(&AdmittedOperation)` signature — type-level enforcement
- Только harness может создать AdmittedOperation после policy check

**Type-level safety:**
```rust
pub struct AdmittedOperation {
    pub(crate) action: NormalizedEffect,
    pub(crate) origin: ActionOrigin,
}

impl ProcessExecution {
    pub fn execute(&self, admission: &AdmittedOperation) -> Result<ProcessOutput, ExecutionError> {
        // Only reachable with harness-issued token
    }
}
```

**Regression tests:**
- `crates/impetus-effects/tests/admission_tests.rs` — unadmitted spawn невозможен
- Agent origin требует approval для ProcessSpawn
- User origin может bypass approval (через explicit Allow в PolicyEngine)

**Outcome:** Процесс не может быть запущен без harness admission token.

---

## A2 — Origin & Approval Continuation ✅

**Commit:** 509aab5 (2026-08-28)  
**Gate:** Agent cannot use user-direct route; stale approval cannot run changed work; approved work resumes exact durable effect.

### Изменения

**Server-side origin derivation:**
- `crates/impetus-core/src/harness_api.rs` — IPC tools всегда используют `ActionOrigin::User`
- Raw socket caller не может подделать agent-origin route
- Agent tools (будущее) будут использовать отдельный origin path

**DeferredEffect storage:**
- `crates/impetus-core/src/runtime.rs`:
  - `AgentRuntime::store_deferred_effect(approval_id, effect)` — сохраняет pending work
  - `AgentRuntime::take_deferred_effect(approval_id)` — возвращает exact effect для approved work
  - `HashMap<Uuid, DeferredEffect>` в runtime state

**DeferredEffect type:**
```rust
pub struct DeferredEffect {
    pub effect: NormalizedEffect,
    pub origin: ActionOrigin,
    pub stored_at: SystemTime,
}
```

**Regression tests:**
- `crates/impetus-core/tests/deferred_effect_tests.rs` — store/retrieve cycle
- Origin derivation tests для IPC tools
- Stale approval detection через ActionFingerprint (v0.5 capability version)

**Outcome:** Approval continuation готов к IPC integration (фаза B). Origin forgery невозможен через public IPC.

---

## A3 — Per-Session Coordination ✅

**Commit:** 509aab5 (2026-08-28)  
**Gate:** Two independent sessions make progress concurrently with ordered durable events.

### Изменения

**Global lock removal:**
- `crates/impetus-core/src/harness_api.rs` — удалён `request_lock: Mutex<()>`
- Независимые sessions не блокируют друг друга

**Thread-safety гарантии:**
- `EventStore` (SQLite WAL) — concurrent reads, serialized writes через internal mutex
- `AgentRuntime` — per-session state, immutable после создания
- `PolicyEngine` — stateless, thread-safe
- `ApprovalResolver` — SQLite backend, thread-safe через connection pool

**Concurrency:**
- Два независимых session могут:
  - Attach/detach одновременно
  - Stream events параллельно
  - Run provider calls параллельно
- Ordered events гарантированы SQLite WAL (autoincrement event_id)

**Outcome:** Harness координирует множество sessions без global serialization. EventStore остаётся single source of truth с ordered events.

---

## v0.6 — Remote Profiles (SFTP) ✅

**Commit:** 509aab5 (2026-08-28)  
**Gate:** host-key/target/file approval переживают restart.

### Изменения

**SFTP session management:**
- `crates/impetus-effects/src/sftp.rs`:
  - `SftpSession` lifecycle: connect, disconnect
  - `SftpOperationRequest` с policy check (Read, Write, Delete, List)
  - `SftpSessionManager` координирует SSH, policy, operation execution

**Capability integration:**
- `NetworkConnect` capability расширена на `ActionKind::SftpTransfer`
- Policy check перед SFTP operation (origin, target host, operation type)

**Durable approval:**
- SFTP использует существующий `SSHProfile` + `SqliteSSHApprovalStore`
- Host-key approval переживает restart (gate выполнен)

**Integration tests:**
- `crates/impetus-effects/tests/sftp_tests.rs` — 4 теста:
  - Session lifecycle
  - Operation request validation
  - Approval requirement
  - Manager coordination

**Note:** Stub implementation. Real SSH/SFTP executor — фаза F (после A/B/C architectural work).

**Outcome:** v0.6 gate выполнен. Remote profiles структура готова для real executor integration.

---

## Следующие фазы

**B1 — Typed client & push subscription:**
- Event store notification (cursor backfill + push)
- Typed domain methods (не IpcResponse enum)
- Убрать poll loops из daemon/Zap/memory-client

**B2 — Complete existing DTOs:**
- Attachment/diff/detail endpoints
- Bounded/redacted content delivery

**C1 — Provider registry:**
- ModelProvider trait
- Provider discovery/metadata
- Убрать concrete ProviderBackend enum из Harness
