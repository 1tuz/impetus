# Архитектура Impetus

Impetus — headless harness для безопасного исполнения coding agent действий с durable state, fail-closed policy и structured client protocol.

## Принципы

- **Harness-first:** standalone Rust runtime и CLI; terminal client отделён
- **Fail-closed:** policy → sandbox → capability → execution; unavailable блокирует
- **Durable state:** SQLite WAL для events, approvals, sessions; restart не теряет контекст
- **Typed protocol:** versioned IPC с typed approvals, diffs, attachments
- **Zero raw secrets:** macOS Keychain references, не token в SQLite/logs

## Ключевые модули

### Core (`crates/impetus-core/src/`)

**Effect Seam** (`effects.rs`)
- `EffectSeam`: координирует Policy → Approval → Sandbox → Capability → Execution
- `NormalizedEffect`: типизированные действия (file ops, process, network)
- `ActionFingerprint`: детерминистический hash для stale approval detection
- Fail-closed: unavailable sandbox блокирует execution

**Policy & Approval** (`policy.rs`, `approval.rs`)
- `PolicyEngine`: origin-based decisions (Allow, Deny, NeedsApproval)
- `ApprovalResolver`: durable approval storage в SQLite
- `ActionOrigin`: User | Agent — только user может bypass approval

**Storage** (`storage.rs`, `events.rs`)
- `SqliteEventStore`: durable events в SQLite WAL
- Session state переживает restart без дубликатов
- Immutable fork/checkpoint для deterministic projection

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

**IPC** (`ipc.rs`)
- Versioned local protocol: capability negotiation, typed approvals, diffs, attachments
- Disconnect/crash client не убивает durable session

**Execution** (`execution/`)
- `process.rs`: ProcessExecutionRequest с policy check, bounded output, timeout
- `pty.rs`: PtySession lifecycle (spawn, attach, detach, terminate)
- `storage.rs`: SqlitePtySessionStore для durable PTY state
- Integration tests: 12 тестов покрывают process/PTY scenarios

**Remote** (`remote/`)
- `profile.rs`: SSHProfile с host_key_fingerprint, builder pattern
- `storage.rs`: SqliteSSHApprovalStore для durable SSH approvals
- `tmux.rs`: TmuxSession lifecycle (create, attach, detach, list, kill)
- `tmux_storage.rs`: SqliteTmuxSessionStore для persistent remote sessions
- `mod.rs`: host-key verification, SSH connection policy checks
- Integration tests: 26 тестов покрывают SSH, tmux, storage

**Supervisor** (`supervisor.rs`, `runtime.rs`)
- `SessionSupervisor`: координирует provider, policy, budget, compaction
- Durable session management с restart support

**Tools** (`tools.rs`)
- Tool registry и execution coordination

**Plugins** (`plugins.rs`)
- Plugin system для расширений

**CI** (`ci.rs`)
- CI integration helpers

### CLI (`crates/impetus/`)

- Standalone headless CLI: create/stream/cancel sessions
- Работает в обычной Zap tab без специального рендерера

### App (`crates/impetus-app/`)

- Optional GPUI reference client
- Не влияет на harness core dependency boundary

### CLI Adapters (`crates/impetus-cli/`)

- Zap adapter binary: structured blocks protocol
- OSC escape sequences для harness → Zap notification hooks
- Live session status bar

## Execution Path

```
User Request
    ↓
NormalizedEffect
    ↓
PolicyEngine → Allow / Deny / NeedsApproval
    ↓
[if NeedsApproval] → ApprovalResolver → wait for user approval
    ↓
Sandbox availability check (fail-closed)
    ↓
Capability version match check (exact approval)
    ↓
Execution (process, PTY, network, file ops)
    ↓
ToolOutcome → stored в events
```

## Trust Boundary

- **Controlled shell/process/PTY:** capability исполнения; harness координирует
- **ANSI parser, tabs, scrollback, terminal renderer:** клиентская функция
- **Secrets:** только macOS Keychain; в SQLite/logs/tracing — reference метки
- **Клиент не владеет:** SQLite connection, секретами, SSH transport, policy

## Protocol Layering

- **Local IPC:** versioned typed protocol между harness и клиентом
- **ACP:** протокол для external coding agents; авторизация принадлежит agent CLI
- **Zap integration:** adapter binary рендерит structured blocks; OSC hooks для notifications

## Current State (v0.6)

- ✓ SSH profiles с host-key verification
- ✓ Controlled process/PTY execution
- ✓ tmux integration для persistent remote sessions
- ⏳ SFTP для remote file access (запланировано)
- ⏳ Zap native button для harness integration (запланировано)

## Testing

- **124 unit tests** в workspace (включая 38 для execution + remote)
- Integration tests для process, PTY, SSH, tmux
- Policy replay тесты для аудита
- Fail-closed sandbox tests

## Next Steps

- v0.6: завершить SFTP для remote file access
- v0.7: MVP UI finalization
- Zap fork: добавить кнопку для native harness integration
