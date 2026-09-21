# TODO — Impetus

Executable roadmap. **Code is source of truth.** Status labels:

- **Implemented** — production path + tests
- **Partial** — library/import exists; gaps remain
- **Planned** — not started or deferred

Detail and evidence: [ARCHITECTURE.md](ARCHITECTURE.md).
Narrative phases (historical): [docs/ROADMAP.md](docs/ROADMAP.md).

Rule: mark Implemented only when the vertical slice works end-to-end. Types-only
or import-only adapters are Partial.

Active parallel TUI work: #146 (markdown), #148 (diff) — do not duplicate.

---

## P0 — Runtime correctness (now)

Gate before P1 product features.

### 1. Provider-native protocol adapters

- [x] Wire `OpenAiProvider` (Chat Completions SSE + tool-call assembly) into
      `impetusd` / `Harness` via `OpenAiNativeAdapter`
- [x] Fix stream end handling so accumulated tool calls emit before
      `[DONE]` / `message_stop` early-return
- [x] Export `AnthropicProvider` (still optional / not default daemon path)
- [x] Keep legacy OpenAI-compatible text adapter in tree for compatibility
- [ ] OpenAI Responses API (`/v1/responses`) — Planned (after Chat Completions production)
- [ ] Explicit `ProviderProtocolAdapter` trait boundary (shared assembler) — Planned

Evidence: `openai_provider.rs`, `openai_native_adapter.rs`, `anthropic_provider.rs`,
`impetusd` `--provider-profile`.

### 2. Mandatory tool argument validation

- [ ] Validate model tool args against tool JSON Schema **before** policy/execution
- [ ] Reject malformed args without reaching executor (`{}` silent fallback banned)
- [ ] Send provider `tools` schemas where the protocol supports them (or document prompt-only catalog honestly)

### 3. Single durable ArtifactStore semantics

- [x] Doctor text: distinguish path-scope sandbox vs Seatbelt process wrap
- [x] Wire measured provider usage into `BudgetChecker::record_usage` via
      `record_turn_with_usage`
- [ ] Process/shell stdout/stderr → durable artifact when large
- [x] Ephemeral `AttachmentStore` for approval previews (keep; document as non-durable)
- [x] `DurableArtifactStore` for truncated tool/web/paste bodies

### 4. Context engine + durable compaction

- [x] HOT/WARM/COLD + lazy descriptions + token-budgeted assemble
- [x] ContextBuilder chunked artifact summarize
- [x] Shared-prefix fork + named checkpoints
- [x] Wire measured provider usage into `BudgetChecker::record_usage`
- [ ] Compaction as durable events (`CompactionStarted` / range / summary refs /
      `CompactionCommitted`) executed from agent loop — not silent history rewrite
- [ ] Structural state (permissions, cwd, budgets, parent, worktree) never only in text summary

### 5. Security / runtime E2E in PR CI

Keep suite small. PR CI today: macOS `fmt` + `clippy -D warnings` +
`cargo test --workspace --lib --bins`.

- [ ] Add focused lib/bin tests (or tiny PR-safe suite) covering:
  - approve → execute; reject
  - cancel
  - reconnect / attach after daemon restart (where feasible without Seatbelt)
  - sandbox deny → no execution
  - `UnknownOutcome` / retry blocked for mutating
  - secret redaction
  - durable artifact restore
- [ ] Do **not** move full Seatbelt integration into every PR; keep nightly/manual

### 6. Capability truth generation

- [ ] `impetus doctor --json` (and human doctor) reflects real capability matrix
      (providers wired, seatbelt vs path-scope, artifact stores, extensions runtime)
- [ ] Prefer generating/checking docs claims from doctor JSON where practical

---

## P1 — Modern coding-agent capabilities

Start after P0 foundations are solid.

### Subagents (explicit roles, not a swarm)

- [ ] Roles: Explore (read-only), Research (read + approved web), Build (worktree),
      Review (read-only diff/tests)
- [ ] Structured child metadata: `parent_id`, `cwd`, `worktree`, `allowed_tools`,
      `write_roots`, `max_tokens`, `max_time`, `max_depth` — **not** prompt-only
- [ ] Persist child results before parent resume
- [ ] Concurrency caps enforced in harness

### Worktree isolation

- [ ] git worktrees for write-capable parallel agents
- [ ] Safe cleanup + recoverable abandoned worktrees
- [ ] Worktree identity survives compaction/resume

### Steer vs follow-up

- [ ] Steer running task vs enqueue follow-up — distinct from a normal user message

### Skills + MCP (real adapters)

- [x] Skills import + filesystem instruction path (`InstructionResolver`, CLI)
- [x] MCP **import** adapter (JSON-RPC client library)
- [ ] MCP tools live in ToolOrchestrator / agent loop
- [ ] Small extension contract: `SKILL.md`, MCP config, manifest, capabilities, digest

### LSP

- [ ] First-class coding tools: definition, references, diagnostics, symbols, hover
- [ ] No hard couple of runtime to one LSP binary

### Web / research

- [x] Search + fetch + SSRF + citations/provenance (core path)
- [x] Session outbound / private-network grants
- [ ] Optional API search backends (Tavily/Exa) as replaceable modules only
- [ ] Real browser providers (Firefox/Chrome/…) — optional, not core deps

---

## P2 — Advanced orchestration (explicitly deferred)

- [ ] Deterministic multi-agent workflows / teams
- [ ] Plugin marketplace / large plugin ABI
- [ ] Portable sessions between harnesses
- [ ] Deep Claude/Codex/Cursor compatibility **runtime** (imports already Partial)
- [ ] Autonomous long-running planner/tester loops
- [ ] Ubuntu 24.04 release tier + clean-machine smoke
- [ ] Full Zap discovery/authorize production protocol

---

## Frontends (ongoing; not blocking P0)

TUI uses `HarnessClient` only (`impetus-tui` boundary tests).

- [x] Ratatui/Crossterm adopted; composer single/multi; large paste upload; streaming
- [ ] Bounded markdown (#146)
- [ ] Diff view (#148)
- [ ] Approval UI, session picker, command palette, scrollback/status polish
- [ ] Redraw coalescing; Codex-like error remediation UX

---

## Historical checklist (superseded)

Older Phase 0–10 checkboxes lived here and over-marked “done”. Prefer P0/P1/P2
above. For archaeology see git history and `docs/TODO_AUDIT_2026-08-30.md`
(dated; do not treat as current).
