# Architecture Audit — Historical snapshot

> Historical snapshot. Not current architecture. This is the 2026-08-28 pre-B1
> baseline retained to explain the work that followed. Read
> [the canonical architecture](../ARCHITECTURE.md) and [ROADMAP.md](ROADMAP.md)
> for the current state.

Date: 2026-08-28  
Baseline: commit 509aab5, 247 tests passing, cargo verify clean

## Scope

Audit актуального checkout: workspace manifests, все crate sources, tests, docs. Не authorization для broad refactor — только state snapshot.

## Архитектура

### CURRENT

```
CLI / Zap adapter / optional GPUI app / future clients
  │
  ├─ CLI and Zap use raw IpcRequest/IpcResponse
  ├─ GPUI app imports impetus-core directly
  ▼
Unix socket daemon
  │ JSON-lines IPC v2, socket mode 0600
  │ Subscribe polls EventStore every 25 ms
  ▼
Harness (per-session coordination, A3 completed)
  ├─ IPC dispatch, session attach, provider selection, run/cancel
  ├─ tool dispatch, approval lookup и redaction
  └─ Mock or concrete OpenAI provider
  ▼
AgentRuntime + EventStore
  ├─ append-only typed events, deterministic projection
  ├─ SQLite WAL / in-memory store
  ├─ copied-event fork, no shared-prefix DAG
  ├─ DeferredEffect storage (A2)
  └─ AdmittedOperation enforcement (A1)
```

Separate, non-IPC-reachable: process execution (A1 protected), PTY stub, SSH profile/approval storage, tmux stub, SFTP stub, ACP gateway, GPUI CI preview.

### TARGET

```
Disposable clients: TUI | Zap adapter | CLI | GPUI | IDE | ACP
  │ typed SDK; no direct store/core ownership
  ▼
impetusd
  ├─ session manager: per-session coordination (✅ A3)
  ├─ agent runtime: lifecycle, budgets, interrupt/cancel
  ├─ provider registry + deterministic model router
  ├─ tool/effect orchestrator
  │ trusted origin (✅ A2) → normalized effect → policy
  │ → sandbox → admitted operation (✅ A1) → capability
  │ → execution → durable event
  ├─ artifact/checkpoint/context services
  └─ append-only EventStore: single durable truth
       └─ cursor backfill then ordered push
```

## Component Status

| Component | Current | Target | Confirmed problem | Priority |
| --- | --- | --- | --- | --- |
| Daemon | Unix socket, 25ms poll | Push subscription | Subscribe polls EventStore | **P1 (B1)** |
| Harness | ✅ Per-session (A3) | Thin facade | Attachment/detail placeholders | P1 (B2) |
| Runtime | ✅ DeferredEffect (A2) | Session runtime | No IPC integration yet | P1 (B1) |
| EventStore | SQLite WAL, thread-safe | Event store | Fork copies events (no shared DAG) | P2 (E) |
| Provider | Mock or concrete OpenAI | Registry/router | **No ModelProvider trait** | **P1 (C1)** |
| Budget | In-memory BudgetChecker | Durable budgets | Not persisted | P2 (C2) |
| Client | Raw IpcResponse match | Typed SDK | **Clients pattern-match wire enum** | **P1 (B1)** |
| CLI | Reference client | Disposable | No typed SDK | P2 (after B1) |
| Zap adapter | 100ms poll | Push subscription | **Repeated wakeups** | **P1 (B1)** |
| GPUI | Direct core import | Via impetus-client | Bypasses client seam | P2 (after B1) |
| Approval/effect | ✅ DeferredEffect (A2) | Autonomy guard | IPC integration pending | P1 (B1) |
| Origin | ✅ Server-side (A2) | Trusted boundary | ✅ IPC tools = User | Done |
| Process/sandbox | ✅ AdmittedOperation (A1) | Sandbox broker | ✅ Type-level enforcement | Done |
| Remote | DTOs + stores | Real executor | Stubs only | P3 (F) |
| Artifacts | FNV-1a in-memory | Durable SHA-256 | **Metadata disappears on restart** | **P1 (B2)** |
| ACP | External JSON-RPC | ACP adapter | No Harness integration | P2 |

## Top Architectural Problems

**Resolved (5 из 10):**
- ✅ #1: Global Harness lock (A3)
- ✅ #2: ProcessExecution bypass (A1)
- ✅ #4: Approval continuation (A2)
- ✅ #5: IPC origin hardcoded (A2)
- ✅ #10: Roadmap docs overstated (v0.6 gate met, docs corrected)

**Remaining (priority order):**
1. **#8: Daemon/Zap/memory-client poll events** (25ms/100ms/10ms) → **B1**
2. **#9: Provider selection concrete in Harness** (no trait) → **C1**
3. **#7: Attachment/detail endpoints placeholders** → **B2**
4. #3: ProcessSpawn workspace scope → future
5. #6: ACP raw credentials → future (isolated, low priority)

## Highest-ROI Changes

1. **B1: Event-driven subscription** (убрать 40+ wakeups/sec per subscription)
2. **B1: Typed client SDK** (domain results, не IpcResponse enum)
3. **C1: Provider trait + registry** (убрать concrete enum)
4. **B2: Durable artifact metadata** (SHA-256, survives restart)
5. C2: Durable budgets (persist cost tracking)
6. F: Real remote executor (SSH/SFTP/PTY/tmux через proven seam)

## Implementation Status

**Implemented and reusable:**
- ✅ Durable events (SQLite WAL, migration, versioned schema)
- ✅ Deterministic projection
- ✅ IPC capability negotiation
- ✅ State survives client disconnect
- ✅ OpenAI-compatible streaming с secret redaction
- ✅ Bounded read-only artifacts
- ✅ Logical policy/exact approval
- ✅ AdmittedOperation type-level enforcement (A1)
- ✅ Server-side origin derivation (A2)
- ✅ DeferredEffect storage (A2)
- ✅ Per-session coordination (A3)

**Partial:**
- Subscription transport (polls, needs push)
- Typed client (raw enum matching)
- Provider health/budget (in-memory)
- Compaction (signals only, no real cache telemetry)
- Fork (copies events, no shared DAG)
- Approval DTOs (detail empty)
- ACP integration (isolated, no Harness events)
- Zap display (structured blocks, но polls)

**Absent:**
- Provider registry/router (concrete enum)
- Durable cost budgets (in-memory only)
- Real compaction/cache metrics
- Shared DAG/checkpoints
- Resume-after-approval via IPC
- Production Seatbelt broker
- Real SSH/SFTP/PTY/tmux executor
- Swarm/profiles/learning
- Reproducible benchmarks

## Updated Phased Roadmap

| Phase | Narrow outcome | Gate | Status |
| --- | --- | --- | --- |
| A0 | Truthful audit and status docs | Audit + verify baseline | ✅ Done |
| A1 | Safe local execution authority | No public spawn without admission | ✅ Done |
| A2 | Origin and approval continuation | Server-side origin, deferred storage | ✅ Done |
| A3 | Per-session coordination | Independent sessions concurrent | ✅ Done |
| **B1** | **Typed client + push subscription** | **No poll loop, reconnect cursor** | **🚧 Next** |
| B2 | Complete existing DTOs | Attachment/diff/detail or absent | Planned |
| C1 | Provider registry/metadata | One interface, no concrete branch | Planned |
| C2 | Router + durable budgets | Rules fallback, persisted cost | Planned |
| D | Context efficiency | Deterministic reducers, cache metrics | Planned |
| E | Advanced sessions | Shared-prefix DAG, checkpoints | Planned |
| F | Remote executor | Real SSH/SFTP/PTY/tmux through seam | Planned |
| v0.7 | MVP UI | End-to-end smoke | Planned |

## Do Not Build Now

- Custom terminal renderer, Electron/WebView
- Swarm, learning, SOUL/profile hierarchy
- Remote/mobile control
- LSP/MCP eager indexing
- Shared-prefix DAG (до фазы E)
- Multi-provider routing (до C1/C2)

Eager polling, full-history projection, copied forks, unbounded output/context создают noticeable cost. Event-driven subscriptions, per-session coordination (✅ done), bounded artifact reads, deterministic reduction, lazy services снижают его.

## Test Coverage

- **247 workspace tests** (baseline commit 509aab5)
- Unit tests: policy, approval, budget, effects, capabilities
- Integration tests: process (12), remote (26), SSH, PTY, tmux, SFTP
- Regression tests: A1 admission, A2 origin/deferred, A3 concurrency
- Policy replay tests
- Fail-closed sandbox tests

## Next Step: B1

**Target:** Event-driven subscription + typed client SDK.

**Impact:** Убирает 40+ wakeups/sec per subscription, даёт typed domain methods клиентам.

**Scope:**
- Event store cursor backfill + notification
- Typed client methods (не IpcResponse enum)
- Daemon push delivery (не poll)
- Zap adapter reconnect с cursor
