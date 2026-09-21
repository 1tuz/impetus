# Roadmap

Canonical **task list**: [TODO.md](../../TODO.md) (Now / Next / Later).

Canonical **architecture + capability matrix**: [ARCHITECTURE.md](../../ARCHITECTURE.md).

This file stays short on purpose. Do not duplicate checkboxes here.

## Priority model

1. **Now** ([#308](https://github.com/1tuz/impetus/issues/308)) — make daily use
   coherent: faster PR CI (target cache / fmt off macOS critical path), honest
   docs memory (no Implemented-unwired / `[x]`+Partial), **daemon-owned**
   execution modes + Auto Risk Gate (not TUI prompt prefixes), Keychain
   non-interactive hardening, wire Explore + MCP autoload + PolicyConfig IPC
   reload into **production** `impetusd`. (Seatbelt macOS process wrap,
   PolicyConfig startup load, Explore **library** E2E already in tree.)
2. **Next** — operator / orchestration runtime and optional modules: live
   WorkflowEngine spawn + cancel, other subagent roles, `PolicyStore`, hooks on
   process spawn (performance prefilter ≠ RiskGate), Steer live rewrite, real
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
