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

Production-harden / Next items. Extension package host (#324) core path is on
branch `feature/issue-324-extension-sdk-abi` — merge via PR when ready.

- [ ] crates.io publish of `impetus-extension-sdk` (git `rev` pin recipe shipped)
- [ ] Richer `host_process` operate surface beyond initialize/shutdown (tools)

---

## Next

Actionable after Now. Not priority theatre.

### Extension split follow-through

- [ ] Stand up `impetus-extensions` repo against contract + demo packs
- [ ] Unify legacy CLI Skill/MCP SoT vs package host (allowlist `#296`)

### Daemon / protocol

- [ ] Schema: broader validate-on-wire coverage
- [ ] AttachmentStore: bind `GetAttachment` to session; TUI fetch path
- [ ] Child mid-run action stream (progress / tool detail on parent log)
- [ ] Same-uid peer isolation beyond connection-bind (enforce `IMPETUS_ACP_CHILD` /
      peer-cred; umask before socket bind) — residual trust model, not #322 blocker

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
| Full LSP protocol | Parked beyond spawn + goto/hover + crash respawn; also request cancel / diagnostics push |
| macOS Instruments/authd proof | Parked — SIP interactive tooling; unit+userspace E2E cover no-sudo paths |
| Custom TUI ANSI emulator | Won't — PTY passthrough only |
| Hidden chain-of-thought UI | Won't — summary/intent only |
| Browser CDP/WebDriver bridges | Parked |

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
