# Roadmap

Canonical product path. Invariants are in [ARCHITECTURE.md](../ARCHITECTURE.md);
executable tasks are in [TODO.md](../TODO.md).

## FOUNDATION — current

- Durable events and SQLite WAL.
- Crate split `impetus` (client) / `impetusd` (daemon) / `impetus-core`.
- Versioned local IPC, `HarnessClient`.
- Safety, capability, sandbox, approval, secret-reference base.
- `ModelProvider` and `ProviderRegistry` foundations.
- Basic copied-event fork and compaction/budget primitives.
- Attachment/diff/detail DTOs with bounded **ephemeral/in-memory** backing (not durable `ArtifactStore`).
- Agent Loop / Tool Orchestrator vertical slice: read tools execute through
  policy and sandbox; writes and shell commands require exact user approval,
  then resume with durable observations. Large read outputs are stored in the
  durable content-addressed artifact store and referenced from the event log.

## MODULE RUNTIME / EXTENSIBILITY FOUNDATION

Ранняя архитектурная фаза — **до** массового внедрения конкретных integrations.

**Gate:**

- typed service contracts (не hard deps loop/scheduling → concrete backends);
- replaceable `AgentLoopStrategy` / `AgentScheduler` behind contracts (Kernel safety/durability pipeline unchanged);
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

**Current:** a working single-session vertical slice parses model tool calls,
executes read-only tools, defers mutating tools for exact approval, and resumes
the model from durable observations. Tool artifacts persist in the stable data
root. Native provider tool-call protocols, web research, and model routing
remain future work.

**Target:**

```text
Model → Tool Orchestrator → Tool request → Effect normalization
      → Safety / Policy / Sandbox → Execution → Observation → Model
```

Отдельная подсистема, не деталь provider implementation.

### Tool Orchestrator

**Current:** structured tool lifecycle, normalized effects, durable observations,
and explicit safety admission for `list_files`, `read_file`, `search`,
`write_file`, `edit_file`, and `bash`/`shell`/`exec`.

**Target:** provider-native structured tool calls, bounded execution, and the
remaining tool families.

### Model Router

**Current:** provider abstraction, registry foundation, direct provider path.

**Target:** route by capability, health, cost, latency, privacy,
context, prompt cache, budget, reasoning. Policies: `local-first`, `free-first`,
`balanced`, `quality-first`. Technical fallback between providers when unavailable.
Sensitive repo context не уходит в облако по умолчанию.

### Durable budgets

**Target:** per-session steps, calls, tokens, cost, time; rate limits; router
feedback.

## WEB / INTERNET RESEARCH

**Target:** first-class Agent Runtime capability — native Rust HTTP search/fetch
without mandatory cloud APIs, Python, or Docker. JCode
([1jehuang/jcode](https://github.com/1jehuang/jcode)) implementation reference;
upstream source audit before implementation lock.

**Base (required path):**

```text
DuckDuckGo HTML → Bing HTML fallback → optional SearchBackend (e.g. SearXNG)
WebFetch: bounded HTTP → extract → WebObservation → ArtifactRef if large
```

**Optional:** `BrowserProvider` for JS-heavy sites; external search API backends.

**Gate:**

- `WebResearchService` + `WebSearchService` / `WebFetchService` contracts;
- `SearchBackend` + `BrowserProvider` via Module Runtime (loop agnostic of vendor);
- typed `WebObservation`, provenance/citations, ArtifactStore integration;
- fine-grained web capabilities (`web.read`, `web.search`, `web.submit`, …);
- SSRF protection (URL, DNS, redirects, final destination);
- research loop in harness (search → fetch → follow → cite);
- `doctor` web section with per-backend health and degraded fallback semantics;
- JCode audit: websearch, webfetch, browser tool, Browser Provider Protocol.

**Не в base gate:** Chromium/Playwright/Node in core; mandatory Tavily/Exa/etc.

Исполнимые пункты — [TODO.md](../TODO.md) § WEB / INTERNET RESEARCH.

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
context, deterministic reducers, **durable** artifact store (current attachment
backing is ephemeral/in-memory only), HOT/WARM/COLD, lazy
tools/MCP/instructions, telemetry. Large paste → `ArtifactRef`, не giant IPC JSON.

### Instruction model

**Current:** scoped deterministic instruction resolution.

**Target:** task-aware lazy instruction/skill selection. SOUL, AGENTS,
conventions, guides, skills — без расширения permissions.

### Reference Store

**Current:** YAML-based reference store with:
- `ReferenceService` trait and `YamlReferenceService` implementation
- Partitioned storage (monthly/yearly/by-project sharding)
- Lazy loading with search filters and top-K results
- Privacy-aware (public/internal/private sensitivity)
- Tempo importer vertical slice (CSV → YAML → search → agent context)
- Complete test coverage and working CLI example

**Target:** reference datasets as long-term agent memory. Jira Tempo worklogs,
past decisions, project patterns — queryable context that doesn't bloat prompts.
YAML remains authoritative; derived indexes (if added) stay rebuildable.

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

**Target:** `cd project && impetus` — first-class CLI/TUI. JCode
([1jehuang/jcode](https://github.com/1jehuang/jcode)) = UX reference only;
Ratatui/Crossterm baseline. TUI audit plan: [TUI_REFERENCE.md](TUI_REFERENCE.md)
(audit not started). Bracketed paste + large-paste artifact flow.

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

**Current focus:** macOS Apple Silicon (Keychain for credential references).

**Target:** Ubuntu 24.04 x86_64, later Linux ARM64 / Intel macOS; checksums, curl
installer, clean-machine smoke, update/uninstall docs. **Credentials:** macOS =
Keychain; other OS = corresponding system credential store (e.g. libsecret /
portal — per-platform TBD). Profiles, SQLite, and events hold opaque references
only — never raw secrets on any OS.

## Not now

- Separate native GUI app.
- Custom terminal emulator / ANSI renderer without proven need.
- Local HTTP UI, Electron/WebView, Node runtime in harness.
- Cloud sync, marketplace, multi-user auth.
- Automatic permission/credential/safety-policy expansion.

## Readiness rule

Feature ready только с proportionate tests, runtime smoke where applicable,
documented trust boundary, explicit evidence gate is met.
