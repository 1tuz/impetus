# Roadmap

Canonical **task list**: [TODO.md](../../TODO.md) (Now / inventory / Later).

Canonical **architecture + capability matrix**: [ARCHITECTURE.md](../../ARCHITECTURE.md).

This file stays short on purpose. Do not duplicate checkboxes here.

## Priority model

1. **Now** — [#315](https://github.com/1tuz/impetus/issues/315) harness unify:
   shared capabilities/events for TUI + Desktop; daemon-owned Git/Files/Diff/
   activity/PTY; thin protocol boundary; doc honesty (README Seatbelt/MCP).
   Detail + inventory = [TODO.md](../../TODO.md).
2. **Sibling desktop** ([#310](https://github.com/1tuz/impetus/issues/310)) —
   thin shell in [`impetus-desktop`](../../../impetus-desktop); already IPC **v7**.
   Must consume new harness Git/MCP/Files APIs (drop local `git_*` /
   `list_mcp_servers`). Presentation backlog = desktop `TODO.md`.
3. **Later** — marketplaces, multi-harness portability, deep vendor runtime
   parity, large swarm/team loops, Ubuntu clean-machine automation, full Zap
   authorize, ACP dependency invert, mega thin-client split, CLI migration.

Shipped baseline on `main`: [#308](https://github.com/1tuz/impetus/issues/308) +
[#311](https://github.com/1tuz/impetus/issues/311) (IPC v7, modes/RiskGate,
WorkflowRuntime, Explore, MCP/hooks autoload, policy IPC, SteerRewrite).

## Kernel (do not dilute)

`EventStore` + `DurableArtifactStore` + `Policy` + `Approval` + `Sandbox` +
`Executor`.

Replaceable above:

```text
ProviderProtocol → ContextEngine → ToolOrchestrator
  → AgentScheduler + WorkflowEngine + WorktreeManager
  → ExtensionGateway
```

Decision chain: hard Policy → ExecutionMode → RiskGate → Approval →
Sandbox → Capability → Execution.

Client rule: `capability → impetusd/core → HarnessClient → TUI/Desktop renderer`.
No parallel feature logic in GUI/TUI.

## Platform

- **macOS**: primary development and PR CI (`macos-14` clippy + lib/bin tests).
- **Linux**: PR CI fmt + `cargo check` (`ubuntu-24.04`); install target;
  sandbox/Keychain parity Planned.
- **Path-scope sandbox**: Implemented and fail-closed.
- **Seatbelt profiles**: Implemented on macOS process spawn
  (`execution/sandbox.rs`); non-macOS path-scope only. Linux/Windows OS wrap
  Planned.
- **Linux x86_64**: install target; sandbox/Keychain parity Planned.
  Clean-machine Ubuntu 24.04 smoke proofs vs PR CI:
  [ubuntu-smoke.md](../guides/ubuntu-smoke.md) (#293; automated
  smoke still Planned).
- **Windows**: not a current target.

## CLI

- **`impetus`**: primary user-facing CLI/TUI.
- **`impetus-cli`**: legacy/secondary; keep for existing workflows; migrate
  callers over time (do not delete).

## Known debt (postponed)

- Prefer invert `impetus-core` → `impetus-acp-gateway` dependency; large refactor
  postponed. See [TODO.md](../../TODO.md) Later.
- Gradual `impetus-protocol` is **Now** (#315); full harness_api domain split
  stays Later.
