# Impetus backlog

Executable open list. Architecture truth = [ARCHITECTURE.md](ARCHITECTURE.md)
(capability matrix + evidence). Done work lives there — not duplicated as walls
of checkboxes here. Short narrative: [docs/architecture/roadmap.md](docs/architecture/roadmap.md).
Doc map: [docs/README.md](docs/README.md).

Rule: mark done only when the vertical slice works end-to-end. Types-only or
import-only adapters stay open / Partial in the matrix.

---

## Now

Real open work that should happen next.

- [ ] Wire `ExploreChildRunner` into production `AgentLoop` / `impetusd`
      (Foundation slice exists; model-provider binding still open) (#296)
- [ ] Wire live MCP tools into production `AgentLoop` / `impetusd`
      (`McpLiveBridge` + orchestrator hook exist; capability truth
      `mcp_live_tools_in_loop: false`)
- [ ] Production macOS Seatbelt wrap into `execution/process.rs`
      (spike / `macos_sandbox_spike` exist; path-scope sandbox already live)
- [ ] Default IPC / CLI / `impetusd` path to load + reload user `PolicyConfig`
      (engine + JSON format exist; #9 leftover)
- [ ] Parent-resume gate wired to live Explore child completion
      (beyond in-memory Explore slice / `gate_parent_resume` stub)

---

## Next

Important, not blocking daily single-session use.

### Orchestration runtime

- [ ] Live agent process spawn from `WorkflowEngine` / scheduler
- [ ] `WorkflowEngine` cancel/replace wired to session-run intents
      (follow-up drain race with cancel/replace still open)
- [ ] Live child process / PTY spawn for Research / Build / Review roles
- [ ] Per-parent concurrency caps / fair scheduling
- [ ] Cross-machine / IPC orchestration beyond in-process fanout

### Trust / policy / hooks

- [ ] `PolicyStore` type + governed-instruction surface
      (not `PolicyConfig` overrides; `Runtime ≠ Memory ≠ Policy`)
- [ ] Operator UX to edit/customize policy (not a harness UI rewrite)
- [ ] Wire `hook_prefilter` into live `ProcessExecution` spawn path
- [ ] Live provider wire for `SteerRewrite` (passthrough default today) (#285)

### Coding / research modules (optional backends)

- [ ] Real LSP process spawn (rust-analyzer / clangd / …) — not a core dep (#282)
- [ ] LSP / coding-tool TUI surface beyond IPC `GotoDefinition` (#267)
- [ ] Real Tavily/Exa HTTP search clients (module seam exists) (#264)
- [ ] Real browser automation behind `BrowserProvider` (seam exists) (#268)

---

## Later

Deferred / explicit non-goals for the near term.

- [ ] Large multi-team / swarm orchestration beyond small `WorkflowEngine` recipes
- [ ] Plugin marketplace / large plugin ABI
- [ ] Portable sessions between harnesses
- [ ] Deep Claude/Codex/Cursor **runtime** compatibility
      (import adapters already Partial)
- [ ] Autonomous long-running planner/tester loops
- [ ] Ubuntu 24.04 release tier + clean-machine smoke automation
      (honesty checklist: [docs/guides/ubuntu-smoke.md](docs/guides/ubuntu-smoke.md); #293)
- [ ] Full Zap discovery/authorize production protocol
      (adapter checklist today: ARCHITECTURE.md § Zap path; #290 / classic #5)
- [ ] Invert `impetus-core` → `impetus-acp-gateway` dependency
      (core should not own gateway as a library dep long-term)
- [ ] Thin-client boundary / `harness_api` domain split
      (Zap/TUI on protocol crate; recoverable errors ≠ daemon panic)
- [ ] CLI migration: keep `impetus` primary; migrate `impetus-cli` callers
      over time (do **not** delete the crate)

---

## Frontends

TUI talks `HarnessClient` only (`impetus-tui` boundary tests). Detail:
[docs/reference/tui-ux-audit.md](docs/reference/tui-ux-audit.md). Help overlay
(`?` / F1) lists the live keymap.

Open:

- [ ] Default path for PolicyConfig load/reload — see **Now**
- [ ] Operator policy-edit UX — see **Next**
- [ ] Full Zap discovery/authorize — see **Later** (#290)
- [ ] Clickable live subagent / child-run surfaces in TUI (needs Explore→daemon
      / child spawn from **Now** / **Next** first)

Done (evidence in ARCHITECTURE / TUI crate tests; do not re-litigate here):
Ratatui+Crossterm GO (#137), bounded markdown (#146), diff (#148), approval UI
(#165/#169), session picker (#166/#169), command palette (#175), scrollback/
status (#178), redraw coalesce + remediation (#179), Zap path honesty docs
(#290), modern harness hotkeys + mouse hit-testing (#302: `?`/F1 help,
Ctrl+O sessions, Ctrl+T steer, Ctrl+Shift+P intent cycle, N/Ctrl+N new
session, Home→timeline top; click timeline/session/composer/approval).

---

## Notes

| Doc | Role |
| --- | --- |
| [ARCHITECTURE.md](ARCHITECTURE.md) | Capability matrix + invariants |
| [docs/architecture/roadmap.md](docs/architecture/roadmap.md) | Now / Next / Later narrative |
| [docs/architecture/kernel-invariants.md](docs/architecture/kernel-invariants.md) | Kernel rules |
| [docs/guides/](docs/guides/) | Getting started, config, CI, Ubuntu smoke |
| [docs/reference/](docs/reference/) | Protocols, TUI audit, components |
| [docs/archive/](docs/archive/) | Historical audits/spikes (not current truth) |

Foundation walls formerly listed under runtime/extension/worktree/schema
checklists are **done** — see ARCHITECTURE capability matrix (provider adapters,
tool-arg schema gate, durable artifacts/compaction, PR security suite, doctor
truth, schemas, extension lifecycle + ownership, memory trust, WorktreeManager,
WorkflowEngine/subagent **Foundation**, steer/follow-up IPC, hooks prefilter,
anti-sprawl, LSP/web seams).

Archaeology only: [docs/archive/todo-audit-2026-08-30.md](docs/archive/todo-audit-2026-08-30.md).
