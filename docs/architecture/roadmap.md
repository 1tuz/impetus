# Roadmap

Canonical **task list**: [TODO.md](../../TODO.md) (Now / inventory / Later).

Canonical **architecture + capability matrix**: [ARCHITECTURE.md](../../ARCHITECTURE.md).

This file stays short on purpose. Do not duplicate checkboxes here.

## Priority model

1. **Now** — pick next slice from [TODO.md](../../TODO.md) **Now/Next**
   (crates.io SDK publish, extension repo, client polish). Kernel + daemon SoT
   path stable; IPC negotiate **12..=15**.
2. **Sibling desktop** ([#310](https://github.com/1tuz/impetus/issues/310)) —
   thin shell in [`impetus-desktop`](../../../impetus-desktop); harness IPC
   **v12..=15** (`PtyList`, `extension_manage` install/remove). Presentation
   backlog = desktop `TODO.md` (PtyList attach picker, model/worktrees polish).
3. **Later** — marketplaces, multi-harness portability, deep vendor runtime
   parity, large swarm/team loops, Ubuntu clean-machine automation, full Zap
   authorize, ACP dependency invert, mega thin-client split, CLI migration.

Shipped: [#308](https://github.com/1tuz/impetus/issues/308) +
[#311](https://github.com/1tuz/impetus/issues/311) (IPC negotiate baseline);
[#315](https://github.com/1tuz/impetus/issues/315) harness unify;
[#320](https://github.com/1tuz/impetus/issues/320) production harden;
[#322](https://github.com/1tuz/impetus/issues/322) production-harden follow-up;
[#395](https://github.com/1tuz/impetus/issues/395) PtyList + package Install/Remove
+ Module Runtime freeze.

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

- **macOS**: primary development; platform suite on **Nightly Full**.
- **Linux**: **PR Fast** = Ubuntu `git diff --check` + `cargo fmt` + affected
  `cargo check` only (no clippy/tests on PR). Nightly = full quality.
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
