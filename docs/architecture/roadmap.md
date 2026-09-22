# Roadmap

Canonical **task list**: [TODO.md](../../TODO.md) (Now / inventory / Later).

Canonical **architecture + capability matrix**: [ARCHITECTURE.md](../../ARCHITECTURE.md).

This file stays short on purpose. Do not duplicate checkboxes here.

## Priority model

1. **Now** — post-[#322](https://github.com/1tuz/impetus/issues/322): pick
   next slice from [TODO.md](../../TODO.md) **Next** (ExtensionRuntime →
   AgentLoop skill inject, schema/attachments, client polish). No open
   #322 Now checklist (workflow daemon Unix E2E shipped).
2. **Sibling desktop** ([#310](https://github.com/1tuz/impetus/issues/310)) —
   thin shell in [`impetus-desktop`](../../../impetus-desktop); already IPC **v7**.
   Must consume new harness Git/MCP/Files APIs (drop local `git_*` /
   `list_mcp_servers`). Presentation backlog = desktop `TODO.md`.
3. **Later** — marketplaces, multi-harness portability, deep vendor runtime
   parity, large swarm/team loops, Ubuntu clean-machine automation, full Zap
   authorize, ACP dependency invert, mega thin-client split, CLI migration.

Shipped: [#308](https://github.com/1tuz/impetus/issues/308) +
[#311](https://github.com/1tuz/impetus/issues/311) (IPC v7 baseline);
[#315](https://github.com/1tuz/impetus/issues/315) harness unify;
[#320](https://github.com/1tuz/impetus/issues/320) production harden;
[#322](https://github.com/1tuz/impetus/issues/322) production-harden follow-up.

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
- Gradual `impetus-protocol` / thin-client domain split = **Next/Later**
  (full harness_api mega-split Parked); not current Now.
