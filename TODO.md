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

_Nothing open._ [#320](https://github.com/1tuz/impetus/issues/320) production harden shipped
(no-sudo / silent Keychain, model SoT + `/v1/models`, MCP data-dir SoT + live reload,
worktrees + Explore bridge, IPC 12..=13, docs honesty).

Pick next from **Next**, one issue + one branch.

---

## Next

Actionable gaps (from matrix Remaining). Not priority theatre.

### Daemon / protocol

- [ ] Worktree `List` without `session_id` — empty or scoped catalog
- [ ] ACP: tool/permission broker; push model/reasoning via agent RPC (caps already
      vendor-neutral; no `codex_*` in IPC)
- [ ] Schema: broader validate-on-wire coverage
- [ ] AttachmentStore: bind `GetAttachment` to session; TUI fetch path
- [ ] Child mid-run action stream (progress / tool detail on parent log)

### Clients

- [ ] TUI: sequence / model picker polish
- [ ] Desktop: PTY UI, model picker, worktrees UI; drop any remaining local
      MCP/provider config parse (daemon SoT only)
- [ ] Full socket E2E smoke: approval → git → mcp → pty → detach → reconnect
      (in-process smoke exists; prefer real Unix socket)

### Planned (honest — not fake Partial)

- [ ] MemoryStore control-plane IPC
- [ ] Browser daemon negotiate/health IPC (CDP/WebDriver stays Parked)

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
| Full LSP protocol | Parked beyond spawn + goto/hover |
| Custom TUI ANSI emulator | Won't — PTY passthrough only |
| Hidden chain-of-thought UI | Won't — summary/intent only |

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
[#320](https://github.com/1tuz/impetus/issues/320) production harden — evidence in matrix,
not re-listed here.
