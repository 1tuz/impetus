# Impetus Architecture

**Impetus** is a policy-centered local runtime for autonomous coding agents on macOS.

## Core Principles

- **Durable events first**: SQLite WAL event log, survive restart
- **Policy-gated execution**: every action goes through `Policy → Sandbox → Capability → Execution`
- **Versioned IPC**: Unix domain socket with protocol negotiation
- **Fail-closed sandbox**: macOS Seatbelt profiles, no execution without explicit capability
- **Secrets in Keychain only**: never in SQLite, logs, or tracing
- **Module runtime**: pluggable backends with external process isolation

## Architecture

```
┌─────────────────┐
│  impetus (CLI)  │  User-facing client
└────────┬────────┘
         │ Unix socket (versioned IPC)
         ▼
┌─────────────────────────────────────────┐
│          impetusd (daemon)              │
│  ┌─────────────────────────────────┐   │
│  │     Harness (Policy Kernel)     │   │
│  │  ┌──────────┐  ┌─────────────┐  │   │
│  │  │ Policy   │  │ Approval    │  │   │
│  │  │ Engine   │  │ Resolver    │  │   │
│  │  └──────────┘  └─────────────┘  │   │
│  └─────────────────────────────────┘   │
│  ┌─────────────────────────────────┐   │
│  │      Agent Loop & Tools         │   │
│  │  ┌──────────┐  ┌─────────────┐  │   │
│  │  │ Model    │  │ Tool        │  │   │
│  │  │ Provider │  │ Orchestrator│  │   │
│  │  └──────────┘  └─────────────┘  │   │
│  └─────────────────────────────────┘   │
│  ┌─────────────────────────────────┐   │
│  │    Module Runtime (Phase 2)     │   │
│  │  ┌──────────┐  ┌─────────────┐  │   │
│  │  │ Module   │  │ External    │  │   │
│  │  │ Registry │  │ IPC         │  │   │
│  │  └──────────┘  └─────────────┘  │   │
│  └─────────────────────────────────┘   │
│  ┌─────────────────────────────────┐   │
│  │  Extension Compat (Phase 3)     │   │
│  │  ┌──────────┐  ┌─────────────┐  │   │
│  │  │ Canonical│  │ Import      │  │   │
│  │  │ Types    │  │ Adapters    │  │   │
│  │  └──────────┘  └─────────────┘  │   │
│  └─────────────────────────────────┘   │
│                                         │
│  ┌─────────────────────────────────┐   │
│  │    Durable Storage              │   │
│  │  ┌──────────┐  ┌─────────────┐  │   │
│  │  │ SQLite   │  │ Keychain    │  │   │
│  │  │ Event Log│  │ (secrets)   │  │   │
│  │  └──────────┘  └─────────────┘  │   │
│  └─────────────────────────────────┘   │
└─────────────────────────────────────────┘
         │
         ▼
┌─────────────────┐
│  Sandbox        │  macOS Seatbelt
│  (fail-closed)  │  Shell, FS, Network
└─────────────────┘
```

## Key Components

### Harness (Policy Kernel)
- **PolicyEngine**: `Action → Deny | Allow | NeedsApproval`
- **ApprovalResolver**: human-in-the-loop decisions
- **EventStore**: durable SQLite WAL log with cursor backfill
- **Sandbox integration**: macOS Seatbelt enforcement

### Agent Loop
- **ModelProvider**: OpenAI-compatible, local, cloud
- **ToolOrchestrator**: tool execution through policy pipeline
- **BudgetChecker**: token/time tracking per session
- **InstructionResolver**: layered context (global/project/session)

### Module Runtime (Phase 2)
- **ModuleRegistry**: discover, probe, lifecycle
- **External IPC**: Unix socket, separate process isolation
- **FallbackPolicy**: FailFast, Retry, Alternate, Degrade, SafeDefault
- **UnknownOutcome safety**: blocks retry for mutating operations

### Extension Compatibility (Phase 3)
- **Canonical types**: ModuleSpec, Skill, Instruction, Profile, Command, MCP
- **ExtensionAdapter**: import from Agent Skills, MCP, Cursor, Codex, Claude, etc.
- **Compatibility matrix**: SUPPORTED | PARTIAL | UNSUPPORTED | INCOMPATIBLE
- **ExtensionRegistry**: multi-source module registration

### Storage
- **EventStore**: SQLite WAL, schema migrations, event replay, and append-only
  session branches. A branch stores parent/fork metadata plus its local suffix;
  logical replay reads the immutable shared prefix and suffix without copying
  prefix rows.
- **Session checkpoints**: durable named sequence references. Restore/revert
  creates a new branch head and never deletes or rewrites historical events.
- **ArtifactStore**: bounded ephemeral/in-memory backing (durable planned)
- **ReferenceStore**: YAML-based partitioned storage for long-term agent reference data (Tempo worklogs, past decisions, project patterns)
- **Keychain**: macOS-native secret storage, never in SQLite

## Security Model

1. **Origin tracking**: every action has `origin=user|agent`
2. **Policy decision**: before execution, not after
3. **Sandbox enforcement**: fail-closed, no execution without capability
4. **Secret isolation**: Keychain API only, redacted in logs/events
5. **Approval flow**: typed approvals for destructive/sensitive operations

## Development Phases

- **Phase 0**: Foundation (done) — IPC, policy, events, sandbox
- **Phase 1**: Binary topology & diagnostics (done) — doctor, daemon discovery
- **Phase 2**: Module runtime (done) — external IPC, fallback policies, tests
- **Phase 3**: Extension compatibility (done) — canonical types, adapters
- **Phase 3.5**: VimTrap architecture (done) — profile system, service providers, kernel invariants
- **Phase 4**: Output optimization — structured observations, RTK integration
- **Phase 5**: Agent runtime — real loop, tool execution, web research
- **Phase 6**: Context & sessions — lazy loading, artifact store, fork/checkpoint
- **Phase 7**: TUI — standalone client with Ratatui
- **Phase 8**: Integrations — Zap backend, credential UI, policy customization
- **Phase 9**: Remote & platform — SSH/tmux, Ubuntu support
- **Phase 10**: Security & verification — audit, end-to-end tests

## Documentation

- **Detailed architecture** (Russian): [ARCHITECTURE.ru.md](ARCHITECTURE.ru.md)
- **Kernel invariants**: [docs/KERNEL_INVARIANTS.md](docs/KERNEL_INVARIANTS.md)
- **VimTrap principle**: [docs/VimTrap_Implementation_Plan.md](docs/VimTrap_Implementation_Plan.md)
- **Roadmap**: [docs/ROADMAP.md](docs/ROADMAP.md)
- **TODO**: [TODO.md](TODO.md)

## References

- macOS Sandbox: [docs/MACOS_SANDBOX_SPIKE.md](docs/MACOS_SANDBOX_SPIKE.md)
- TUI design: [docs/TUI_REFERENCE.md](docs/TUI_REFERENCE.md)
- Development guide: [docs/development.md](docs/development.md)
