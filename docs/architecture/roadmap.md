# Roadmap

Canonical **task list**: [TODO.md](../../TODO.md) (Now / Next / Later).

Canonical **architecture + capability matrix**: [ARCHITECTURE.md](../../ARCHITECTURE.md).

This file stays short on purpose. Do not duplicate checkboxes here.

## Priority model

1. **Now** — work that unblocks daily production use: Explore→daemon entry,
   live MCP in the loop, PolicyConfig IPC reload, live Explore parent-resume
   on daemon path. (Seatbelt macOS process wrap + PolicyConfig startup load
   + Explore library E2E are in tree — see capability matrix.)
2. **Next** — operator / orchestration runtime and optional modules: live
   WorkflowEngine spawn + cancel, other subagent roles, `PolicyStore`, hooks on
   process spawn, Steer live rewrite, real LSP/search/browser backends, policy
   operator UX. Capability leases / RepoMap: design only —
   [capability-leases-and-repomap.md](capability-leases-and-repomap.md).
3. **Later** — marketplaces, multi-harness portability, deep vendor runtime
   parity, large swarm/team loops, Ubuntu clean-machine automation, full Zap
   authorize, ACP dependency invert, thin-client split, CLI migration.

## Kernel (do not dilute)

`EventStore` + `DurableArtifactStore` + `Policy` + `Approval` + `Sandbox` +
`Executor`.

Replaceable above:

```text
ProviderProtocol → ContextEngine → ToolOrchestrator
  → AgentScheduler + WorkflowEngine + WorktreeManager
  → ExtensionGateway
```

## Platform

- **macOS**: primary development and PR CI (`macos-14` fmt/clippy/tests).
- **Linux**: PR CI compile guard (`ubuntu-24.04` `cargo check`); install target;
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
