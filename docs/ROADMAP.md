# Roadmap

Canonical product path. Current truth is in
[ARCHITECTURE.md](../ARCHITECTURE.md); historical delivery detail is in
[IMPLEMENTATION_HISTORY.md](IMPLEMENTATION_HISTORY.md).

## FOUNDATION — current

- Durable events and SQLite WAL.
- Daemon/client split with versioned local IPC.
- Safety, capability, sandbox, approval, and secret-reference base.
- `ModelProvider` and `ProviderRegistry` foundations.
- Basic copied-event fork and compaction/budget primitives.

## AGENT RUNTIME

### Agent Loop

**Target:** first-class autonomous loop:

```text
Model → Tool Orchestrator → Tool request → Effect normalization
      → Safety / Policy / Sandbox → Execution → Observation → Model
```

It is a distinct subsystem, not provider implementation detail.

### Tool Orchestrator

**Target:** structured tool lifecycle, normalized effects, durable observations,
and explicit safety admission.

### Model Router

**Current:** provider abstraction, registry foundation, and direct provider path.

**Target:** route by task complexity, capability, provider health, cost,
latency, privacy, context size, prompt-cache characteristics, remaining budget,
and reasoning need. Policies: free-first, balanced, quality-first, optional
local-first. Escalate cheap/local/free → standard → strong only when justified.

### Durable budgets

**Target:** per-session/agent steps, calls, tokens, cost, and time state;
rate-limit scheduling and router feedback.

## CONTEXT INTELLIGENCE

### Token / Context Optimizer

**Target:** stable prompt prefix, prompt cache, shared fork/subagent prefix,
delta context, deterministic reducers, artifact store, HOT/WARM/COLD data,
lazy tools/MCP/instructions, and token/cost/cache telemetry. No unmeasured
savings claims.

### Instruction model

**Current:** scoped deterministic instruction resolution.

**Target:** task-aware lazy instruction and skill selection. SOUL expresses
persona; AGENTS broad rules; conventions declarative project rules; guides
factual/process knowledge; skills procedural workflows. None can grant
network, sudo, SSH, credentials, sandbox scope, or approval.

### Repo Intelligence

**Target:** Tree-sitter map, symbols/imports/dependencies, git diff and recent
files, ranked token-budgeted context, and lazy/on-demand LSP—not an always-on
heavy service.

## SESSIONS / ORCHESTRATION

### Session DAG and checkpoints

**Current:** basic fork with copied event history and compaction/budget
primitives.

**Target:** parent/fork relations, shared history/prefix where useful,
checkpoints, restore/revert, and branch-aware agent sessions.

### Interrupt, pause, resume

**Target:** durable control states and explicit unknown outcomes across client
disconnects.

### Swarm

**Target, post-MVP:** isolated subagent sessions with own
branch/checkpoint/budget/profile/scoped capabilities; shared repo index/cache
where appropriate; conflict detection; compact worker result to parent. Swarm
is not automatic for every task.

## AGENT BEHAVIOR

### Profiles and memory

**Target:** profiles/SOUL, durable memory, and scoped instruction selection
without expanding permissions.

### Self-Repair

**Target flow:**

```text
Event Log → Failure Detector → Failure Fingerprint → Retry Guard
→ Candidate Lesson → Validate / Deduplicate → Memory / Convention / Guide / Skill proposal
```

Signals include repeated tool failure, failed tests, user correction, revert,
safety denial, abandoned approach, and explicit negative feedback. Equivalent
failure of the same tool/input/state is `RETRY_BLOCKED`: change strategy.
Self-Repair cannot change safety policy, sandbox, credentials, permissions, or
core executable code automatically.

## CLIENTS

### Standalone Harness TUI

**Target:** `cd project && impetus` as first-class CLI/TUI for Terminal.app,
iTerm, SSH, Linux, and environments without Zap. Evaluate jcode reuse first;
choose a thin Rust framework only when justified.

### Zap backend integration

**Target MVP:** discover local Impetus; Connect/Authorize; show
connected/disconnected state; choose Impetus as agent backend; forward agent
requests. Zap owns its UI. The existing adapter is historical/experimental and
is not a target renderer/status-bar/Blocks protocol.

## REMOTE

**Target:** controlled SSH, PTY, tmux, and SFTP capabilities through policy,
approval, and durable events. Current models/stubs are not a completed remote
agent flow. Later Android/remote control follows this boundary.

## PLATFORM / DISTRIBUTION

**Current focus:** macOS Apple Silicon.

**Target:** Ubuntu 24.04 x86_64, later Linux ARM64 and Intel macOS when needed;
credential abstraction, release binaries, checksums, curl installer,
clean-machine smoke, update, and uninstall docs. Do not present a developer
checkout as final product installation.

## Not now

- A separate native GUI application.
- A custom terminal emulator or ANSI renderer without proven need.
- Local HTTP UI, Electron/WebView, Node runtime, cloud sync, marketplace, or
  multi-user auth.
- Automatic permission expansion, credentials, or safety-policy changes.

## Readiness rule

A feature is ready only with proportionate tests, runtime smoke where
applicable, documented trust boundary, and explicit evidence that current code
meets its gate.

