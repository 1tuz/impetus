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

- Effect Fence call sites: ToolOrchestrator write/bash + mutating MCP fenced
  (`#407`); session-scoped mutating `OperateExtensionPackage` fenced (`#421`);
  agent-origin PTY spawn fenced (`#428`); agent-origin direct
  `ProcessExecution::execute_with_admission_and_fence` fenced (`#433`;
  User-origin unfenced); agent-origin remote SSH host-key save, mutating
  SFTP Write/Delete, and tmux create/attach/kill fenced (`#434`; User-origin
  and SFTP Read/List unfenced). Remaining: none for listed Effect Fence sites
  (live SSH/SFTP transport still stub)
- host_process operate EffectSeam admission: shipped `#421` (refs `#397`)
- ObservationPack + Evidence Anchors compaction: shipped `#406` (pack +
  evidence-preserving reduce + durable compaction anchors; raw recover via
  ArtifactStore / EventStore)
- Eval CLI + ExperimentCapsule: shipped `#408` (offline mock fixtures,
  capsule digest, role stubs; promotion never automatic)
- Promise/obligation ledger: shipped `#412` (ledger + CompletionGate
  fail-closed on Open required; fulfill allows Accept)
- Flight Recorder receipts + observe-only replay: shipped `#413`
  (`impetus receipt export` / `impetus replay`; EventStore projection only;
  no EffectSeam re-execute)
- Durable Offline Batch + BatchProvider: Implemented `#416`/`#422`/`#427`/`#439`/`#441`/`#445` (parent `#397`) —
  `offline_batch.rs` + `EventPayload::OfflineBatch` journal, `MockBatchProvider`,
  `FsBatchProvider` (local filesystem queue / file-drop — not paid network API),
  idempotent workspace collect, `poll_collect_due` library tick,
  `OfflineBatchRegistry` on Harness + daemon `spawn_offline_batch_poll_loop`,
  CLI `impetus batch submit|status|collect` (`--provider mock|fs` /
  `$IMPETUS_BATCH_PROVIDER`); sock live + mock → typed `AdmitOfflineBatch` IPC;
  sock down / `--provider fs` → offline journal + plan sidecar (+ FS jobs under
  `offline_batch_fs`). Remaining: none for Offline Batch core mechanisms
  (paid Anthropic/OpenAI batch network adapters out of scope)
- DeclaredWriteSet + writer lease/handoff fence: `#417` library + `#429`
  Explore/RoleChild production spawn + `#437` WorkflowRuntime shares same
  daemon `WriterLeaseTable` (role/explore admit+release). Remaining:
  BatchProvider stays separate (`#416`)
- PtyList daemon E2E: shipped `#420` (`daemon_unix_pty_list.rs`)
- host_process EffectSeam on operate: shipped `#421` (optional `session_id`, PLAN deny E2E)

### Extension split follow-through

- [ ] Stand up `impetus-extensions` repo against contract + demo packs
      (CLI package Install/Remove via daemon IPC shipped `#447`)

### Daemon / protocol

- [ ] ACP live reconnect polish after cancel/crash (stream/registry/health/#335 landed)

### Clients

- [ ] TUI: sequence picker polish
- [ ] Desktop: model picker / worktrees UI polish; PtyList attach picker
      (PTY dock already on harness; Core `PtyList` shipped #395)
- [ ] Desktop: delete any remaining local spawn if still present; prefer
      bundled `impetusd` path via `impetus-daemon-control`

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
| host_process Seatbelt wrap on operate RPC | Parked — EffectSeam admission on operate shipped `#421`; macOS Seatbelt on extension child still activate/spawn path |

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
Daemon SoT close (#395): `PtyList` + package Install/Remove IPC; Module Runtime
Removed (#443).
