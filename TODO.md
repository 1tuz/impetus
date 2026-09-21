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

---

## Now

Active work for [#311](https://github.com/1tuz/impetus/issues/311) — clear former
**Next** backlog. (#308 Now is done on `main`.)

### Orchestration

- [x] Live agent process spawn from `WorkflowEngine` / scheduler into harness
      **Done:** `WorkflowRuntime` + IPC `StartWorkflow`/`AdvanceWorkflow`; recipe
      step → scheduler → role/explore child → durable `ChildResultStore`
- [x] `WorkflowEngine` cancel/replace wired to session-run intents (drain race closed)
      **Done:** `Cancel` IPC + `CancelWorkflow` cancel admissions; terminal hook
      drains FollowUp at-most-once via `UserIntentRouter`
- [x] Live child process / PTY for Research / Build / Review (beyond Explore)
      **Done:** `role_child` + `ProcessRoleChildExecutor` OS spawn; Build requires
      worktree; fair `ChildConcurrencyGate` on live path
- [x] Per-parent concurrency caps / fair scheduling on live path
      **Done:** `ChildConcurrencyConfig::fair` enforced in `RoleChildRunner` /
      `WorkflowRuntime` gate
- [x] Daemon-owned hook_prefilter catalog file load
      **Done:** `$IMPETUS_DATA_DIR/hooks.json` (or equiv) → ProcessExecution path

### Trust / policy / provider

- [x] `PolicyStore` type + governed-instruction surface
      (`Runtime ≠ Memory ≠ Policy`)
      **Done:** type + load/validate + engine surface; tests; no secrets
- [x] Operator UX to edit/customize policy (CLI and/or IPC, not harness UI rewrite)
      **Done:** `impetus-cli policy reload|store-show|store-reload` + IPC
      `ReloadPolicyConfig` / `GetPolicyStore` / `ReloadPolicyStore`
- [x] Live provider wire for `SteerRewrite` (#285)
      **Done:** `ProviderSteerRewrite` via `with_provider_steer_rewrite` in daemon
- [x] Clickable live subagent / child-run surfaces in TUI
      **Done:** IPC `ListChildRuns`/`GetChildRun`; TUI `/children`; CLI `children`

### Optional coding / research backends

- [x] Real LSP process spawn (not a core dep) (#282)
      **Done:** `ProcessLspBackend` spawns when binary present; fail-closed if absent
- [x] LSP / coding-tool TUI beyond IPC `GotoDefinition` (#267)
      **Done:** IPC `Hover` (`coding_hover` capability)
- [x] Real Tavily/Exa HTTP clients (seam exists) (#264)
      **Done:** `HttpApiSearchBackend` + `ApiKeyResolver`; absent key fail-closed
- [x] Real browser automation behind `BrowserProvider` (seam exists) (#268)
      **Done:** binary-present negotiate/health Available; navigate still fail-closed
      without CDP (honest)

---

## Later (parked — not open work)

Explicit non-goals / postponed. **No open checkboxes.** Track via issues if
revisited; do not treat as current backlog.

| Item | Status |
| --- | --- |
| Cross-machine / IPC orchestration beyond in-process fanout | Parked |
| Large multi-team / swarm beyond small WorkflowEngine recipes | Won't near-term |
| Plugin marketplace / large plugin ABI | Won't near-term |
| Portable sessions between harnesses | Parked |
| Deep Claude/Codex/Cursor **runtime** compatibility | Import adapters Partial only |
| Autonomous long-running planner/tester loops | Parked |
| Ubuntu 24.04 release tier + clean-machine smoke automation | Checklist exists (#293); automation Parked |
| Full Zap discovery/authorize production protocol | Checklist (#290); production Parked |
| Invert `impetus-core` → `impetus-acp-gateway` dependency | Parked large refactor |
| Thin-client / `harness_api` domain split | Parked |
| CLI migration `impetus-cli` → `impetus` | Keep both; migrate callers over time |
| Full CDP/WebDriver browser automation | Parked (negotiate/health live; navigate stub) |
| Full LSP protocol completeness (diagnostics push, symbols) | Parked beyond spawn+definition+hover |

---

## Frontends

TUI talks `HarnessClient` only. Child-run / policy UX items under **Now** are done.

Done (evidence in ARCHITECTURE / crate tests — do not re-litigate):
Ratatui+Crossterm (#137), markdown (#146), diff (#148), approval UI (#165/#169),
session picker (#166/#169), palette (#175), scrollback (#178), redraw (#179),
Zap honesty docs (#290), modern harness hotkeys (#302), theme pack (#304),
Explore library + daemon AgentLoop wire (#306 / #308), CI/modes/RiskGate (#308),
WorkflowRuntime / role children / policy CLI / LSP-process / API search (#311).

---

## Notes

| Doc | Role |
| --- | --- |
| [ARCHITECTURE.md](ARCHITECTURE.md) | Capability matrix + invariants |
| [docs/architecture/roadmap.md](docs/architecture/roadmap.md) | Now / Next / Later narrative |
| [docs/guides/](docs/guides/) | Getting started, config, CI, Ubuntu smoke |
| [docs/reference/](docs/reference/) | Protocols, TUI audit, components |
| [docs/archive/](docs/archive/) | Historical audits (not current truth) |

Archaeology: [docs/archive/todo-audit-2026-08-30.md](docs/archive/todo-audit-2026-08-30.md).
