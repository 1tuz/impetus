# Roadmap

Canonical **task list**: [TODO.md](../TODO.md) (P0 / P1 / P2).

Canonical **architecture + capability matrix**: [ARCHITECTURE.md](../ARCHITECTURE.md).

This file stays short on purpose. Do not duplicate checkboxes here.

## Priority model

1. **P0 — Runtime correctness** — provider-native wiring, arg validation, artifact
   durability truth, durable compaction, PR-critical security tests, honest doctor.
2. **P1 — Operator / extension / orchestration** — versioned schemas, extension
   lifecycle with ownership, `Runtime ≠ Memory ≠ Policy`, WorktreeManager,
   WorkflowEngine + small recipes, small role set (Explore/Research/Build/Review),
   live MCP, LSP, anti-sprawl.
3. **P2 — Advanced orchestration** — marketplaces, multi-harness portability,
   deep vendor runtime parity, large swarm/team loops.

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
- **Seatbelt profiles**: spike/evidence only until wired into process execution.
  **Priority:** production macOS Seatbelt matters more than broad cross-platform
  sandbox backends.
- **Linux x86_64**: install target; sandbox/Keychain parity Planned.
  Clean-machine Ubuntu 24.04 smoke proofs vs PR CI:
  [ubuntu-smoke-checklist.md](ubuntu-smoke-checklist.md) (#293; automated
  smoke still Planned).
- **Windows**: not a current target.

## CLI

- **`impetus`**: primary user-facing CLI/TUI.
- **`impetus-cli`**: legacy/secondary; keep for existing workflows; migrate
  callers over time (do not delete).

## Known debt (postponed)

- Prefer invert `impetus-core` → `impetus-acp-gateway` dependency; large refactor
  postponed. See [TODO.md](../TODO.md) P2.
