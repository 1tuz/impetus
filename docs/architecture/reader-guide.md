# Architecture reader guide

[ARCHITECTURE.md](../../ARCHITECTURE.md) — canonical architecture.
[README.md](../../README.md) — product one-liner + Quick Start.

## Layers (memorize this)

```text
Clients  →  HarnessClient / IPC  →  impetusd
                                      ├─ Runtime services
                                      └─ Trusted Kernel
                                           └─ Extension Host (replaceable)
```

| Term | Meaning |
| --- | --- |
| Trusted Kernel | Policy / Approval / Sandbox / Executor / EventStore / secrets-by-ref |
| Runtime services | AgentLoop, Context, Tools, Providers, Workflows, Worktrees, … |
| `impetus-core` | Rust crate (kernel **and** runtime libs) — not a synonym for Kernel |
| `impetusd` | Authoritative process; **lazy-started** by `impetus` for ordinary UX |

## Binary topology

```text
impetus       → user-facing CLI / TUI (`impetus ui`) — ensures daemon
impetusd      → authoritative daemon process
impetus-core  → libraries (no binary)
```

## CURRENT crates

| Component | Path | Responsibility |
| --- | --- | --- |
| Core libs | `crates/impetus-core` | Events, runtime, policy, effects, providers, tools, IPC types |
| Daemon | `crates/impetusd` | Unix-socket server, provider/ACP profiles, Keychain resolver |
| CLI | `crates/impetus` | User commands + lazy daemon start + `doctor` / `ui` |
| TUI | `crates/impetus-tui` | Ratatui library for `impetus ui` |
| Client | `crates/impetus-client` | `HarnessClient`, transports |
| ACP | `crates/impetus-acp-gateway` | External ACP agents |
| Zap | `crates/impetus-zap-adapter` | Experimental Zap baseline |

## Trust boundary

```text
origin=user|agent → Policy → Approval? → Sandbox → Capability → Execution → Event
```

Credentials: opaque Keychain references only (macOS).

## Related

| Topic | Document |
| --- | --- |
| Component matrix | [ARCHITECTURE.md](../../ARCHITECTURE.md) |
| Kernel invariants | [kernel-invariants.md](kernel-invariants.md) |
| Diagrams | [system-architecture.svg](../../assets/readme/system-architecture.svg), [execution-flow.svg](../../assets/readme/execution-flow.svg) |
| Extensions | [EXTENSION_REPOSITORY_CONTRACT.md](../../EXTENSION_REPOSITORY_CONTRACT.md) |
| Backlog | [TODO.md](../../TODO.md), [roadmap.md](roadmap.md) |
