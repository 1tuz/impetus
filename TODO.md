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

Active work for [#308](https://github.com/1tuz/impetus/issues/308).

### Docs truth (persistent memory)

- [x] Rewrite `TODO.md` Now/Next/Later without `[x]`+Partial tails (#308)
- [x] AGENTS.md + CONTRIBUTING: read TODO+ARCHITECTURE; stale docs = bug
- [x] Fix ARCHITECTURE Seatbelt spike-only vs Implemented; MCP runtime → Partial
      until `impetusd_autoload`; add Execution modes Partial row; sync roadmap
- [x] Second docs audit after modes/RiskGate/Explore/MCP land (#308 §11):
      no prompt_prefix; Explore daemon Implemented; MCP Partial (autoload +
      first-use connect); no `[x]`+Partial tails; CI warm ~85s recorded.

### CI speed

- [x] Enable rust-cache **target** cache on macOS/Linux compile jobs;
      Security stays `cache-targets: false` with separate key
- [x] Move `cargo fmt --check` to Linux; drop macOS dependant `cargo check`
      (Linux already checks `check_packages`)
- [x] `CARGO_PROFILE_*_DEBUG=0` on PR jobs
- [x] Run `scripts/tests/ci-affected.sh` in Detect; expand site/workflow/client cases
- [x] Measure warm-cache PR→Gate: macOS ~77s (clippy 15 + test 37, cache hit),
      Linux ~30s, Gate ~**85s** wall (run 35638437125). Stretch 60–90s met;
      sccache / macos-15 not needed. Cold populate was ~174s.

### Daemon-owned execution modes

- [x] Core `ExecutionMode` + IPC v6 `Set`/`Get` + durable `ExecutionModeChanged`
      + projection + harness handlers + client wrappers (#308)
- [x] Enforce mode in daemon admission (PLAN denies mutations; not prompt text).
      EffectSeam + runtime/tool_orchestrator use session `ExecutionMode`.
- [x] TUI syncs mode via IPC; drop `prompt_prefix`; show daemon-confirmed state.
- [x] Shift+Tab cycles ASK → ACCEPT EDITS → PLAN → AUTO → ASK; Tab unchanged;
      F4 mode picker secondary; slash `/mode` `/plan` `/ask` `/auto`.

### Auto Risk Gate

- [x] Core `DeterministicRiskGate` + `EffectSeam` admission chain (sandbox →
      policy hard deny → execution mode → RiskGate → decision) with adversarial
      unit tests (#308)
- [ ] Production-wide RiskGate coverage (all spawn paths, hook_prefilter wired
      separately as performance-only prefilter).
      **Done:** runtime + tool_orchestrator + process argv + IPC read tools use
      session-aware seam; AUTO/ACCEPT_EDITS auto-allow safe reads + scoped
      workspace edits; risky paths need approval; hard denies still win.
- [ ] Wire `hook_prefilter` into live `ProcessExecution` as performance hook
      only (not security classifier).
      **Done:** spawn path calls prefilter; docs say RiskGate ≠ HookPrefilter.

### Keychain / non-interactive

- [x] CI / `IMPETUS_NONINTERACTIVE` fail closed without Keychain GUI;
      `IMPETUS_CREDENTIAL_BACKEND=mock|keychain`; unit test; operator docs (#308).
- [ ] Diagnose intermittent `impetusd` Keychain GUI prompt after rebuild
      (service/account/ACL/signing). No permissive ACL / `-A` / plaintext.

### Runtime gaps (honest open items)

- [x] Wire `ExploreChildRunner` / parent-resume into **production** `impetusd`
      provider path (`explore_spawn` via daemon wiring + default provider).
- [x] `impetusd` autoload MCP servers from disk config into
      `ToolProviderRuntime` (`$IMPETUS_DATA_DIR/mcp/*.json`; fail closed on bad
      config; `impetusd_autoload: true`).
- [ ] Typed IPC `ReloadPolicyConfig` (startup file load already exists).
      **Done:** invalid reload keeps previous policy + durable/audit event.

---

## Next

Important after Now; not blocking daily single-session use.

### Orchestration

- [ ] Live agent process spawn from `WorkflowEngine` / scheduler
- [ ] `WorkflowEngine` cancel/replace on session-run intents (drain race open)
- [ ] Live child process / PTY for Research / Build / Review
- [ ] Per-parent concurrency caps / fair scheduling

### Trust / policy / provider

- [ ] `PolicyStore` type + governed-instruction surface
      (`Runtime ≠ Memory ≠ Policy`; not PolicyConfig overrides)
- [ ] Operator UX to edit/customize policy (not a harness UI rewrite)
- [ ] Live provider wire for `SteerRewrite` (passthrough/mock today) (#285)
- [ ] Clickable live subagent / child-run surfaces in TUI (needs Explore daemon)

### Optional coding / research backends

- [ ] Real LSP process spawn (not a core dep) (#282)
- [ ] LSP / coding-tool TUI beyond IPC `GotoDefinition` (#267)
- [ ] Real Tavily/Exa HTTP clients (seam exists) (#264)
- [ ] Real browser automation behind `BrowserProvider` (seam exists) (#268)

---

## Later

Deferred / explicit non-goals near-term.

- [ ] Cross-machine / IPC orchestration beyond in-process fanout
- [ ] Large multi-team / swarm beyond small WorkflowEngine recipes
- [ ] Plugin marketplace / large plugin ABI
- [ ] Portable sessions between harnesses
- [ ] Deep Claude/Codex/Cursor **runtime** compatibility (import adapters Partial)
- [ ] Autonomous long-running planner/tester loops
- [ ] Ubuntu 24.04 release tier + clean-machine smoke automation (#293)
- [ ] Full Zap discovery/authorize production protocol (#290)
- [ ] Invert `impetus-core` → `impetus-acp-gateway` dependency
- [ ] Thin-client / `harness_api` domain split (recoverable ≠ daemon panic)
- [ ] CLI migration: keep `impetus` primary; migrate `impetus-cli` over time
      (do **not** delete the crate)

---

## Frontends

TUI talks `HarnessClient` only. Detail:
[docs/reference/tui-ux-audit.md](docs/reference/tui-ux-audit.md).

Open items above under Now (modes/hotkeys) and Next (policy UX, child surfaces).

Done (evidence in ARCHITECTURE / crate tests — do not re-litigate):
Ratatui+Crossterm (#137), markdown (#146), diff (#148), approval UI (#165/#169),
session picker (#166/#169), palette (#175), scrollback (#178), redraw (#179),
Zap honesty docs (#290), modern harness hotkeys (#302), theme pack (#304),
Explore library + daemon AgentLoop wire (#306 / #308).

---

## Notes

| Doc | Role |
| --- | --- |
| [ARCHITECTURE.md](ARCHITECTURE.md) | Capability matrix + invariants |
| [docs/architecture/roadmap.md](docs/architecture/roadmap.md) | Now / Next / Later narrative |
| [docs/architecture/kernel-invariants.md](docs/architecture/kernel-invariants.md) | Kernel rules |
| [docs/guides/](docs/guides/) | Getting started, config, CI, Ubuntu smoke |
| [docs/reference/](docs/reference/) | Protocols, TUI audit, components |
| [docs/archive/](docs/archive/) | Historical audits (not current truth) |

**Baseline CI (pre-#308, cold cache examples on recent Rust PRs):**

| Run | PR→Gate | Detect | macOS | Linux |
| --- | ---: | ---: | ---: | ---: |
| #307 explore | ~176s | 7s | 153s | 80s |
| #304 themes | ~222s | 6s | 198s | 70s |
| #296 path | ~248s | 5s | 223s | 57s |
| #296 ci-fast | ~153s | 6s | 131s | 64s |

macOS #307 steps (cold, `cache-targets: false`): toolchain ~7s, rust-cache
restore miss ~0s, fmt 1s, clippy 38s, test 63s, dependant check 24s, cache
save ~11s. Critical path = macOS.

Archaeology: [docs/archive/todo-audit-2026-08-30.md](docs/archive/todo-audit-2026-08-30.md).
