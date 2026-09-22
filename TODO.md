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

### Doc honesty (remaining)

- [x] README Seatbelt/MCP underclaims — fixed in #316
- [x] ARCHITECTURE SteerRewrite/PolicyConfig/orchestration prose — fixed in #316
- [x] Matrix schemas session/mcp underclaim — fixed in #316
- [x] Desktop IPC v5 claim — fixed in #316 (sibling on v7; anti-patterns = local git/MCP)
- [x] Sync `ARCHITECTURE.ru.md` / `README.ru.md` (ArtifactStore, installer)

### P0 — protocol boundary + rich activity events

- [x] Complete `impetus-client::protocol` façade: DTO + `IPC_VERSION` +
      `IPC_CAPABILITIES` + `IpcErrorCode` + session status
      (`RuntimeStatus` / `SessionInfo` / `CheckpointInfo`) + events;
      feature-gate `InMemoryTransport` (`in-memory`; Unix default)
- [x] Thin `impetus-protocol` crate owns IPC/events wire DTOs (no rusqlite/
      reqwest/Harness); core + `impetus-client::protocol` re-export. Client
      still path-deps core for Unix/`Harness`/`in-memory` runtime
- [x] Compatibility policy note: Hello remains exact-match until RFC for
      `min_supported`; bump IPC when adding Git/Files/PTY/activity caps
- [x] Rich activity event model on parent session log (typed, not tool-name
      scrape): tool lifecycle with `tool_call_id`, child lifecycle, optional
      reasoning **summary** only (no hidden CoT), explicit durable vs ephemeral
      — `ReasoningSummary` + Child* + Tool id/Output; PtyStarted/Output(eof)/
        Exited (+Spill); Tool FileRead/SearchStarted/SearchResult; Command
        Started/Output/Finished on bash approve path; EventStore=durable,
        AttachmentStore=ephemeral (documented in `events.rs`)
- [x] Bound agent chunk persistence (coalesce / spill to artifact); SQL cursor
      `list_after(sequence > after LIMIT)` + `append_next` head-via-COUNT done;
      projection Final+Chunks double-append fixed. Chunks coalesce tiny deltas
      (`AGENT_CHUNK_COALESCE_BYTES`) and spill bodies over
      `MAX_AGENT_CHUNK_EVENT_BYTES` to DurableArtifactStore (preview + ArtifactRef);
      large Final with prior chunks stays preview-only (no duplicate MB row)

### P0 — daemon-owned Git / Files / Diff (kill Desktop local git)

- [x] Typed Git IPC over `WorktreeManager` + system git (read-heavy ok;
      hooks/LFS/checkout via CLI): repository state, branches, status, diffs
      — library + IPC v8 `git` + `git_ops.rs` / harness handlers; impetusd
      attaches WorktreeManager; TUI `Ctrl+B` branch picker
- [x] Branch switch / create with dirty/conflict/stale safeguards; session
      active worktree awareness
      — dirty/conflict/stale guards in `git_ops`; session cwd via daemon
      `with_worktree_manager`; TUI switch/create via harness
- [x] Workspace Files read IPC (`ListDir` / `Stat` / `ReadFile` / search) with
      traversal + symlink-escape hardening (match `memory_store` rigor)
      — library + IPC v8 wired (`ListWorkspaceDir` / `StatWorkspaceFile` /
      `ReadWorkspaceFile` / `SearchWorkspaceFiles`); TUI `Ctrl+F` / `/files`
- [x] Desktop drop local `git_*` + MCP/Models config parse → HarnessClient
      Git / `ListMcpServers` / `ListModels` IPC (sibling `impetus-desktop`,
      path-dep; IPC v10). Files/Review Desktop UI still open.
- [x] `DiffObservation` producer (`diff_observation.rs`) +
      `GetApprovalDetail` real proposed/git unified (no fake write-only SoT)
- [x] TUI Review pane (F6 / Ctrl+R / `/review`): file list + status + +/- via
      daemon `GitStatus`/`GetDiff`/`GetFileDiff`; hunk nav; approval overlay OK
- [x] Typed Diff/Review IPC beyond patch string (`GetDiff`/`GetFileDiff`
      include `DiffObservation` hunks; cap `structured_diff`; IPC v11;
      WorktreeManager counts overlay when `base_ref` bound; HarnessClient
      `get_structured_diff` / `get_structured_file_diff`; TUI Review prefers
      structured when present)
- [x] Files pagination / search UI polish; Desktop Files UI
      — TUI `/` daemon search + name filter + 500-entry dir cap; Desktop
      FileTree `search_workspace_files` hits + load-more via harness (no local walk)

### P0 — child live stream + approvals E2E

- [x] Emit parent-log `Child*` events (id, parent, role, state, summary);
      `ListChildRuns` remains snapshot, not sole source
- [x] Approval→Seatbelt E2E sentinel (macOS heavy OK in `tests/`; thin
      approve/reject+fingerprint in `--lib`): workspace allow + sibling deny;
      forged ResolveApproval no-op (Files/Git/PTY IPC already shipped)
- [x] Record `SandboxDecision` (backend=`macos_seatbelt`) in durable events
- [x] Document/test: PolicyConfig soft `Allow` skips RiskGate NeedsHuman;
      hard Deny path/network still wins. Consider mid-run AgentLoop stale
      policy clone after ReloadPolicyConfig
      — `effects` soft Allow + path/network/RiskGate Deny; `policy`
        `config_cannot_allow_write_outside_workspace`; AgentLoop
        `mid_run_policy_clone_ignores_later_reload`; ARCHITECTURE note

### P1 — PTY capability (not TUI xterm)

- [x] Real PTY via `portable-pty` 0.9.0 (replace stub fake PID); IPC
      Start/Attach/Input/Output/Resize/Detach/Terminate/Status (`IPC_VERSION` 9,
      capability `pty`)
- [x] Bounded output ring (256 KiB); daemon owns PTY; optional SqlitePtySessionStore
- [x] TUI: passthrough attach (stdin/stdout/resize) + escape back to Ratatui —
      no custom ANSI emulator (`Ctrl+\` / `/pty`, detach `Ctrl+]`)
- [x] Artifact spill for PTY overflow (ring drops oldest today)

### P1 — artifacts / MCP-model read APIs

- [x] Durable artifact read/range IPC + MIME persistence
- [x] Durable artifact prod GC/lifecycle
- [x] Daemon `ListMcpServers` / status read API (labels + connected; no
      env/args/secrets) — Desktop still must drop local config parse
- [x] Daemon `ListModels` / provider status read API (picker UI later)

### P1 — TUI presentation (after harness APIs exist)

- [x] Structured activity tree fold (Explore/tools/reasoning expandable)
      — TUI folds Child+Tool(+ReasoningSummary)+Pty/File/Search/Command into
        collapsed `◆ activity` tree with `├`/`└` expand detail
- [x] Typed Search/FileRead/Command activity rows (Pty*/FileRead/Search*/
      Command* emit + ActivityStep fold; no token-delta flood)
- [x] Files overlay/pane (list + read; not full IDE) — `Ctrl+F` / `/files`
- [x] Review overlay/pane (changed files, `+/-`, hunks) — F6 / `Ctrl+R` /
      `/review` (same as P0 Review pane)
- [x] Artifact attach from filesystem + show refs
      — `/attach [path]` · `Ctrl+Shift+A` path overlay → chunked
        `artifact_upload` via HarnessClient; composer holds
        `[Attached · name · size · mime]` + pending DurableArtifactRef;
        Enter Prompt carries ArtifactRef; Intent timeline shows
        `artifact {id} · N KB` (no secrets/bytes)
- [x] Fork / checkpoint UX; workspace folder pick
      — `/fork` · `Ctrl+Shift+K` · `/checkpoint` · F7 `/checkpoints` over
        HarnessClient; header shows fork@seq ← parent from SessionInfo;
        `/new` + picker N → path prompt for CreateSession

### P2 — CI / performance (keep PR fast)

- [x] Named `--lib` sentinels: protocol / git / files / events / artifacts
      (pattern of `security_runtime_pr`); do **not** put `crates/*/tests/` in PR
      — `sentinel_protocol` (`ipc`), `sentinel_git` (`git_ops`),
        `sentinel_files` (`workspace_files`), `sentinel_events` (`events`),
        `sentinel_artifacts` (`durable_artifacts`);
        filter: `cargo test -p impetus-core --lib -- sentinel`
- [x] Criterion: chunk ingest + Stream frame size; document bounds
      (`benches/event_log.rs` + `docs/benchmarks/event-log-v0.2.md`; MAX vs 64 KiB)

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
| PCFG | PolicyConfig load/reload | Implemented | startup + IPC `ReloadPolicyConfig` | mid-run AgentLoop clone stale until next Prompt | invalid reload keeps prior | harness_api + effects + agent_loop | — | none | optional |
| PSTORE | PolicyStore | Implemented | autoload + Get/Reload IPC + CLI | — | labels only | policy_store | — | none | optional |
| SB-PATH | Path-scope sandbox fail-closed | Implemented | `effects.rs`, tools resolve, `workspace_files` resolve | — | deny outside root | sandbox_fail_closed / tools / workspace_files | — | via tools | via Files IPC |
| SB-SEAT | macOS Seatbelt process wrap | Implemented | `execution/sandbox.rs` + process.rs | Linux/Win Planned; README still “spike” | spawn through sandbox-exec | macos_sandbox_production (heavy) | — | n/a | n/a |
| RISK | RiskGate mode-aware | Implemented | EffectSeam | — | post-policy | risk_gate | POL | modes IPC | modes IPC |
| APPR | Approvals + ApprovalDetail | Implemented | events + GetApprovalDetail; `approval_seatbelt_e2e` | fake write diff | Y/N + detail | harness_api (write only) | POL | overlays | cards |
| KEY | Keychain API-key refs | Implemented | MacosKeychainResolver on profile | noninteractive fail-closed | no raw tokens in SQLite | doctor / CI env | — | n/a | n/a |

### Runtime / orchestration

| ID | Capability | Status | Production paths | Remaining | AC | Tests | Deps | TUI | Desktop API |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| EVT | Durable EventStore + reconnect cursor | Implemented | SqliteEventStore, Stream/Subscribe, `list_after`; agent Chunk coalesce+spill; Stream `Events` frame ≤ `IPC_EVENTS_FRAME_BUDGET` | — | resume `after_sequence` no dup; chunk preview+ArtifactRef; batched Stream under 64KiB | storage / ipc / runtime chunk bound | — | subscribe_live | subscribe |
| ART | DurableArtifactStore + chunked upload/read | Implemented | upload+read/meta/range IPC + MIME + paste TUI + filesystem attach TUI + tool spill + agent Chunk spill; prod age GC 7d (startup+6h) | — | Prompt by artifact id; Read/Meta/Range; age GC | artifact_upload / durable_artifacts / harness_api / runtime / impetusd | EVT | `/attach` · paste | HarnessClient read helpers |
| ATT | Ephemeral AttachmentStore | Implemented | approvals/diffs RAM | GetAttachment unbound to session; TUI barely fetches | ephemeral only | attachments | APPR | partial | GetAttachment |
| ALOOP | AgentLoop + tools | Implemented | Prompt path | — | — | agent_loop | POL | stream | stream |
| MCP-RT | MCP runtime in daemon | Partial | autoload `mcp/*.json` → ToolProviderRuntime; `ListMcpServers` IPC | Desktop parses Codex TOML; lifecycle writes `{workspace}/.impetus/mcp/` (third path, not autoloaded); no live health probe on list | daemon SoT for catalog | daemon_wiring + harness_api | — | none | **must use IPC** |
| EXP | Explore child (AgentLoop) | Implemented | `explore_spawn` + ChildResultStore | no live Child events on parent log | restricted tools | explore_* | EVT | `/children` poll | ListChildRuns |
| WF | WorkflowRuntime role children | Implemented | Start/Advance/Cancel IPC | Role exec = process/echo stub; Workflow Explore ≠ AgentLoop Explore; fake worktree id | cancel + fair caps | workflow_runtime | EXP | workflow none | workflow IPC |
| WT | WorktreeManager | Partial | library lifecycle + `impetusd` `open_daemon_worktree_manager` → `Harness::with_worktree_manager` (`worktrees.sqlite3` + `worktrees/`) | Build uses synthetic wt id; no create/merge IPC | create/stale/merge-ready | worktree_manager + daemon_wiring | SB-PATH | none | none (Desktop uses local git — anti-pattern) |
| PTY | PTY sessions | Partial | `portable-pty` spawn + Pty* IPC (v9+) + ring + harness_api; overflow spill → DurableArtifactStore + `PtyEvent::Spill`; TUI `Ctrl+\` / `/pty` passthrough | store optional; no re-attach UI | real spawn + bounded I/O + spill + TUI passthrough | pty unit (cat/sleep/spill) + encode/detach unit | POL | Ctrl+\ passthrough | xterm.js later |
| LSP | Coding tools / LSP | Partial | IPC GotoDefinition/Hover handlers | **impetusd never wires** ProcessLspBackend → always absent | binary-present → Available | coding_tools | — | none | none |
| BRW | Browser provider | Partial | negotiate/health binary-present | navigate fail-closed without CDP | honest Available | browser modules | — | none | none |
| STEER | SteerRewrite live provider | Implemented | ProviderSteerRewrite in daemon | — | one-shot rewrite | steer tests | ALOOP | Ctrl+T | intents |
| MEM | MemoryStore | Partial | library only | not daemon control plane | — | memory_store | — | none | none |
| EXT | Extension lifecycle | Partial | CLI plan/install/doctor | no marketplace | allowlisted ids | extension_* | — | none | none |
| ACP | ACP ModelProvider | Partial | `--acp-profile` | tool/permission broker gaps | Incompatible explicit | acp gateway | — | via daemon | via daemon |
| SCHEMA | Canonical schemas | Partial | approval/capabilities/extension/session/mcp in registry | matrix underclaims session/mcp | validate on load | schema.rs | — | n/a | n/a |

### Client protocol

| ID | Capability | Status | Production paths | Remaining | AC | Tests | Deps | TUI | Desktop API |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| IPC | Unix IPC v11 + Hello exact-match | Implemented | `ipc.rs` IPC_VERSION=11 | no min_supported range; client still path-deps full core | Incompatible on skew | ipc / impetusd protocol | — | HarnessClient | HarnessClient (v11) |
| PROTO | Thin protocol crate / façade | Implemented | `impetus-protocol` + `impetus-client::protocol` re-exports | client still path-deps core for Unix/`Harness`/`in-memory`; Desktop/TUI can import types via protocol without rusqlite for TYPES | Desktop/TUI import types via façade/protocol | client unit | IPC | boundary.rs | path-dep client |
| ACT | Rich activity events | Implemented | Tool Observed/Started/Finished(+id)/Output + FileRead/Search* + Agent Chunk/ReasoningSummary + Child* + Pty* + Command* | Desktop stream map parity | same events TUI+Desktop | events serialize + FileRead emit path | EVT | Activity fold | stream map |
| GIT | Git/branches/status/diff IPC | Implemented | IPC `git` + `git_ops` / harness; WorktreeManager cwd; TUI `Ctrl+B`; Desktop BranchSelect via harness | create-branch UX polish; worktrees UI | daemon owns checkout | git_ops + harness | WT | Ctrl+B picker | harness git_* |
| FILES | Workspace Files read API | Implemented | IPC + TUI `Ctrl+F` `/` search + Desktop FileTree list/read/search | CodeMirror highlight optional | path-safe structured | workspace_files | SB-PATH | Ctrl+F + `/` search | FileTree search+list |
| DIFF | Typed Diff/Review API | Implemented | producer + ApprovalDetail observation/unified; GitDiff patch + `observation` hunks IPC v11 (`structured_diff`); WorktreeManager counts overlay; HarnessClient structured helpers; TUI Review prefers observation | ArtifactRef spill for huge diffs; Desktop Review | structured hunks on GetDiff/GetFileDiff | diff_observation + harness temp-repo | GIT | Review pane (F6/Ctrl+R) | Review UI |
| CHILD | Child live events | Partial | explore/role/workflow emit `Child*` on parent log + List/GetChildRuns | mid-run action stream thin | live + snapshot | `parent_event_log_gets_child_started_then_finished` | EXP/WF | Activity fold | tree |
| MCP-RD | ListMcpServers / ListModels IPC | Implemented | harness + Desktop AttachMenu via IPC (v10) | TUI picker later; no live MCP health probe on list | read-only status | harness_api + ipc | MCP-RT | picker later | harness list_* |

### Frontends

| ID | Capability | Status | Production paths | Remaining | AC | Tests | Deps | TUI | Desktop API |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| TUI | `impetus ui` | Partial | sessions/stream/approvals/modes/paste/attach `/attach`+Ctrl+Shift+A/children/Files `Ctrl+F`+`/` search/git `Ctrl+B`/Review F6/`Ctrl+R`/fork `/fork`+`Ctrl+Shift+K`/checkpoints F7/`/checkpoint`/workspace `/new` path prompt/Activity fold/PTY `Ctrl+\` | sequence picker polish | HarnessClient only | tui unit | IPC | — | — |
| DESK | Impetus Desktop (sibling) | Partial | path-dep client; harness Git/MCP/Models/Files list+read+search (no local fs walk); Review via Git IPC | PTY UI; model picker polish; worktrees; CodeMirror optional | all features via harness | desktop smoke | IPC+GIT+FILES+… | — | thin shell |

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
