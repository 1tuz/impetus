# Kernel Invariants

**Impetus Kernel** defines non-bypassable security and execution boundaries.

## Core Principle

```
Everything practical is replaceable.
Kernel invariants are not bypassable.
```

Users may replace almost any service (AgentLoop, ModelRouter, Context, Tools, etc.), but **no custom provider can bypass the kernel pipeline.**

## Non-Bypassable Pipeline

Every action flows through this fixed sequence:

```
Action with origin (user | agent)
         ↓
   Policy admission
         ↓
Deny | Allow | NeedsApproval
         ↓
   [if NeedsApproval]
   Human approval gate
         ↓
   Sandbox capability
         ↓
     Execution
         ↓
  Durable outcome/event
```

### 1. Action Origin

Every action carries `origin: ActionOrigin`:

```rust
pub enum ActionOrigin {
    User,    // Explicit user command
    Agent,   // Model-generated action
}
```

**Invariant:** A custom provider cannot forge `origin=User` to bypass policy.

### 2. Policy Admission

`PolicyEngine` evaluates every action before execution:

```rust
pub enum PolicyDecision {
    Allow,
    Deny { reason: String },
    NeedsApproval { prompt: String },
}
```

**Invariant:** Custom policy extensions may influence decisions within allowed bounds, but cannot bypass the admission gate entirely.

### 3. Approval Resolver

For `NeedsApproval` decisions, execution blocks until human input:

```rust
pub enum ApprovalResponse {
    Approved,
    Rejected,
}
```

**Invariant:** No custom service can auto-approve or skip the human gate.

### 4. Sandbox Capability

Execution requires explicit capability grant from macOS Seatbelt sandbox:

```rust
pub enum SandboxProfile {
    NoExecution,
    ReadOnly,
    WorkspaceWrite,
    FullAccess,
}
```

**Invariant:** Custom modules cannot escape sandbox boundaries or elevate privileges.

### 5. Durable Outcome

Every execution produces a durable event in SQLite WAL:

```rust
pub enum ActionOutcome {
    Success { result: ActionResult },
    Failure { error: String },
    UnknownOutcome,  // Non-replayable failure
}
```

**Invariant:** `UnknownOutcome` blocks retry for mutating operations. Custom providers cannot mark a failed mutating action as `Success` or replayable.

### 6. Secrets Boundary

Secrets live **only** in macOS Keychain:

- Never in SQLite
- Never in event logs
- Never in tracing output
- Never in JSON payloads to custom modules

**Invariant:** Custom modules receive only reference tokens, never raw secrets.

## Replaceable vs Non-Replaceable

### Kernel Owns (Non-Replaceable)

- Action origin enforcement
- Policy admission gate
- Approval resolver protocol
- Sandbox enforcement
- UnknownOutcome semantics
- Secrets boundary
- Durable event log integrity
- Module isolation
- IPC protocol compatibility

### Services May Replace (Replaceable)

- Policy decision logic (within admission framework)
- AgentLoop strategy
- Scheduler
- ModelRouter
- ContextService
- ReferenceService
- MemoryService
- ToolProvider implementations
- SearchBackend
- BrowserProvider
- CredentialResolver (reads Keychain references)
- OutputReducer
- RepoIntelligence

## Custom Module Contract

A custom module:

**MAY:**
- Implement service traits (AgentLoopStrategy, etc.)
- Read/write through approved capabilities
- Emit structured outputs
- Request capabilities via module descriptor
- Fail gracefully with fallback

**MAY NOT:**
- Bypass policy admission
- Forge action origin
- Auto-approve NeedsApproval
- Access Keychain raw secrets
- Escape sandbox boundaries
- Rewrite durable outcome of failed mutating action
- Skip IPC protocol negotiation

## Enforcement

- **Compile-time:** Trait bounds, type system
- **Runtime:** Policy engine, sandbox profiles
- **Process isolation:** External modules via Unix socket IPC
- **Audit:** Event log immutability, secrets redaction

## Violation Handling

If a custom module attempts to bypass kernel invariants:

1. **Detected at registration:** Module marked `Incompatible`, not loaded
2. **Detected at runtime:** Module state → `Failed`, fallback to builtin (if safe), emit warning event
3. **Detected in audit:** Security review flags for investigation

## References

- [ARCHITECTURE.md](../ARCHITECTURE.md)
- [VimTrap Implementation Plan](VimTrap_Implementation_Plan.md)
- Issue #63
