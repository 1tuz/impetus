# Roadmap

Canonical **task list**: [TODO.md](../../TODO.md) (Now / Next / Later).

Canonical **architecture + capability matrix**: [ARCHITECTURE.md](../../ARCHITECTURE.md).

This file stays short on purpose. Do not duplicate checkboxes here.

## Priority model

1. **Now** ([#308](https://github.com/1tuz/impetus/issues/308)) — largely landed
   on the feature branch: faster PR CI (~85s warm Gate), honest docs memory,
   daemon-owned execution modes + RiskGate, Keychain non-interactive + rebuild
   diagnosis docs, Explore + MCP autoload in `impetusd`, PolicyConfig IPC
   reload, live `hook_prefilter` on process spawn. Remaining Now items move to
   Next as they complete (see [TODO.md](../../TODO.md)).
2. **Next** — operator / orchestration runtime and optional modules: live
   WorkflowEngine spawn + cancel, other subagent roles, `PolicyStore`,
   daemon hook_prefilter catalog file load, Steer live rewrite, real
   LSP/search/browser backends, policy operator UX. Capability leases / RepoMap:
   design only — [capability-leases-and-repomap.md](capability-leases-and-repomap.md).
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

Decision chain (target): hard Policy → ExecutionMode → RiskGate → Approval →
Sandbox → Capability → Execution.

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
