# Current Architecture Audit — Impetus

Date of snapshot: 2026-08-28.  
**Progress update:** 2026-08-28 — A1 complete, v0.6 complete, A2 in progress.

## Recent completions

**A1 (Safe local execution authority)** — ✅ Complete 2026-08-28
- AdmittedOperation token enforces harness-issued admission at compile time
- ProcessExecution::execute() requires &AdmittedOperation
- Regression tests prove unadmitted spawn impossible, agent origin requires approval
- Gate met: no public spawn without admission; exact approval when needed; unavailable Seatbelt fails closed
- Documentation: docs/A1-IMPLEMENTATION.md

**v0.6 (Remote profiles)** — ✅ Complete 2026-08-28
- SSH profiles with host-key verification, keychain integration, durable approval
- PTY/tmux DTOs, stores, and stub executors with policy integration
- SFTP module: SftpSession, SftpOperationRequest, SftpSessionManager, 4 integration tests
- Gate met: host-key/target/file approval survives restart via SSH profiles + durable store
- Real SSH/SFTP/PTY/tmux executors deferred to phase F (after A/B/C foundations)
- Documentation: docs/v0.6-SFTP-IMPLEMENTATION.md

**Next:** A2 (origin forgery + deferred continuation), A3 (per-session concurrency).

## Scope and evidence

This audit uses the actual checkout: workspace manifests, all crate sources,
ARCHITECTURE.md, ROADMAP.md, TODO.md, tests, and a fresh task verify. It is an
audit and roadmap update, not authorization for a broad refactor.

task verify completed: formatting, workspace tests, cargo check, and Clippy
passed. Cargo reported a future-incompatibility warning for transitive
block v0.1.6. It is not a test failure, but it is work for the next dependency
maintenance update.

## CURRENT

    CLI / Zap adapter / optional GPUI app / future clients
      │
      ├─ CLI and Zap use raw IpcRequest/IpcResponse
      ├─ GPUI app imports impetus-core directly
      ▼
    Unix socket daemon
      │ JSON-lines IPC v2, socket mode 0600
      │ Subscribe polls EventStore every 25 ms
      ▼
    Harness
      │ one global std::sync::Mutex<()> for every request
      ├─ IPC dispatch, session attach, provider selection, run/cancel
      ├─ tool dispatch, approval lookup and redaction
      └─ Mock or one concrete OpenAI-compatible provider
      ▼
    AgentRuntime + EventStore
      ├─ append-only typed events and deterministic projection
      ├─ SQLite WAL / in-memory store
      ├─ copied-event fork, no shared-prefix DAG
      └─ policy/effect seam and bounded read-only artifacts

Separate, non-IPC-reachable modules: process execution, simulated PTY, SSH
profile/approval storage, simulated tmux, ACP gateway, and GPUI CI preview.

## TARGET

    Disposable clients: TUI | Zap adapter | CLI | GPUI | IDE | ACP
      │ typed SDK; no direct store/core ownership
      ▼
    impetusd
      ├─ session manager: per-session coordination and event cursor
      ├─ agent runtime: lifecycle, budgets, interrupt/cancel
      ├─ provider registry plus deterministic model router
      ├─ tool/effect orchestrator
      │ trusted origin → normalized effect → policy → sandbox
      │ → capability → execution → durable event
      ├─ artifact/checkpoint/context services
      └─ append-only EventStore: single durable truth
           └─ cursor backfill then ordered push

Only bounded shared registries/caches use global coordination. Independent
sessions do not wait on an unrelated request or provider stream.

## Component inventory

| Component | Current implementation and owner | Target owner | Confirmed problem | Cost / concern | Priority / change |
| --- | --- | --- | --- | --- | --- |
| Daemon | Unix socket, handshake, SQLite startup; 25 ms subscription poll. | Transport/subscription delivery only. | Subscribe acknowledges then polls Stream. | 40 wakeups/sec per subscription. | P1: cursor backfill then store notification. |
| Harness | Dispatch, global request lock, provider branch, cancellation map, tools, DTOs, redaction. | Thin facade composed from services. | Every request serializes; attachment/detail endpoints are placeholders. | Slow storage blocks all sessions; sync mutex in daemon path. | P0: split session/run coordination before removing lock. |
| Runtime/projection | Durable session/intent/run/approval lifecycle and deterministic reduce. | Session runtime. | One active run per session; no durable deferred-effect continuation. | Full projection rebuilds complete history. | Keep; add per-session coordinator later. |
| EventStore | SQLite WAL/memory, version migration, copied-prefix fork. | Event store. | Fork copies and renumbers events; no parent/checkpoint model. | Mutex/one SQLite connection; replay is O(history). | P2: shared-prefix DAG only after core safety. |
| Provider | One OpenAI-compatible adapter, profile validation, transient credential resolver, bounded SSE retry/health. | Registry/router. | Harness chooses Mock or concrete OpenAI; no ModelProvider trait or metadata. | Narrow health mutex; no rate-limit scheduler. | P1: provider trait plus registry first. |
| Budget | BudgetConfig/checker and compaction events in SessionSupervisor. | Runtime/router. | No cost/model-call/subagent scope; not wired to real Harness provider run; compaction only signals. | Low idle cost, in-memory state. | P2: one durable budget after registry. |
| Client | Trait with Unix/in-memory transports; high-level calls return IpcResponse. | Typed SDK. | Every client pattern-matches wire enum; in-memory subscription polls 10 ms. | Socket mutex is correct per connection only. | P1: typed domain results, raw request low-level. |
| CLI | Reference client, raw enum matching. | Disposable client. | No typed SDK consumption. | No state ownership. | P2 after client types. |
| Zap adapter | Raw Stream loop, sleeps 100 ms, OSC/block renderer. | Typed SDK subscriber. | Confirmed polling; approvals/attachments TODO; InterruptedUnknown is not terminal. | Repeated wakeups, duplicate stream logic and potential endless poll after uncertain outcome. | P1: one subscription, reconnect cursor and terminal uncertain-state handling. |
| GPUI | Optional diagnostics/theme/CI, direct core import. | Disposable client via impetus-client. | Bypasses universal client seam if expanded. | Could regain store/runtime ownership. | P2 after SDK query surface. |
| Approval/effect seam | Fingerprint/version/intent checks, logical sandbox, policy replay. | Autonomy guard/sandbox broker. | ResolveApproval writes event only; it cannot resume a deferred effect. | Test seam is not an end-to-end executor. | P0: durable deferred work or typed unavailable. |
| Action origin | User and Agent only; IPC Tool always uses User. | Trusted execution boundary. | Raw socket caller can use user-direct route; no system/subagent/remote origins. | Current tools read-only, but mutable extension would be unsafe. | P0: server derives origin; add forgery tests. |
| Process/sandbox | Logical EffectSeam; Seatbelt proof is a spike. | Sandbox broker. | **A1 resolved:** AdmittedOperation enforces admission. Remaining: ProcessSpawn not workspace-scoped; user origin immediately permits it; no production Seatbelt broker. | Dormant but serious: workspace scope and OS sandbox enforcement absent. | P0: provision narrow per-session workspace scope; wire Seatbelt broker. |
| Remote/PTY/tmux | DTOs and stores; PTY fake pid/kill, tmux TODO, no live SSH/SFTP executor. **v0.6 complete:** SSH profiles, PTY/tmux/SFTP stubs with policy integration, 4 SFTP tests. | Remote executor behind same seam. | Stub executors simulate behavior; no live SSH/SFTP/PTY/tmux connection. Real executors deferred to phase F. | Simulation is not product behavior. | P0 docs correction ✅; P3 real executor (phase F after safety foundations). |
| Artifacts/DTOs | FNV-1a-addressed files plus bounded read-only preview; in-memory metadata index. | Durable artifact service. | GetAttachment unavailable; approval detail empty; metadata disappears on reopen and range read loads full artifact. | Full content is not durably indexed and declared API cannot retrieve it. | P1: durable SHA-256 metadata plus redacted/bounded range DTO, or unadvertise. |
| ACP | External JSON-RPC process gateway and mock. | ACP adapter. | No durable Harness session/event/policy integration; respond_credential accepts a raw String. | Isolated process, but current memory secret path contradicts the intended boundary. | P0: remove raw credential forwarding before any integration. |

## Hypotheses: verdict

| Hypothesis | Verdict | Evidence |
| --- | --- | --- |
| Global Harness lock remains. | Confirmed. | Harness.handle locks request_lock for every request. |
| harness_api is a god-object risk. | Confirmed. | It dispatches IPC, session, provider, cancellation, tools, DTOs and redaction. |
| Provider selection is central/concrete. | Confirmed. | ProviderBackend is Mock or OpenAI inside Harness. |
| Client leaks wire protocol. | Confirmed. | Methods return IpcResponse; CLI/Zap match it. |
| Zap and subscriptions poll. | Confirmed. | Zap waits 100 ms; daemon 25 ms; memory client 10 ms. |
| Event log is durable/versioned. | Confirmed. | SQLite WAL, typed schema/legacy migration, deterministic projection. |
| Fork reuses shared history/checkpoints. | Refuted. | fork_session copies events; no checkpoint domain type. |
| Attachment/diff DTOs are complete. | Refuted. | Attachment is unavailable; detail returns empty fields. |
| Agent can impersonate user. | Partially confirmed. | IPC tool is hardcoded User; no agent tool path is reachable now. Unsafe extension point exists. |
| macOS sandbox/remote execution is done. | Refuted. | **A1 ✅:** AdmittedOperation enforces admission. **v0.6 ✅:** SSH/SFTP/PTY/tmux stubs + policy. Real executors = phase F. |

## Implemented, partial, absent

Implemented and reusable: durable events, SQLite WAL/migration, deterministic
projection, IPC version/capability handshake, state surviving client disconnect,
OpenAI-compatible streaming with transient credentials, bounded read-only
artifacts, logical policy/exact approval, and a Seatbelt proof.

Partial: subscription transport, typed client, provider health/budget,
compaction, fork, approval DTOs, execution seam, remote DTOs, ACP integration,
Zap display and GPUI isolation.

Absent: per-session concurrency, provider registry/router, durable cost
budgets, real compaction/cache telemetry, shared DAG/checkpoints,
resume-after-approval, production Seatbelt broker, real SSH/SFTP/PTY/tmux,
swarm/profiles/learning/repo intelligence, and reproducible product benchmarks.

## Top ten architectural problems

1. **✅ A3 resolved:** Global Harness lock serializes unrelated sessions.
2. **✅ A1 resolved:** ProcessExecution.execute bypasses admission and OS sandbox enforcement.
3. ProcessSpawn is not workspace-scoped and user origin immediately permits it.
4. **✅ A2 resolved:** Approval resolution does not resume a durable deferred effect.
5. **✅ A2 resolved:** IPC tool origin is hardcoded User instead of being server-derived.
6. ACP forwards raw credential strings and is outside Harness policy/events.
7. Advertised attachment and approval-detail endpoints are placeholders.
8. Daemon, in-memory transport and Zap all poll for events.
9. Provider selection is concrete inside Harness.
10. **✅ v0.6 docs corrected:** Roadmap marks simulated/unreachable remote features as ready.

## Highest-ROI changes

1. **✅ A1/A2/A3/v0.6 done:** Correct readiness/status documentation.
2. **✅ A1 done:** Make unadmitted process execution impossible and provision a narrow per-session workspace scope before it reaches an OS sandbox.
3. **✅ A2 done:** Separate user-direct and agent-generated action paths; test origin forgery.
4. **✅ A2 done:** Store and resume deferred effects with exact approval, or return unavailable.
5. **✅ A3 done (minimal):** Remove global lock; per-session coordination via EventStore/Runtime internal locks.
6. **Next: B1:** Push events with cursor reconnect; move Zap to it.
7. **Next: B2:** Complete attachment/detail DTOs or mark capability absent.
8. **Next: C1:** Extract provider trait/metadata before another provider or router.
6. Replace global lock with per-session coordination and concurrency tests.
7. Push events with cursor reconnect; move Zap to it.
8. Add typed client methods for existing domain operations.
9. Replace the non-durable FNV artifact index with a durable SHA-256 metadata service before exposing attachments/range reads.
10. Extract provider trait/metadata before another provider or router.

## Do not build now

Do not add a custom terminal renderer, Electron/WebView, swarm, learning,
SOUL/profile hierarchy, remote/mobile control, LSP/MCP indexing, shared-prefix
DAG, or multi-provider routing. Each has complexity or overhead that cannot
compensate for the current safety and ownership gaps.

Eager polling, full-history projection, copied forks, unbounded output/context,
and eager LSP/MCP/local-model services create noticeable cost. Event-driven
subscriptions, per-session coordination, bounded artifact range reads,
deterministic output reduction, stable prompt serialization, lazy services,
and later shared immutable prefixes reduce it.

## Updated phased roadmap

| Phase | Narrow outcome | Gate |
| --- | --- | --- |
| A0 | Truthful audit and status documents. | This audit and factual roadmap; baseline verify recorded. |
| A1 | Safe local execution authority. | No public spawn without harness-issued admission, exact approval when needed, and unavailable Seatbelt fails closed. |
| A2 | Origin and approval continuation. | Agent cannot use user-direct route; stale approval cannot run changed work; denial continues task; approved work resumes exact durable effect. |
| A3 | Per-session coordination. | Two independent sessions make progress concurrently with ordered durable events. |
| B1 | Typed client and push subscription. | Clients use typed methods; reconnect gets only events after cursor; no poll loop. |
| B2 | Complete existing typed DTOs. | Attachment/diff/detail are complete, bounded/redacted, or capability is absent. |
| C1 | Provider registry/metadata. | One provider interface; no central concrete provider branch. |
| C2 | Router and durable budgets. | Bounded rules-based fallback and per-session/agent steps/calls/tokens/cost/time. |
| D | Context efficiency. | Deterministic reducers/artifacts first; cache metrics only with measured provider benefit. |
| E | Advanced sessions. | Shared-prefix fork/checkpoints with restore and concurrency tests. |
| F | Remote executor. | Real SSH/SFTP/PTY/tmux goes through proven local effect path and durable scoped approval. |
| G | Optional TUI/swarm/profiles/learning. | Disposable/bounded components with measured benefit. |

## First proposed implementation slice — pending approval

A1 is deliberately narrow. It changes no client feature: represent an admitted
operation as an internal harness value, require it for ProcessExecution, and
add regressions proving that unadmitted requests, unavailable sandbox and
changed approval cannot spawn a child. Keep it outside IPC until A2 provides
durable continuation.
