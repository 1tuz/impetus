# Roadmap

Canonical product path. Инварианты — [ARCHITECTURE.md](../ARCHITECTURE.md);
история поставки — [IMPLEMENTATION_HISTORY.md](IMPLEMENTATION_HISTORY.md);
исполнимые задачи — [TODO.md](../TODO.md).

## FOUNDATION — current

- Durable events и SQLite WAL.
- Crate split `impetus` (client) / `impetusd` (daemon) / `impetus-core` (in progress; docs/tooling cleanup pending).
- Versioned local IPC, `HarnessClient`.
- Safety, capability, sandbox, approval, secret-reference base.
- `ModelProvider` и `ProviderRegistry` foundations.
- Basic copied-event fork и compaction/budget primitives.
- First Agent Loop / Tool Orchestrator slice.

## MODULE RUNTIME / EXTENSIBILITY FOUNDATION

Ранняя архитектурная фаза — **до** массового внедрения конкретных integrations.

**Gate:**

- typed service contracts (не hard deps AgentLoop → concrete backends);
- `ServiceRegistry` / `ModuleRegistry`;
- `ModuleDescriptor` shape;
- capability negotiation и probing (не только version compare);
- lifecycle: discover, probe, start, health, stop;
- compatibility matrix (harness protocol, contracts, platforms);
- permissions model;
- execution semantics: `read_only | idempotent | mutating | non_replayable`;
- safe fallback policies;
- `UnknownOutcome` rule (no auto-retry mutating/non-replayable on alternate backend);
- external-module isolation (process + IPC preferred);
- Extension Compatibility Adapter foundation + canonical internal types;
- partial import (`SUPPORTED | PARTIAL | UNSUPPORTED | INCOMPATIBLE`).

**Не в gate:** marketplace, plugin manager UI, arbitrary dynamic library ABI.

## BINARY TOPOLOGY & DIAGNOSTICS

**Target:**

- однозначные роли: `impetus` = CLI/TUI client, `impetusd` = daemon;
- release/install ship оба binary;
- client auto-discovers / safely spawns `impetusd`;
- `impetus doctor` и `impetus doctor --json` (typed, redacted, remediation);
- `impetus components` introspection (list/status/health; update later).

## AGENT RUNTIME

### Agent Loop

**Target:**

```text
Model → Tool Orchestrator → Tool request → Effect normalization
      → Safety / Policy / Sandbox → Execution → Observation → Model
```

Отдельная подсистема, не деталь provider implementation.

### Tool Orchestrator

**Target:** structured tool lifecycle, normalized effects, durable observations,
explicit safety admission.

### Model Router

**Current:** provider abstraction, registry foundation, direct provider path.

**Target:** route by complexity, capability, health, cost, latency, privacy,
context, prompt cache, budget, reasoning. Policies: `local-first`, `free-first`,
`balanced`, `quality-first`. Escalation light/local → strong/cloud через
minimal sanitised request; sensitive repo context не уходит в облако по умолчанию.

### Durable budgets

**Target:** per-session steps, calls, tokens, cost, time; rate limits; router
feedback.

## OUTPUT OPTIMIZATION

**Target:**

```text
Execution → Raw Observation → Output Optimization
  ├─ native structured observations (Test/Diff/Search/Pipeline)
  ├─ builtin reducer
  ├─ RTK (optional, probed, replaceable)
  └─ bounded raw + ArtifactRef
```

RTK не обязателен; removable без изменения Agent Loop.

## CONTEXT INTELLIGENCE

### Token / Context Optimizer

**Target:** stable prefix, prompt cache, shared fork/subagent prefix, delta
context, deterministic reducers, artifact store, HOT/WARM/COLD, lazy
tools/MCP/instructions, telemetry. Large paste → ArtifactRef, не giant IPC JSON.

### Instruction model

**Current:** scoped deterministic instruction resolution.

**Target:** task-aware lazy instruction/skill selection. SOUL, AGENTS,
conventions, guides, skills — без расширения permissions.

### Repo Intelligence

**Target:** Tree-sitter map, symbols/imports, git diff, ranked token-budgeted
context, lazy LSP.

## SESSIONS / ORCHESTRATION

### Session DAG and checkpoints

**Current:** basic fork with copied history.

**Target:** parent/fork, shared prefix, checkpoints, restore/revert, branch-aware
sessions.

### Interrupt, pause, resume

**Target:** durable control states; explicit unknown outcomes across disconnects.

### Swarm

**Target, post-MVP:** isolated subagent sessions; compact worker results; not
automatic for every task.

## AGENT BEHAVIOR

### Profiles and memory

**Target:** profiles/SOUL, durable memory, scoped instructions.

### Self-Repair

**Target:** Event Log → failure fingerprint → retry guard → lesson proposal.
Cannot change safety, sandbox, credentials, or core code automatically.

## CLIENTS

### Standalone Harness TUI

**Target:** `cd project && impetus` — first-class CLI/TUI. JCode = UX reference
only; Ratatui/Crossterm baseline. Bracketed paste + large-paste artifact flow.
Audit: [TUI_REFERENCE.md](TUI_REFERENCE.md).

### Zap backend integration

**Target MVP:** discover, Connect/Authorize, status, backend selection, forward
requests. Zap owns UI. Existing adapter — historical baseline only.

## EXTENSION ECOSYSTEM

**Target:** compatibility adapters for Skills, MCP, Agent Plugins, Claude/Codex/Cursor
extensions; canonical internal representation; `doctor` shows partial compatibility.
Upstream spec check before locking format.

## REMOTE

**Target:** controlled SSH, PTY, tmux, SFTP through policy/approval/events.
Current models/stubs ≠ completed flow.

## PLATFORM / DISTRIBUTION

**Current focus:** macOS Apple Silicon.

**Target:** Ubuntu 24.04 x86_64, later Linux ARM64 / Intel macOS; checksums, curl
installer, clean-machine smoke, update/uninstall docs.

## Not now

- Separate native GUI app.
- Custom terminal emulator / ANSI renderer without proven need.
- Local HTTP UI, Electron/WebView, Node runtime in harness.
- Cloud sync, marketplace, multi-user auth.
- Automatic permission/credential/safety-policy expansion.

## Readiness rule

Feature ready только с proportionate tests, runtime smoke where applicable,
documented trust boundary, explicit evidence gate is met.
