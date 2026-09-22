# Impetus backlog

Executable open work + short context for the next agent.
Architecture truth = [ARCHITECTURE.md](ARCHITECTURE.md) (capability matrix +
evidence). Narrative direction = [docs/architecture/roadmap.md](docs/architecture/roadmap.md).
Doc map = [docs/README.md](docs/README.md).

**Rules**

- Mark `[x]` only when the vertical slice works end-to-end in production
  daemon/client path (not library-only, not mock-only).
- Never leave `[x]` with a Partial tail. Split into `[x]` done + `[ ]` remaining.
- Before non-trivial work: read this file + ARCHITECTURE.md.
- After implementation: update both in the same change. Stale docs = bug.
- **No feature only as GUI/TUI hack.** Capability + typed contract in Impetus
  first; TUI/Desktop are presentation layers over the same `HarnessClient`.

**Program issue:** [#315](https://github.com/1tuz/impetus/issues/315) — harness
unify (TUI/CLI/Desktop share capabilities, events, state).

Status vocabulary (same as ARCHITECTURE matrix):

| Label | Meaning |
| --- | --- |
| **Implemented** | Production `impetusd` / client path + tests |
| **Partial** | Library or incomplete surface; Remaining lists exact gaps |
| **Planned** | Roadmap / open checkbox only |

---

## Now ([#315](https://github.com/1tuz/impetus/issues/315))

Harness-first unify. Do not invent parallel Desktop/TUI feature logic.

### Doc honesty (same change as first code PR)

- [ ] Fix README Seatbelt/MCP underclaims (live on macOS / daemon autoload)
- [ ] Fix ARCHITECTURE request-path prose: SteerRewrite + PolicyConfig are
      Implemented (matrix already); orchestration mermaid “live spawn Planned”
      stale vs WorkflowRuntime
- [ ] Matrix schemas: `impetus.session.v1` / `impetus.mcp.v1` registered —
      drop “session/mcp Planned” underclaim
- [ ] Sync `ARCHITECTURE.ru.md` / `README.ru.md` (ArtifactStore, installer)
- [ ] Correct Desktop IPC status: sibling already on **v7** path-dep (not v5);
      anti-patterns are local `git_*` + `list_mcp_servers`, not Hello skew

### P0 — protocol boundary + rich activity events

- [ ] Gradual `impetus-protocol` slice (or complete `impetus-client::protocol`
      façade): wire DTO + `IPC_VERSION` + events; feature-gate `InMemory` so
      Desktop/TUI do not need full core for types
- [ ] Compatibility policy note: Hello remains exact-match until RFC for
      `min_supported`; bump IPC when adding Git/Files/PTY/activity caps
- [ ] Rich activity event model on parent session log (typed, not tool-name
      scrape): tool lifecycle with `tool_call_id`, child lifecycle, optional
      reasoning **summary** only (no hidden CoT), explicit durable vs ephemeral
- [ ] Bound agent chunk persistence (coalesce / spill to artifact); fix
      Final+Chunks double-store; SQL cursor `sequence > after LIMIT` (kill
      full-list-per-append hot path)

### P0 — daemon-owned Git / Files / Diff (kill Desktop local git)

- [ ] Typed Git IPC over `WorktreeManager` + system git (read-heavy ok;
      hooks/LFS/checkout via CLI): repository state, branches, status, diffs
- [ ] Branch switch / create with dirty/conflict/stale safeguards; session
      active worktree awareness
- [ ] Workspace Files read IPC (`ListDir` / `Stat` / `ReadFile` / search) with
      traversal + symlink-escape hardening (match `memory_store` rigor)
- [ ] Typed Diff/Review API (structured hunks or `ArtifactRef`); wire
      `DiffObservation` producer; stop fake write-only approval previews as
      the only “diff”

### P0 — child live stream + approvals E2E

- [ ] Emit parent-log `Child*` events (id, parent, role, state, summary);
      `ListChildRuns` remains snapshot, not sole source
- [ ] Approval→Seatbelt E2E sentinel (macOS heavy OK in `tests/`; thin
      approve/reject+fingerprint in `--lib`): workspace allow + sibling deny;
      forged ResolveApproval no-op. **Block before shipping Files/Git/PTY IPC**
- [ ] Record `SandboxDecision` (backend=`macos_seatbelt`) in durable events
- [ ] Document/test: PolicyConfig soft `Allow` skips RiskGate NeedsHuman;
      hard Deny path/network still wins. Consider mid-run AgentLoop stale
      policy clone after ReloadPolicyConfig

### P1 — PTY capability (not TUI xterm)

- [ ] Real PTY via `portable-pty` (replace stub fake PID); IPC
      Start/Attach/Input/Output/Resize/Detach/Terminate/Status
- [ ] Bounded output ring + artifact spill; daemon owns PTY
- [ ] TUI: passthrough attach (stdin/stdout/resize) + escape back to Ratatui —
      no custom ANSI emulator

### P1 — artifacts / MCP-model read APIs

- [ ] Durable artifact read/range IPC + MIME persistence; prod GC/lifecycle
- [ ] Daemon `ListMcp` / status read API; Desktop must drop local config parse
- [ ] Daemon `ListModels` / provider status read API (picker UI later)

### P1 — TUI presentation (after harness APIs exist)

- [ ] Structured activity tree (Explore/tools/commands expandable)
- [ ] Git branch popup (`Ctrl+B`): list/filter/switch/create
- [ ] Files overlay/pane (list + read; not full IDE)
- [ ] Review overlay/pane (changed files, `+/-`, hunks)
- [ ] Artifact attach from filesystem + show refs
- [ ] Fork / checkpoint UX; workspace folder pick

### P2 — CI / performance (keep PR fast)

- [ ] Named `--lib` sentinels: protocol / git / files / events / artifacts
      (pattern of `security_runtime_pr`); do **not** put `crates/*/tests/` in PR
- [ ] Criterion: chunk ingest + Stream frame size; document bounds

---

## Capability inventory

Audit evidence: 2026-09-22 code audit (#315). Prefer this table over stale README
prose. Matrix in ARCHITECTURE must match; if conflict — **code + this table**.

Legend columns: **TUI** / **Desktop API** = presentation readiness over shared
harness (not a second implementation).

### Kernel / trust

| ID | Capability | Status | Production paths | Remaining | AC | Tests | Deps | TUI | Desktop API |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| POL | Policy Deny/Allow/NeedsApproval + origin | Implemented | `policy.rs`, ToolOrchestrator, EffectSeam | — | origin never client-trust | orchestrator + security_runtime_pr | — | Y/N overlays | resolve_approval |
| PCFG | PolicyConfig load/reload | Implemented | startup + IPC `ReloadPolicyConfig` | docs Partial prose | invalid reload keeps prior | harness_api | — | none | optional |
| PSTORE | PolicyStore | Implemented | autoload + Get/Reload IPC + CLI | — | labels only | policy_store | — | none | optional |
| SB-PATH | Path-scope sandbox fail-closed | Implemented | `effects.rs`, tools resolve | symlink walk harden for Files API | deny outside root | sandbox_fail_closed / tools | — | via tools | via Files IPC |
| SB-SEAT | macOS Seatbelt process wrap | Implemented | `execution/sandbox.rs` + process.rs | Linux/Win Planned; README still “spike” | spawn through sandbox-exec | macos_sandbox_production (heavy) | — | n/a | n/a |
| RISK | RiskGate mode-aware | Implemented | EffectSeam | — | post-policy | risk_gate | POL | modes IPC | modes IPC |
| APPR | Approvals + ApprovalDetail | Implemented | events + GetApprovalDetail | fake write diff; no Policy→Approval→Seatbelt E2E; no SandboxDecision audit event | Y/N + detail | harness_api (write only) | POL | overlays | cards |
| KEY | Keychain API-key refs | Implemented | MacosKeychainResolver on profile | noninteractive fail-closed | no raw tokens in SQLite | doctor / CI env | — | n/a | n/a |

### Runtime / orchestration

| ID | Capability | Status | Production paths | Remaining | AC | Tests | Deps | TUI | Desktop API |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| EVT | Durable EventStore + reconnect cursor | Implemented | SqliteEventStore, Stream/Subscribe | O(n) list-per-append; unbounded Chunk text; Stream line vs 64KiB | resume `after_sequence` no dup | storage / ipc | — | subscribe_live | subscribe |
| ART | DurableArtifactStore + chunked upload | Implemented | upload IPC + paste TUI + tool spill | no durable Read IPC; MIME ignored; no prod GC | Prompt by artifact id | artifact_upload / durable_artifacts | EVT | large paste | need ReadArtifact |
| ATT | Ephemeral AttachmentStore | Implemented | approvals/diffs RAM | GetAttachment unbound to session; TUI barely fetches | ephemeral only | attachments | APPR | partial | GetAttachment |
| ALOOP | AgentLoop + tools | Implemented | Prompt path | — | — | agent_loop | POL | stream | stream |
| MCP-RT | MCP runtime in daemon | Partial | autoload `mcp/*.json` → ToolProviderRuntime | no ListMcp IPC; Desktop parses Codex TOML; lifecycle writes `{workspace}/.impetus/mcp/` (third path, not autoloaded) | daemon SoT for catalog | daemon_wiring | — | none | **must use IPC** |
| EXP | Explore child (AgentLoop) | Implemented | `explore_spawn` + ChildResultStore | no live Child events on parent log | restricted tools | explore_* | EVT | `/children` poll | ListChildRuns |
| WF | WorkflowRuntime role children | Implemented | Start/Advance/Cancel IPC | Role exec = process/echo stub; Workflow Explore ≠ AgentLoop Explore; fake worktree id | cancel + fair caps | workflow_runtime | EXP | workflow none | workflow IPC |
| WT | WorktreeManager | Partial | library lifecycle + system git | **not** in IPC/daemon wire; Build uses synthetic wt id | create/stale/merge-ready | worktree_manager | SB-PATH | none | none (Desktop uses local git — anti-pattern) |
| PTY | PTY sessions | Partial | library stub + SqlitePtySessionStore orphaned | fake PID; no portable-pty; no IPC | real spawn + bounded I/O | pty unit only | POL | Planned passthrough | xterm.js later |
| LSP | Coding tools / LSP | Partial | IPC GotoDefinition/Hover handlers | **impetusd never wires** ProcessLspBackend → always absent | binary-present → Available | coding_tools | — | none | none |
| BRW | Browser provider | Partial | negotiate/health binary-present | navigate fail-closed without CDP | honest Available | browser modules | — | none | none |
| STEER | SteerRewrite live provider | Implemented | ProviderSteerRewrite in daemon | request-path docs still Partial | one-shot rewrite | steer tests | ALOOP | Ctrl+T | intents |
| MEM | MemoryStore | Partial | library only | not daemon control plane | — | memory_store | — | none | none |
| EXT | Extension lifecycle | Partial | CLI plan/install/doctor | no marketplace | allowlisted ids | extension_* | — | none | none |
| ACP | ACP ModelProvider | Partial | `--acp-profile` | tool/permission broker gaps | Incompatible explicit | acp gateway | — | via daemon | via daemon |
| SCHEMA | Canonical schemas | Partial | approval/capabilities/extension/session/mcp in registry | matrix underclaims session/mcp | validate on load | schema.rs | — | n/a | n/a |

### Client protocol

| ID | Capability | Status | Production paths | Remaining | AC | Tests | Deps | TUI | Desktop API |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| IPC | Unix IPC v7 + Hello exact-match | Implemented | `ipc.rs` IPC_VERSION=7 | no min_supported range; client pulls full core | Incompatible on skew | ipc / impetusd protocol | — | HarnessClient | HarnessClient (v7) |
| PROTO | Thin protocol crate / façade | Planned | `impetus-client::protocol` re-export only | split `impetus-protocol`; gate InMemory | Desktop/TUI compile without rusqlite/reqwest | — | IPC | boundary.rs | path-dep client |
| ACT | Rich activity events | Partial | Tool Observed/Started + Agent Chunk | no Child*/Pty*/FileRead/Search/Command/ReasoningSummary typed; flat TUI timeline | same events TUI+Desktop | events serialize | EVT | flat timeline | stream map |
| GIT | Git/branches/status/diff IPC | Planned | WorktreeManager counts only | full typed API missing | daemon owns checkout | worktree + new | WT | Ctrl+B Planned | **replace git_*** |
| FILES | Workspace Files read API | Partial | IPC `Tool` List/Read/Search strings | no structured DirEntry/pagination; TUI unused; symlink walk gaps | path-safe structured | tools + new | SB-PATH | Files pane Planned | Files UI |
| DIFF | Typed Diff/Review API | Partial | ApprovalDetail string; DiffObservation types unused | no producer/IPC; TUI paints heuristics | structured hunks or ArtifactRef | observations + new | GIT | Review pane Planned | Review UI |
| CHILD | Child live events | Partial | poll List/GetChildRuns | no EventPayload Child* | live + snapshot | child_result_store | EXP/WF | tree Planned | tree |
| MCP-RD | ListMcp / model catalog IPC | Planned | daemon file autoload only | was Parked; unblock for Desktop SoT | read-only status | — | MCP-RT | picker later | drop local parse |

### Frontends

| ID | Capability | Status | Production paths | Remaining | AC | Tests | Deps | TUI | Desktop API |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| TUI | `impetus ui` | Partial | sessions/stream/approvals/modes/paste/children | no Files/Review/git/activity-tree/fork UI | HarnessClient only | tui unit | IPC | — | — |
| DESK | Impetus Desktop (sibling) | Partial | path-dep client v7; live subscribe/modes/approvals | local `git_*`, `list_mcp_servers`; no ReadArtifact | all features via harness | desktop smoke | IPC+GIT+FILES+… | — | thin shell |

---

## Acceptance criteria (program)

1. Any new user-visible feature lands as harness capability + IPC (+ events)
   before TUI/Desktop UI.
2. Desktop never shells out to `git` or parses MCP/provider config when daemon
   can answer via typed API.
3. TUI and Desktop render the **same** durable/ephemeral event model.
4. TODO + ARCHITECTURE matrix stay aligned after each PR; README “What works
   now” matches matrix (no Seatbelt spike / MCP unwired lies).
5. PR CI stays path-aware and fast; heavy Seatbelt/integration stays
   `task verify`.

---

## Recently shipped (do not re-litigate)

- [#308](https://github.com/1tuz/impetus/issues/308) / [#311](https://github.com/1tuz/impetus/issues/311):
  IPC v7, execution modes + RiskGate, WorkflowRuntime / role children, policy
  CLI/IPC, live SteerRewrite, Explore daemon wire, MCP/hooks autoload, LSP
  Hover handlers, API search, browser negotiate
- TUI: hotkeys (#302), themes (#304), Explore/modes, `/children`, approvals,
  large-paste artifacts
- Desktop sibling: rebase to IPC v7 + live subscribe / modes / approval cards
  (see `impetus-desktop/TODO.md`) — **still** must migrate git/MCP to harness

---

## Later (parked — not open work)

| Item | Status |
| --- | --- |
| Cross-machine orchestration beyond in-process fanout | Parked |
| Large multi-team / swarm beyond WorkflowEngine recipes | Won't near-term |
| Plugin marketplace / large plugin ABI | Won't near-term |
| Portable sessions between harnesses | Parked |
| Deep Claude/Codex/Cursor **runtime** compatibility | Import adapters Partial only |
| Autonomous long-running planner/tester loops | Parked |
| Ubuntu 24.04 clean-machine smoke automation | Checklist (#293); automation Parked |
| Full Zap discovery/authorize production protocol | Checklist (#290); production Parked |
| Invert `impetus-core` → `impetus-acp-gateway` dependency | Parked large refactor |
| Full thin-client / `harness_api` domain split (beyond gradual protocol) | Parked mega-refactor; gradual PROTO is Now |
| CLI migration `impetus-cli` → `impetus` | Keep both; migrate callers over time |
| Full CDP/WebDriver browser automation | Parked (negotiate/health live) |
| Full LSP protocol completeness | Parked beyond spawn+definition+hover |
| Custom PTY/ANSI terminal emulator inside TUI | Won't — passthrough only |
| Hidden chain-of-thought UI | Won't — reasoning summary/intent only |

---

## Notes

| Doc | Role |
| --- | --- |
| [ARCHITECTURE.md](ARCHITECTURE.md) | Capability matrix + invariants |
| [docs/architecture/roadmap.md](docs/architecture/roadmap.md) | Now / Next / Later narrative |
| [docs/guides/](docs/guides/) | Getting started, config, CI |
| [docs/reference/](docs/reference/) | Protocols, TUI audit, components |
| [docs/archive/](docs/archive/) | Historical audits (not current truth) |
| Sibling [impetus-desktop/TODO.md](../impetus-desktop/TODO.md) | Desktop presentation backlog |

Audit agents (2026-09-22, all 18 zones): docs, daemon, protocol, events, TUI,
git, files, diff, artifacts, activity, children, PTY, approval/policy, MCP,
CI, perf, security, Desktop. Archaeology:
[docs/archive/todo-audit-2026-08-30.md](docs/archive/todo-audit-2026-08-30.md).
