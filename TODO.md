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
- [ ] IPC surface for `ExtensionHost::operate` (host_process operate RPC is in
      core/SDK; clients still go through future typed IPC)

---

## Next

Actionable after Now. Not priority theatre.

### Extension split follow-through

- [ ] Stand up `impetus-extensions` repo against contract + demo packs

### Daemon / protocol

- [ ] ACP live reconnect polish after cancel/crash (stream/registry/health/#335 landed)

### Clients

- [ ] TUI: sequence picker polish
- [ ] Desktop: PTY UI, model picker, worktrees UI; drop any remaining local
      MCP/provider config parse (daemon SoT only)

---

## Later

| Item | Note |
| --- | --- |
| Cross-machine orchestration | Parked |
| Multi-team swarm beyond Workflow recipes | Won't near-term |
| Plugin marketplace / large plugin ABI | Won't — CLI `extension *` stays |
| Portable sessions between harnesses | Parked |
| Deep Claude/Codex/Cursor runtime compat | Import adapters only |
| Long-running planner/tester loops | Parked |
| Ubuntu clean-machine smoke automation | Checklist #293; automation Parked |
| Full Zap discovery/authorize | Checklist #290; production Parked |
| Invert core → acp-gateway dependency | Parked mega-refactor |
| Full thin-client domain split | Gradual PROTO; mega-split Parked |
| Full LSP protocol | Parked beyond spawn + goto/hover/symbols + cancel + diagnostics cache; multi-language installers via extension `LspIntegration` operate (#362) |
| Extension `MemoryProvider` | Optional operate (`memory/recall|store`); core MemoryStore remains session SoT (#363) |
| macOS Instruments/authd proof | Parked — SIP interactive tooling; unit+userspace E2E cover no-sudo paths |
| Custom TUI ANSI emulator | Won't — PTY passthrough only |
| Hidden chain-of-thought UI | Won't — summary/intent only |
| Browser CDP/WebDriver bridges | Parked — extension `BrowserIntegration` only; core keeps negotiate/health Absent (#336) |

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
Daemon Unix E2E gaps (#357): approvals / MCP mutate / Files-Diff landed in
`daemon_unix_approvals_mcp_files`; GOAL §4 still open for provider-option persist
(#328) and ACP permission mapping with mock agent.
