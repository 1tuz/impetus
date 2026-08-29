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

## Примечание о структуре workspace

Записи выше отражают slices на момент соответствующих commits. Имена crate и
роли binary со временем менялись:

- **target:** `impetus` = user CLI/TUI client, `impetusd` = daemon, `impetus-core` = libraries;
- **не реализовано** на момент этого журнала и позже: Module Runtime, standalone TUI,
  `impetus doctor`, model router, extension compatibility layer, `impetus components`.

Актуальный план — [ROADMAP.md](ROADMAP.md) и [TODO.md](../TODO.md), не этот файл.

## Исторический backlog (pre-2026-09, superseded)

Следующие пункты были next steps на момент ранних фаз; часть уже поставлена
после commits выше. Не использовать как текущий план:

- B1 — typed client, push subscription, убрать poll loops
- B2 — attachment/diff/detail endpoints

---

## Phase 1 (Binary Topology & Doctor baseline) ✅

**Commit:** b40e246  
**Gate:** `impetus doctor` reports daemon health, protocol compatibility, basic subsystem status.

### Изменения

**Binary topology consolidation:**
- Все docs обновлены: `impetus` = client CLI, `impetusd` = daemon
- `Taskfile.yml`: `task daemon` → `cargo run -p impetusd`, `task client` → `cargo run -p impetus`
- Deprecated: `task harness`, `task cli` (forwarding с warnings)
- Install/release docs зафиксированы на финальные binary roles

**Doctor command (baseline):**
- `crates/impetus/src/doctor.rs` — diagnostic framework
- `impetus doctor` — human-readable report с remediation hints
- `impetus doctor --json` — versioned schema (v1) для bug reports

**Implemented probes:**
- `impetus_version` — client version из CARGO_PKG_VERSION
- `socket_path` — socket discovery, existence, permissions, Unix socket type validation
- `daemon_connection` — connection attempt через UnixSocketTransport
- `ipc_protocol` — Hello handshake, version/capabilities negotiation, Incompatible detection
- `daemon_readiness` — health check via list_sessions

**Output format:**
- Human: `✓ ✗ ⚠ ○` status icons, remediation hints
- JSON: versioned schema с `ProbeStatus` enum, optional `details` fields
- Overall status: OK | WARN | ERROR | UNAVAILABLE

**Tests:**
- Existing unit tests pass (`cargo test -p impetus`)
- Manual verification: daemon running/stopped scenarios

**Outcome:** Phase 1 Doctor baseline готов. Расширенные probes (Event Store, Sandbox, Policy, ProviderRegistry, etc.) — следующие итерации.

---

## Phase 2 (Doctor subsystem probes) ✅

**Commit:** (pending)  
**Gate:** `impetus doctor` inspects subsystem health via IPC::Diagnostics endpoint.

### Изменения

**IPC extension:**
- `crates/impetus-core/src/ipc.rs`:
  - `IpcRequest::Diagnostics` — subsystem health query
  - `IpcResponse::Diagnostics { subsystems: Box<SubsystemHealth> }`
  - Added `diagnostics` capability to IPC_CAPABILITIES
- `crates/impetus-core/src/diagnostics.rs` — новый модуль:
  - `SubsystemHealth` struct с полями: event_store, artifact_store, policy_engine, provider_registry, sandbox, credential_store
  - `SubsystemStatus { available, message, details }` — статус каждого subsystem

**Harness integration:**
- `crates/impetus-core/src/harness_api.rs`:
  - `gather_subsystem_health()` — server-side diagnostics
  - Проверки: Event Store (via list_sessions), Artifact Store (ephemeral marker), Policy Engine, ProviderRegistry, Sandbox (platform check), Credential Store (Keychain на macOS)

**Client probes:**
- `crates/impetus/src/doctor.rs`:
  - `probe_daemon_connection()` расширен с Diagnostics endpoint
  - `add_subsystem_probes()` — transform SubsystemHealth в DoctorReport probes
  - Все subsystem проверки выполняются через IPC, без direct access к daemon internals

**Subsystem coverage:**
- Event Store: operational status, session count
- Artifact Store: ephemeral/durable backing (текущая реализация — in-memory)
- Policy Engine: active, workspace_root
- ProviderRegistry: registered providers list
- Sandbox: platform availability (Seatbelt на macOS, fail-closed mode)
- Credential Store: platform keychain accessibility (macOS Keychain)

**Output:**
- Human: subsystem probes интегрированы в unified report
- JSON: subsystem details в structured schema с nested `details` fields

**Tests:**
- Existing tests pass (268 passed, 2 ignored)
- Clippy clean (large variant warning fixed via Box)
- Manual verification: daemon running scenario, все subsystem статусы OK

**Outcome:** Phase 2 завершён. Doctor теперь показывает harness-subsystem health через typed IPC. Следующие probes (tools/capabilities, ACP adapters, web research, disk health) — опциональные расширения.

---

## Phase 1 — Binary Topology & Auto-Spawn ✅

**Commit:** 6679042 (2026-08-29)  
**Gate:** Client must safely discover and spawn daemon; stale sockets cleaned; doctor diagnoses without mutating state.

### Изменения

**Auto-spawn daemon:**
- `crates/impetus/src/daemon.rs` — новый модуль:
  - `discover_socket_path()` — IMPETUS_SOCKET env или default `~/Library/Application Support/Impetus/harness.sock`
  - `is_daemon_running()` — async socket connect probe
  - `ensure_daemon_running()` — spawn if needed, wait up to 3s, remove stale socket
  - `find_impetusd_binary()` — lookup: target/debug (dev), PATH, sibling binary (release)

**Client integration:**
- `crates/impetus/src/main.rs`:
  - All commands except `doctor` call `ensure_daemon_running()` before connecting
  - Doctor intentionally skips auto-spawn to diagnose actual state
  - Socket discovery centralized via `daemon::discover_socket_path()`

**Binary discovery strategy:**
1. Development: `target/debug/impetusd` next to `target/debug/impetus`
2. Installed: `which impetusd` from PATH
3. Release bundle: `impetusd` next to `impetus` binary

**Graceful startup:**
- Remove stale socket before spawn
- Spawn with null stdin/stdout/stderr (daemon detaches)
- Poll socket availability every 500ms for up to 3 seconds
- Clear error message if daemon fails to start

**Tests:**
- Manual: `pkill impetusd; cargo run -p impetus -- list` → daemon spawns, command succeeds
- Manual: `cargo run -p impetus -- doctor` → reports actual state, no auto-spawn
- Existing tests pass (270 passed, 2 ignored)

**Outcome:** Phase 1 topology complete. Client transparently manages daemon lifecycle. Next: release artifacts with install/uninstall docs.

---

## Phase 2 — Module Runtime Foundation ✅

**Commit:** 2321e2c (2026-08-29)  
**Gate:** Pluggable strategies, capability probing, UnknownOutcome enforcement, safe fallback policies.

### Изменения

**Module contracts:**
- `crates/impetus-core/src/module.rs` — core types:
  - `ModuleDescriptor` — id, name, version, kind, provides/requires, capabilities, permissions
  - `ModuleKind` — AgentLoop, Scheduler, ToolProvider, SearchBackend, BrowserProvider, CredentialResolver, PolicyExtension, Custom
  - `ModuleState` — Discovered, Probing, Ready, Starting, Running, Degraded, Stopping, Stopped, Failed, Incompatible
  - `ExecutionSemantics` — ReadOnly, Idempotent, Mutating, NonReplayable
  - `ModulePermissions` — filesystem, network, process, secrets, remote
  - `CapabilityProbe` — availability check with version/details
  - `CompatibilityReport` — harness/module version compatibility

**Module registry:**
- `crates/impetus-core/src/module_registry.rs`:
  - `ModuleRegistry` — register, list, get, update state/health, probe capabilities, check compatibility
  - Thread-safe via `Arc<RwLock<HashMap>>`
  - Tests: register/list, probe capabilities

**Service contracts:**
- `crates/impetus-core/src/service_contract.rs`:
  - `AgentLoopStrategy` trait — execute_turn, name, version, supports_capability
  - `AgentScheduler` trait — schedule/cancel tasks
  - `ServiceRegistry` — pluggable strategies (agent_loop, scheduler)
  - `ScheduledTask` — Immediate, Delayed, Cron

**Module lifecycle:**
- `crates/impetus-core/src/module_lifecycle.rs`:
  - `ModuleLifecycle` — discover, probe, start, stop, health_check, restart
  - State transitions: Discovered → Probing → Ready → Starting → Running
  - Degraded mode support when capabilities partially available
  - Tests: full lifecycle transitions

**Fallback policies:**
- `crates/impetus-core/src/module_fallback.rs`:
  - `FallbackPolicy` — strategy (FailFast, Retry, Alternate, Degrade, SafeDefault), max_retries, allow_degraded
  - `UnknownOutcomePolicy` — enforces no retry/fallback for Mutating/NonReplayable operations with UnknownOutcome
  - `OperationOutcome` — Success, Failure, Unknown
  - Default policies per ModuleKind (e.g., CredentialResolver → FailFast, SearchBackend → Alternate)
  - Tests: UnknownOutcome blocks mutating retry, allows idempotent/readonly retry

**Dependencies:**
- Added `chrono` to impetus-core for ModuleHealth timestamps

**Tests:**
- 277 passed, 2 ignored
- Coverage: lifecycle transitions, UnknownOutcome enforcement, per-kind fallback policies

**Outcome:** Phase 2 foundation complete. Pluggable strategies, safe fallback policies, UnknownOutcome enforcement in place. Next: external process isolation, integration tests for degraded/incompatible paths.

---

## Исторический backlog (pre-2026-09, superseded)

Следующие пункты были next steps на момент ранних фаз; часть уже поставлена
после commits выше. Не использовать как текущий план:

- B1 — typed client, push subscription, убрать poll loops
- B2 — attachment/diff/detail endpoints
- C1 — `ModelProvider` / registry (foundation есть; router — target)
