# Roadmap

Canonical **task list**: [TODO.md](../TODO.md) (P0 / P1 / P2).

Canonical **architecture + capability matrix**: [ARCHITECTURE.md](../ARCHITECTURE.md).

This file stays short on purpose. Do not duplicate checklists here.

## Priority model

1. **P0 — Runtime correctness** — provider-native wiring, arg validation, artifact
   durability truth, durable compaction, PR-critical security tests, honest doctor.
2. **P1 — Coding-agent capabilities** — subagents, worktrees, steer/follow-up,
   live MCP/Skills, LSP, optional browsers.
3. **P2 — Advanced orchestration** — marketplaces, multi-harness portability,
   deep vendor runtime parity, long-running autonomous loops.

## Kernel (do not dilute)

`EventStore` + `DurableArtifactStore` + `Policy` + `Approval` + `Sandbox` +
`Executor`.

Replaceable above: ProviderProtocol → ContextEngine → ToolOrchestrator →
AgentScheduler → ExtensionGateway.

## Platform

- **macOS**: primary development and PR CI (`macos-14`).
- **Path-scope sandbox**: Implemented and fail-closed.
- **Seatbelt profiles**: spike/evidence only until wired into process execution.
- **Linux x86_64**: install target; sandbox/Keychain parity Planned.
- **Windows**: not a current target.
