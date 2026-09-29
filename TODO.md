# TODO

Open work only. Architecture truth → [ARCHITECTURE.md](ARCHITECTURE.md).
Roadmap → [docs/architecture/roadmap.md](docs/architecture/roadmap.md).

**Rules**

- `[x]` only for production daemon/client path (not library/mock-only).
- No `[x]` with a Partial tail — split done vs remaining.
- Update this file + ARCHITECTURE in the same change. Stale docs = bug.
- Capability first; TUI/Desktop are presentation over `HarnessClient`.

| Status | Meaning |
| --- | --- |
| Implemented | Prod `impetusd` / client + tests |
| Partial | Incomplete surface; Remaining is exact |
| Planned | Not started |

---

## Now

- [ ] crates.io publish of `impetus-extension-sdk` (git `rev` pin recipe shipped)

---

## Next

Actionable after Now. Not priority theatre.

### Runtime reliability (#397)

- Effect Fence call sites: primary ToolOrchestrator write/bash + mutating MCP
  fenced (`#407`). Remaining: host_process operate, agent PTY, remote
  SSH/SFTP/tmux, direct ProcessExecution Allow outside orchestrator
- ObservationPack + Evidence Anchors compaction: shipped `#406` (pack +
  evidence-preserving reduce + durable compaction anchors; raw recover via
  ArtifactStore / EventStore)
- Eval CLI + ExperimentCapsule: shipped `#408` (offline mock fixtures,
  capsule digest, role stubs; promotion never automatic)
- Flight Recorder receipts + observe-only replay: shipped `#413`
  (`impetus receipt export` / `impetus replay`; EventStore projection only;
  no EffectSeam re-execute)

### Extension split follow-through

- [ ] Stand up `impetus-extensions` repo against contract + demo packs
- [ ] CLI `extension *` package path via daemon `Install`/`Remove` IPC
      (legacy Skill/MCP offline FS remains until migrated; best-effort
      `ReloadExtensionPackages` when sock live)

### Daemon / protocol

- [ ] ACP live reconnect polish after cancel/crash (stream/registry/health/#335 landed)

### Clients

- [ ] TUI: sequence picker polish
- [ ] Desktop: model picker / worktrees UI polish; PtyList attach picker
      (PTY dock already on harness; Core `PtyList` shipped #395)
- [ ] Desktop: delete any remaining local spawn if still present; prefer
      bundled `impetusd` path via `impetus-daemon-control`

### Module Runtime debt

- [ ] Remove frozen Module Runtime library stack
      (`module_registry` / `module_lifecycle` / `module_ipc` / `test-module`)
      after Nightly confirms no external callers — keep `module_fallback`

---

## Later

| Item | Note |
| --- | --- |
| Cross-machine orchestration | Parked |
| Multi-team swarm beyond Workflow recipes | Won't near-term |
| Plugin marketplace / large plugin ABI | Won't — daemon package Install/Remove + CLI `extension *` stays |
| Portable sessions between harnesses | Parked |
| Deep Claude/Codex/Cursor runtime compat | Import adapters only |
| Long-running planner/tester loops | Parked |
| Ubuntu clean-machine smoke automation | Checklist #293; automation Parked |
| Full Zap discovery/authorize | Checklist #290; production Parked |
| Invert core → acp-gateway dependency | Parked mega-refactor |
| Full thin-client domain split | Gradual PROTO; mega-split Parked |
| Extension `MemoryProvider` | Optional operate (`memory/recall|store`); core MemoryStore remains session SoT (#363) |
| macOS Instruments/authd proof | Parked — SIP interactive tooling; unit+userspace E2E cover no-sudo paths |
| Custom TUI ANSI emulator | Won't — PTY passthrough only |
| Hidden chain-of-thought UI | Won't — summary/intent only |
| Full LSP protocol (entire LSP spec in core) | Won't — shipped coding_* + `LspIntegration` / `ProcessLspBackend` (#391); language packs in extensions |
| Browser CDP/WebDriver in core | Won't — health/negotiate + `BrowserIntegration` shipped (#391); CDP bridges only in extensions |
| host_process full EffectSeam / Seatbelt | Parked — operate stays permission + secret-key reject (#395 honesty) |

---

## Docs

| Doc | Role |
| --- | --- |
| [ARCHITECTURE.md](ARCHITECTURE.md) | Capability matrix + invariants |
| [docs/architecture/roadmap.md](docs/architecture/roadmap.md) | Narrative Now / Next / Later |
| [docs/guides/](docs/guides/) | Getting started, config, CI |
| [docs/reference/](docs/reference/) | Protocols, TUI, components |
| Sibling [impetus-desktop/TODO.md](../impetus-desktop/TODO.md) | Desktop presentation backlog |

Shipped program slices: [#315](https://github.com/1tuz/impetus/issues/315) harness unify,
[#320](https://github.com/1tuz/impetus/issues/320) production harden baseline — follow-up [#322](https://github.com/1tuz/impetus/issues/322).
Daemon Unix E2E: approvals / MCP mutate / Files-Diff (`daemon_unix_approvals_mcp_files`);
provider-option persist (`daemon_unix_e2e`); ACP permission (`daemon_unix_acp_permission`);
extension host_process operate (`daemon_unix_extensions` + `OperateExtensionPackage`).
Daemon SoT close (#395): `PtyList` + package Install/Remove IPC; Module Runtime Deprecated.
