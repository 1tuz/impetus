# Что сейчас делать

Подробный план: [docs/ROADMAP.md](docs/ROADMAP.md).  
Этот файл — только очередь ближайших работ.

## Статус продукта

| Версия | Состояние | Gate |
| --- | --- | --- |
| v0.1–v0.6 | ✅ Готово | Фундамент, harness, clients, context, capabilities, remote profiles |
| A1–A3 | ✅ Готово | Safe execution, origin, per-session coordination |
| B1 | 🚧 **Частично** | Push subscription ✅, typed methods TODO |
| B2 | Запланировано | Complete DTOs (attachment/diff/detail) |
| C1–C2 | Запланировано | Provider registry + durable budgets |
| v0.7 | Запланировано | MVP UI |

> Native-window smoke для GPUI reference client остаётся открытым хвостом v0.1. Не блокирует работу.

## Завершённые фазы

### v0.1 — Фундамент ✓

- Durable events (SQLite WAL)
- Policy engine + approval system
- Mock runtime + supervisor
- GPUI reference client

### v0.2 — Standalone headless harness ✓

- Real provider (OpenAI-compatible streaming)
- Execution seam: Policy → Sandbox → Capability → Execution
- Session survives restart
- Secret redaction

### v0.3 — Structured clients ✓

- IPC: typed approvals, diffs, attachments, backend states
- Zap integration: CLI baseline, adapter, structured blocks, OSC notifications
- ACP gateway: manual executable profile, mock agent
- Auth: Keychain, system-browser OAuth, local no-secret

### v0.4 — Long-session context ✓

- CompactionPolicy + auto-compaction
- Budget integration (turns/tokens/wall_time)
- Immutable fork/checkpoint
- Deterministic projection

### v0.5 — Capability SDK ✓

- Capability types (WorkspaceRead, WorkspaceWrite, ProcessSpawn, NetworkConnect)
- Exact approval (CapabilityVersion matching)
- Sandbox fail-closed enforcement
- Policy replay для аудита

### v0.6 — Remote profiles ✓

- SSH profiles с host-key verification
- SSHProfile + SqliteSSHApprovalStore
- Keychain integration (SSHKeyReference)
- PTY/tmux/SFTP stubs с lifecycle + storage
- 38 integration tests (process 12, remote 26)

### A1 — Safe local execution authority ✓

**Gate:** No public spawn without admission; exact approval when needed. ✓

- AdmittedOperation type-level token
- ProcessExecution::execute(&AdmittedOperation) signature
- Regression tests (unadmitted spawn impossible, agent requires approval)

### A2 — Origin & approval continuation ✓

**Gate:** Agent cannot use user-direct route; approved work resumes exact effect. ✓

- Server-side origin derivation (IPC tools = User)
- DeferredEffect storage (store_deferred_effect / take_deferred_effect)
- Regression tests (origin derivation, deferred continuation)

### A3 — Per-session coordination ✓

**Gate:** Independent sessions make progress concurrently with ordered events. ✓

- Global Harness lock удалён
- EventStore thread-safe (SQLite WAL)
- Concurrent session attach/stream/run

## Текущая работа: B1 — Typed client + push subscription

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

## Следующие задачи

### B2 — Complete existing DTOs

**Gate:** Attachment/diff/detail complete, bounded/redacted, or capability absent.

- GetAttachment endpoint (сейчас unavailable)
- Approval detail (сейчас empty fields)
- Durable SHA-256 metadata service (вместо in-memory FNV-1a)
- Bounded/redacted range reads

### C1 — Provider registry/metadata

**Gate:** One provider interface; no central concrete provider branch.

**Проблема:** `ProviderBackend` enum (Mock | OpenAI) в Harness, нет ModelProvider trait.

**Требуется:**
- ModelProvider trait
- Provider registry + metadata
- Discovery mechanism

### C2 — Router + durable budgets

- Rules-based model fallback
- Per-session cost tracking (durable, не in-memory)
- Rate-limit scheduler

### v0.7 — MVP UI

**Gate:** Task проходит intent → evidence → approval → effect → resume/fork.

- Session management UI
- Search по сессиям и событиям
- Notifications система
- Export/delete сессий
- Chosen client path (Zap/GPUI/TUI decision)
- End-to-end MVP smoke test

## Архитектурные проблемы (из audit)

**Resolved (5 из 10):**
- ✅ #1: Global Harness lock (A3)
- ✅ #2: ProcessExecution bypass (A1)
- ✅ #4: Approval continuation (A2)
- ✅ #5: IPC origin hardcoded (A2)
- ✅ #10: Roadmap docs overstated (corrected)

**Remaining (priority order):**
1. **#8: Poll loops** (daemon/Zap/memory-client) → **B1 (NEXT)**
2. **#9: Provider concrete enum** → C1
3. **#7: Attachment/detail placeholders** → B2
4. #3: ProcessSpawn workspace scope → future
5. #6: ACP raw credentials → future

## Не сейчас

- Custom terminal/TUI — только после Zap decision
- Swarm, learning, profiles — после MVP
- Shared-prefix DAG — фаза E
- Multi-provider routing — после C1/C2
- Cloud sync, marketplace, multi-user — вне MVP

## Verification

Перед commit:

```bash
cargo fmt --all
cargo test --workspace
cargo check --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

Для Rust/CI/dependency changes:

```bash
cargo deny check
# gitlab-ci-local --stage verify (если доступен Docker)
```
