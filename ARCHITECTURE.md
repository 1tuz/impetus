# Архитектура Impetus

Это canonical архитектурный документ. Он отделяет текущий код от product target;
исполняемый порядок работ — в [docs/ROADMAP.md](docs/ROADMAP.md).

## Product invariant

Impetus — terminal-first, local-first, all-in-one Agent Harness for
Engineering. Он объединяет durable sessions/events, agent/tool orchestration,
safety, credentials и execution authority за заменяемыми client surfaces.
`impetusd` — единственный владелец authoritative durable runtime/state.
Клиенты не владеют SQLite, policy, model runtime, tool runtime, credentials или
authoritative session state.

## Workspace today

```text
impetus/
├── crates/
│   ├── impetus-core/          durable domain/runtime foundation
│   ├── impetus/               headless daemon + Unix socket server
│   ├── impetus-client/        HarnessClient + local transports
│   ├── impetus-cli/           current command/JSON reference client
│   ├── impetus-zap-adapter/   experimental historical integration baseline
│   └── impetus-acp-gateway/   ACP gateway library
├── config/                    capability and provider configuration
├── docs/                      contracts, roadmap and historical snapshots
├── Cargo.toml                 workspace membership and shared pins
└── ARCHITECTURE.md            this canonical map
```

## CURRENT

```text
impetus-cli / Zap adapter
          │ typed local IPC
          ▼
       impetusd
          │
          ▼
     impetus-core
events · policy · SQLite · providers · tools · artifacts
```

Current code provides durable events, policy/approval, a versioned Unix-socket
protocol, `HarnessClient`, provider registry foundation, copied-event forks,
and a command/JSON client. The Zap adapter renders structured Blocks/OSC as an
experimental baseline; it is not the target integration architecture. There is
no standalone TUI yet, no full Session DAG, no model router, and no complete
remote agent execution flow.

## TARGET system

```text
                    ┌──────────────────┐
                    │      Zap UI      │
                    │ Impetus backend  │
                    └────────┬─────────┘
                             │
Terminal / SSH               │
      │                      │
      ▼                      │
┌───────────────┐            │
│ impetus CLI / │            │
│ TUI           │            │
└──────┬────────┘            │
       └──────────┬──────────┘
                  ▼
            HarnessClient
                  │
                  ▼
               impetusd
                  │
      ┌───────────┼────────────┐
      │           │            │
 Agent Runtime  Context      Safety
 Tool Loop      Router       Capabilities
 Sessions      Repo Intel    Sandbox
 Swarm         Artifacts     Execution
 Self-Repair   Memory
      │
      ▼
Durable Event Store
```

The upper subsystems are target architecture unless a roadmap gate says they
exist. A TUI framework is deliberately not fixed: reuse of jcode ideas is
evaluated first; a thin Rust TUI may use Ratatui/Crossterm if justified.

## Ownership and safety

Every typed action has `origin=user|agent` and follows:

```text
Policy → Deny | Allow | NeedsApproval → Sandbox → Capability → Execution
```

Only `Allow` or a human-approved request proceeds. A model cannot create
`origin=user` or approve itself. Disconnect/crash never turns an unknown
outcome into `Completed`; durable history is replayed on reconnect.

Secrets remain only in macOS Keychain. SQLite, JSONL, tracing, events, and
tests hold opaque references, never tokens, private keys, or passphrases.

## Clients

The standalone path is `cd project && impetus`: a future first-class CLI/TUI
connects through `HarnessClient` to `impetusd`. It is for ordinary terminal
emulators, SSH, and environments without Zap.

Zap uses its own terminal/agent UI. Its MVP is local discovery, explicit
Connect/Authorize, connected/disconnected state, selection of Impetus as agent
backend, and forwarding agent requests. It does not duplicate sessions,
approvals, model state, a renderer, or a custom Blocks/status-bar protocol.
The existing adapter remains historical/experimental evidence only.

Impetus is not a terminal emulator. PTY/ANSI parsing, tabs, scrollback, and
renderer concerns remain client concerns until a proven requirement says
otherwise.

## Current maturity and target boundaries

- **Provider layer:** `ModelProvider` and `ProviderRegistry` are current
  foundations. Router selection by capability, health, cost, latency, privacy,
  context, cache, budget, and reasoning need is target work.
- **Context:** copied-event forks plus compaction/budget primitives are current.
  Shared-prefix Session DAG, checkpoints, restore/revert, and branch-aware
  sessions are target work.
- **Remote:** SSH/tmux/PTY/SFTP models and safety boundaries exist; a controlled
  end-to-end remote agent flow is target work.
- **ACP:** an external coding-agent adapter, not universal provider auth or
  client IPC. The selected agent CLI owns its authorization.
- **Distribution:** macOS Apple Silicon is current focus; Ubuntu 24.04 x86_64
  is a target tier, not a present support promise.

The visual [request control flow](docs/architecture-map.html) explains one
safety path. It is deliberately not a full system map.
