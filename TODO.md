# TODO — Impetus Harness

Executable work map. Context: [ARCHITECTURE.md](ARCHITECTURE.md),
[docs/ROADMAP.md](docs/ROADMAP.md).

**Rule:** mark `[x]` only after a working vertical slice, tests, and its gate.
Stubs and placeholder responses do not count as done.

---

## Phase 0 — Foundation (done)

- [x] Headless runtime with SQLite WAL and durable Event Log
- [x] Versioned local IPC via Unix Domain Socket
- [x] `HarnessClient` for Unix transport and in-memory tests
- [x] Durable sessions and client reconnect
- [x] Cursor backfill + push event subscription
- [x] Policy engine with `Deny | Allow | NeedsApproval`
- [x] Typed `Action` with `origin=user|agent` and request ID tracking
- [x] Sandbox integration for shell/process capabilities
- [x] Attachment/diff/detail DTO contracts with bounded ephemeral/in-memory backing (not durable `ArtifactStore`)
- [x] Agent Loop / Tool Orchestrator vertical slice: durable observations, policy-gated read tools, and exact approval/resume for writes and shell commands (see Phase 5)
- [x] `ModelProvider` trait and `ProviderRegistry` foundation
- [x] Crate split: `impetus-core`, `impetusd`, `impetus` (initial)

---

## Phase 1 — Binary topology & diagnostics (early)

Целевая роль имён: `impetus` = user client, `impetusd` = daemon. Crate split
есть; миграция имён/ролей в docs и dev-tooling не завершена.

### Binary topology

- [x] Зафиксировать target roles во всех user-facing docs (`getting-started`, `configuration`, `troubleshooting`, `README.ru`)
- [x] `task daemon` → `cargo run -p impetusd` (не `harness`)
- [x] `task client` → `cargo run -p impetus` (не `cli`)
- [x] `task harness` / `task cli` deprecated aliases
- [x] Release artifact: оба binary с явными ролями в install script help
- [x] `impetus` auto-discovers socket и safely spawns `impetusd` if needed
- [x] Install/uninstall docs: `impetusd` daemon vs `impetus` client

### Doctor

- [x] `impetus doctor` — human-readable diagnostics + remediation
- [x] `impetus doctor --json` — versioned redacted schema for bug reports
- [x] Probe: `impetus`/`impetusd` versions
- [x] Probe: daemon discovery, socket path, permissions
- [x] Probe: IPC handshake and protocol compatibility (`Incompatible` path)
- [x] Probe: daemon readiness
- [x] Probe: Event Store, SQLite WAL, schema/migrations
- [x] Probe: Artifact Store (durable target; flag ephemeral/in-memory attachment backing)
- [x] Probe: sandbox availability (Seatbelt fail-closed)
- [x] Probe: policy and approval subsystem
- [x] Probe: platform credential store accessibility (Keychain on macOS; redacted)
- [x] Probe: ProviderRegistry, providers, model capabilities
- [x] Probe: tools/capabilities registration
- [x] Probe: external agents / ACP adapters
- [x] Probe: optional modules, compatibility adapters, remote capabilities
- [x] Probe: web research (internet access, WebFetch, per-`SearchBackend` health, BrowserProvider, network policy)
- [x] Probe: disk/runtime health
- [x] Partial extension compatibility matrix in doctor output

### Components (introspection)

- [x] `impetus components list`
- [x] `impetus components status` (health, version, compatibility, source)
- [x] Concept: component version/digest lock for reproducibility
- [x] Update/disable flows (design; no marketplace)

---

## Phase 2 — Module Runtime / extensibility foundation

Gate до массовых integrations. См. ROADMAP § MODULE RUNTIME.

- [x] Typed service contracts (decouple loop/scheduling from concrete backends)
- [x] Replaceable `AgentLoopStrategy` and `AgentScheduler` behind contracts (Kernel pipeline unchanged)
- [x] `ServiceRegistry` / `ModuleRegistry`
- [x] `ModuleDescriptor` (id, kind, versions, provides/requires, capabilities)
- [x] Capability negotiation and probing API (not version-only checks)
- [x] Module lifecycle: discover, probe, start, health, stop
- [x] Compatibility: harness protocol, service contracts, platforms, external versions
- [x] Permissions: filesystem, process, network, secrets, remote
- [x] Execution semantics on modules: read_only, idempotent, mutating, non_replayable
- [x] Safe fallback policies per module kind
- [x] `UnknownOutcome` enforcement: no auto-retry mutating/non-replayable on alternate backend
- [x] External module isolation: separate process + versioned IPC + sandbox where applicable
- [x] Tests: module incompatible/degraded/unavailable paths without policy bypass

---

## Phase 3 — Extension compatibility

- [x] Extension Compatibility Adapter layer (design + minimal slice)
- [x] Canonical types: `CanonicalModuleSpec`, `CanonicalSkill`, `Instruction`, `AgentProfile`, `Command`, `McpModule`, `ToolProvider`
- [x] Import capability matrix: `SUPPORTED | PARTIAL | UNSUPPORTED | INCOMPATIBLE`
- [ ] Agent Skills adapter (upstream spec audit first)
- [ ] MCP adapter
- [ ] Agent Plugins adapter
- [ ] Claude Code extensions/plugins adapter
- [ ] Codex extensions/plugins/skills adapter
- [ ] Cursor plugins/rules/skills/agents/commands adapter
- [ ] DeepSeek Harness/Cordis bridge (process adapter, not TS in daemon)
- [ ] `doctor` reports per-package partial compatibility

---

## Phase 4 — Output optimization

- [x] `TestObservation` from `cargo test` (native structured path)
- [x] `DiffObservation` from `git diff`
- [x] `SearchObservation` from repo search
- [x] `PipelineObservation` from CI backends
- [x] Builtin output reducer (token-bounded)
- [x] Bounded raw fallback → `ArtifactRef`
- [x] Full raw output stored as Artifact alongside structured observation
- [x] Migrate tools.rs ArtifactStore to DurableArtifactStore (SHA-256)
- [ ] RTK optional adapter: probe capabilities, not hard dependency

---

## WEB / INTERNET RESEARCH

Core Agent Runtime capability (not optional marketplace plugin). Base: native Rust
HTTP — no mandatory cloud search API, Python, Docker, or SearXNG daemon.
Details: [ARCHITECTURE.md](ARCHITECTURE.md) § Web / Internet Research.

### Contracts & services

- [ ] `WebResearchService` facade contract
- [ ] `WebSearchService` + `SearchBackend` trait (Module Runtime)
- [ ] `WebFetchService` (separate from search)
- [ ] `BrowserService` + `BrowserProvider` contract (optional module)
- [ ] Agent Loop integration via contracts only (no direct DuckDuckGo/Bing deps)
- [ ] Research loop: search → select → fetch → follow links → compare → cite

### WebSearch backends

- [ ] Native `DuckDuckGoHtml` backend (default)
- [ ] Native `BingHtml` fallback backend
- [ ] Optional `SearXNG` `SearchBackend`
- [ ] Optional future API backends (Tavily, Exa, …) as replaceable modules only
- [ ] Fallback chain + degraded health (one backend down ≠ harness unhealthy)
- [ ] Capability probing per backend (not version-only)

### WebFetch

- [ ] Bounded HTTP fetch (timeout, max response size, redirects)
- [ ] MIME detection
- [ ] HTML → clean text / Markdown extraction (links, title)
- [ ] Source URL, timestamp, content hash, truncation
- [ ] Large/full body → `ArtifactStore` → `ArtifactRef`; bounded preview to model

### Observations & context

- [ ] Typed `WebObservation` (search result list + fetch document shapes)
- [ ] Provenance / citation metadata for research loop answers
- [ ] Raw HTML / large content → artifact, not unbounded context

### Safety & policy

- [ ] Fine-grained capabilities: `web.read`, `web.search`, `web.download`, `web.browser`, `web.submit`, `web.upload`
- [ ] Session-level allowance for read-only web vs stricter approval for outbound data (POST, upload, auth actions)
- [ ] SSRF: block localhost, `127.0.0.0/8`, `::1`, private LAN, link-local, metadata endpoints, local services
- [ ] Validate initial URL, DNS resolution, redirect chain, final destination
- [ ] LAN/internal targets — separate capability, not default `web.read`
- [ ] All web ops through Kernel pipeline (policy → sandbox → capability → execution → durable event)

### JCode source audit (web)

Upstream: `https://github.com/1jehuang/jcode` — pin SHA before implementation.

- [ ] Audit `websearch`, `webfetch`, browser tool, Browser Provider Protocol
- [ ] Audit fallback handling, anti-bot detection, output bounding, HTML cleanup
- [ ] Per-area `ADAPT | REIMPLEMENT | SKIP`; attribution if code adapted

### Browser (optional)

- [ ] JCode Browser Provider Protocol as reference (negotiation, health, session ops)
- [ ] Optional Firefox/Chrome/WebDriver/Safari providers (not in mandatory core)
- [ ] No Chromium/Playwright/Node in required harness dependency set

### Doctor

- [ ] Internet access enabled/disabled
- [ ] WebFetch / per-SearchBackend / BrowserProvider health in `impetus doctor`
- [ ] `DEGRADED — web search fallback available` when fallback path works

---

## Phase 5 — Agent runtime (continued)

### Durable budgets & Model Router

- [x] Token and wall-time budget tracking per session (durable)
- [x] Budget enforcement in agent loop
- [x] Model Router: selection rules (capability, health, cost, latency, privacy, cache, budget)
- [x] Router policies: local-first, free-first, balanced, quality-first
- [x] Escalation: local → sanitised cloud request → result back to local agent
- [x] Cost estimation and budget warnings

### Agent loop (real implementation)

The baseline vertical is working. The remaining items harden and extend it.

- [x] Replace `extract_tool_calls()` placeholder with provider-aware parsing
- [x] Wire Tool Orchestrator to real tool execution through policy/sandbox path
- [x] Durable observations from executed tools (not stub responses)
- [x] Large read output uses durable content-addressed artifacts and bounded event previews
- [x] End-to-end slice: model → tool request → execution → observation → model
- [ ] Wire web research tools through `WebResearchService` (when WEB slice lands)

### Agent loop hardening

- [x] Multi-turn conversation state with durable tool result accumulation and approval/rejection resume
- [ ] Streaming response chunking and client sync
- [ ] Error recovery and retry logic (respect `UnknownOutcome` / `RETRY_BLOCKED`)
- [ ] Parallel tool execution where safe (read_only/idempotent only)
- [ ] Cross-session state isolation and cleanup
- [ ] Audit log with redacted tool arguments

---

## Phase 6 — Context & sessions

- [ ] Lazy module/tool/MCP description loading in Context Optimizer
- [ ] HOT/WARM/COLD context tiers
- [ ] Token-budgeted module/tool selection for prompt
- [ ] Session fork/checkpoint without full event duplication (shared prefix)
- [ ] Session DAG: parent/fork, restore/revert, branch-aware sessions
- [ ] Large paste: bracketed paste in TUI
- [ ] Large paste: detection threshold + compact composer display
- [x] Durable `ArtifactStore` (metadata + content survives restart; SHA-256 refs)
- [ ] Large paste: chunked upload to `impetusd` → `ArtifactStore` → `ArtifactRef`
- [ ] Context Builder: read large artifact in parts, summarize within token budget

---

## Phase 7 — TUI (standalone `impetus`)

Reference audit plan: [docs/TUI_REFERENCE.md](docs/TUI_REFERENCE.md) (audit not started). JCode = UX reference only.

- [ ] JCode source audit: `https://github.com/1jehuang/jcode` — pin commit SHA, list presentation files, lock `ADAPT | REIMPLEMENT | SKIP` per component
- [ ] Ratatui + Crossterm spike / evaluation
- [ ] TUI shell: `HarnessClient` only, no core imports
- [ ] Composer (single-line + multiline mode)
- [ ] Bracketed paste support
- [ ] Large paste UX (`[Pasted text · N KB · M lines]`)
- [ ] Streaming output rendering
- [ ] Markdown rendering (bounded)
- [ ] Diff view
- [ ] Approval UI (typed approvals from harness)
- [ ] Session picker / list
- [ ] Fuzzy search (sessions, commands)
- [ ] Command palette
- [ ] Scrollback / resize
- [ ] Status / usage UI
- [ ] Redraw / event coalescing for performance
- [ ] Codex UX patterns: errors + remediation display

---

## Phase 8 — Clients & integrations

- [ ] Zap discovery/connect/authorize protocol
- [ ] Zap backend handoff (Impetus as agent backend)
- [ ] Provider credential management via Keychain API (user-facing flows)
- [ ] Policy customization and approval UI contracts (IPC)
- [ ] Integration tests for full request flows
- [ ] Performance benchmarks for event log queries
- [ ] Migration strategy for schema changes documented + tested

---

## Phase 9 — Remote & platform

- [ ] Controlled SSH/tmux/SFTP agent flow end-to-end
- [ ] Ubuntu 24.04 x86_64 release tier
- [ ] Clean-machine install smoke
- [ ] Update and uninstall documentation

---

## Phase 10 — Security & verification

- [ ] Security review: secret handling, sandbox escapes, policy bypass
- [ ] End-to-end verification: no policy bypass, no credential leakage
- [x] Client examples: minimal read-only observer

---

**Completion criterion:** working vertical slice, tests, passes relevant gates.
