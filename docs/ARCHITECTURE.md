# Архитектура Impetus

Impetus — headless harness для безопасного исполнения coding agent с durable state, fail-closed policy и structured client protocol.

## Принципы

- **Harness-first:** standalone Rust runtime и CLI; terminal client отделён
- **Fail-closed:** policy → sandbox → capability → execution; unavailable блокирует
- **Durable state:** SQLite WAL для events, approvals, sessions; restart не теряет контекст
- **Typed protocol:** versioned IPC с typed approvals, diffs, attachments
- **Zero raw secrets:** macOS Keychain references, не token в SQLite/logs

## Архитектура

```
Disposable clients: TUI | Zap adapter | CLI | GPUI | IDE | ACP
  │ typed SDK; no direct store/core ownership
  ▼
impetusd (Unix socket daemon)
  ├─ session manager: per-session coordination and event cursor
  ├─ agent runtime: lifecycle, budgets, interrupt/cancel
  ├─ provider registry plus deterministic model router
  ├─ tool/effect orchestrator
  │ trusted origin → normalized effect → policy → sandbox
  │ → capability → execution → durable event
  ├─ artifact/checkpoint/context services
  └─ append-only EventStore: single durable truth
       └─ cursor backfill then ordered push
```

## Execution Path

```
User Request
    ↓
NormalizedEffect (typed action)
    ↓
PolicyEngine → Allow / Deny / NeedsApproval
    ↓
[if NeedsApproval] → ApprovalResolver → wait for user approval
    ↓
Sandbox availability check (fail-closed)
    ↓
AdmittedOperation token (harness-issued)
    ↓
Capability version match check (exact approval)
    ↓
Execution (process, PTY, network, file ops)
    ↓
ToolOutcome → stored в events
```

## Ключевые модули

### impetus-effects (`crates/impetus-effects/`)

**Effect Seam** (`effects.rs`)
- `EffectSeam`: координирует Policy → Approval → Sandbox → Capability → Execution
- `NormalizedEffect`: типизированные действия (file ops, process, network)
- `AdmittedOperation`: type-level token для harness-issued work (фаза A1)
- `ActionFingerprint`: детерминистический hash для stale approval detection
- Fail-closed: unavailable sandbox блокирует execution

**Capabilities** (`capabilities.rs`)
- `WorkspaceRead`, `WorkspaceWrite`, `ProcessSpawn`, `NetworkConnect`
- `CapabilityVersion` для exact approval matching

**Process Execution** (`process.rs`)
- `ProcessExecution::execute(&AdmittedOperation)` — type-level enforcement
- `ProcessOutput` с bounded output (2MB limit) и timeout

**SFTP** (`sftp.rs`)
- `SftpSession` lifecycle: connect, disconnect
- `SftpOperationRequest` с policy check (Read, Write, Delete, List)
- `SftpSessionManager` координирует SSH, policy, operation execution

### impetus-core (`crates/impetus-core/`)

**Harness** (`harness_api.rs`)
- IPC dispatch, session attach, provider selection, run/cancel
- Tool dispatch, approval lookup и redaction
- **A3:** per-session coordination (global lock удалён)
- **A2:** server-side origin derivation (IPC tools = User)

**Runtime** (`runtime.rs`)
- `AgentRuntime`: durable session/intent/run/approval lifecycle
- `SessionSupervisor`: координирует provider, policy, budget, compaction
- **A2:** `DeferredEffect` storage для approval continuation
  - `store_deferred_effect(approval_id, effect)`
  - `take_deferred_effect(approval_id) -> Option<DeferredEffect>`

**EventStore** (`storage.rs`, `events.rs`)
- `SqliteEventStore`: append-only events в SQLite WAL
- Session state переживает restart без дубликатов
- Thread-safe: concurrent reads, serialized writes
- Ordered events (autoincrement event_id)

**Policy & Approval** (`policy.rs`, `approval.rs`)
- `PolicyEngine`: origin-based decisions (Allow, Deny, NeedsApproval)
- `ApprovalResolver`: durable approval storage в SQLite
- `ActionOrigin`: User | Agent — только user может bypass approval
- Policy replay для аудита

**Budget & Compaction** (`budget.rs`)
- `BudgetChecker`: max_turns, max_tokens, max_wall_time enforcement
- Auto-compaction на token threshold
- Budget state events в IPC для live display

**Auth** (`auth.rs`)
- Keychain reference для API keys (не raw token)
- System-browser OAuth с user action
- Local no-secret profiles

**Provider** (`provider.rs`)
- OpenAI-compatible streaming
- Real provider integration с secret redaction
- **TODO (C1):** ModelProvider trait + registry

**IPC** (`ipc.rs`)
- Versioned local protocol: capability negotiation, typed approvals, diffs
- Disconnect/crash client не убивает durable session
- **TODO (B1):** typed domain methods + push subscription

**Remote** (`remote/`)
- `profile.rs`: SSHProfile с host_key_fingerprint
- `storage.rs`: SqliteSSHApprovalStore для durable SSH approvals
- `tmux.rs`: TmuxSession lifecycle (stub)
- `pty.rs`: PtySession lifecycle (stub)
- Host-key verification перед connection

### impetus-daemon (`crates/impetus-daemon/`)

- Unix socket server, JSON-lines IPC v2
- **TODO (B1):** cursor backfill + store notification (сейчас 25ms poll)

### impetus-zap (`crates/impetus-zap/`)

- Zap adapter: structured blocks protocol
- OSC escape sequences для notifications
- **TODO (B1):** push subscription (сейчас 100ms poll)

### impetus-cli (`crates/impetus/`)

- Standalone headless CLI: create/stream/cancel sessions
- Работает в обычной Zap tab

### impetus-app (`crates/impetus-app/`)

- Optional GPUI reference client
- Direct core import (bypass client seam)

## Trust Boundary

- **Controlled execution:** harness координирует process/PTY/network/file ops
- **Terminal rendering:** клиентская функция (ANSI parser, tabs, scrollback)
- **Secrets:** только macOS Keychain; в SQLite/logs — reference метки
- **Клиент не владеет:** SQLite connection, секретами, SSH transport, policy

## Testing

- **247 workspace tests** (включая 38 для execution + remote)
- Integration tests для process, PTY, SSH, tmux, SFTP
- Policy replay тесты для аудита
- Fail-closed sandbox tests
- Regression tests для A1/A2/A3 gates

## Current State

**Готово:**
- ✅ A0-A3: Safe execution, origin derivation, per-session coordination
- ✅ v0.1-v0.6: Durable events, real provider, structured clients, budget, capability SDK, remote profiles

**В работе:**
- B1: Typed client + push subscription (убрать poll loops)
- B2: Complete DTOs (attachment/diff/detail endpoints)
- C1: Provider registry/metadata (убрать concrete enum)

**Запланировано:**
- v0.7: MVP UI (session management, search, notifications)
- F: Real SSH/SFTP/PTY/tmux executor (после B/C architectural work)

## Не сейчас

- Custom terminal/TUI, swarm, learning, SOUL profiles, LSP/MCP indexing
- Shared-prefix DAG, multi-provider routing
- Cloud sync, marketplace, multi-user auth, Windows/Linux
