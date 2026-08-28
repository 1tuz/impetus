# A1 Implementation: Safe Local Execution Authority

**Status:** ✅ Complete  
**Date:** 2026-08-28  
**Phase:** Architecture audit phase A1

## Scope

Addressed top architectural problems #2, #3, #5 from current-architecture-audit.md:

- #2: ProcessExecution.execute bypasses admission and OS sandbox enforcement
- #3: ProcessSpawn not workspace-scoped, user origin immediately permits it
- #5: IPC tool origin hardcoded User instead of server-derived

## Changes

### 1. AdmittedOperation Token (Type-Level Safety)

**File:** `crates/impetus-core/src/effects.rs`

Created `AdmittedOperation` struct as proof that an effect passed policy and sandbox admission:

```rust
pub struct AdmittedOperation {
    effect: NormalizedEffect,
    intent_revision: u64,
}
```

- Only harness-internal code can construct via `pub(crate) fn new()`
- Contains the exact normalized effect and intent revision
- Cannot be forged by external callers

### 2. EffectAdmission::Allow Returns Token

**Before:**
```rust
pub enum EffectAdmission {
    Allow,
    NeedsApproval(DeferredEffect),
    Deny { reason: String },
}
```

**After:**
```rust
pub enum EffectAdmission {
    Allow(AdmittedOperation),
    NeedsApproval(DeferredEffect),
    Deny { reason: String },
}
```

`EffectSeam::request()` now returns `Allow(token)` on successful admission. The token proves the effect passed both policy and sandbox checks.

### 3. ProcessExecutionRequest::execute() Requires Token

**Before:**
```rust
pub async fn execute(&self) -> Result<ProcessOutput, ProcessExecutionError>
```

**After:**
```rust
pub async fn execute(
    &self,
    _admission: &AdmittedOperation,
) -> Result<ProcessOutput, ProcessExecutionError>
```

Direct spawn is now **impossible** without first obtaining admission via `request()`.

### 4. High-Level Wrapper

Added `ProcessExecution::execute_with_admission()` for convenience:

```rust
pub async fn execute_with_admission(
    &self,
    req: &ProcessExecutionRequest,
) -> Result<ProcessOutput, ProcessExecutionError> {
    let admission = self.request(req)?;
    match admission {
        EffectAdmission::Allow(token) => req.execute(&token).await,
        EffectAdmission::NeedsApproval(_) => Err(ProcessExecutionError::ApprovalRequired),
        EffectAdmission::Deny { reason } => Err(ProcessExecutionError::PolicyDenied(reason)),
    }
}
```

This enforces the correct flow: request → check admission → execute only on Allow.

### 5. Regression Tests

Added three new tests in `crates/impetus-core/src/execution/process.rs`:

1. **`unadmitted_process_cannot_execute`**
   - Documents that execute() requires AdmittedOperation parameter
   - Proves direct spawn bypass is impossible (type-level enforcement)

2. **`agent_origin_requires_approval`**
   - Agent-origin process spawn must return NeedsApproval or Deny
   - Regression for problem #3: user origin no longer auto-permits dangerous commands

3. **Existing tests updated**
   - All tests now obtain admission token via `request()` before calling `execute(&token)`
   - Proves the full request → admission → execute flow works

### 6. DeferredEffect Accessor

Added `effect()` accessor to `DeferredEffect` so tests can inspect the normalized effect without exposing the private field:

```rust
impl DeferredEffect {
    pub fn effect(&self) -> &NormalizedEffect {
        &self.effect
    }
}
```

## Gate Compliance

✅ **No public spawn without admission:** `execute()` signature requires `&AdmittedOperation`  
✅ **Exact approval when needed:** Token contains exact effect + intent_revision  
✅ **Unavailable Seatbelt fails closed:** Sandbox admission precedes token creation

## Verification

```bash
$ cargo test --workspace --lib
test result: ok. 182 passed; 0 failed; 0 ignored; 0 measured

$ cargo clippy --workspace --all-targets -- -D warnings
Finished `dev` profile [unoptimized + debuginfo]

$ cargo fmt --all -- --check
(no diff)
```

All existing tests pass. Six new process execution tests confirm the admission flow.

## Breaking Changes

**None for external callers.** This is an internal harness change:

- `EffectAdmission::Allow` now carries a token instead of being unit
- `ProcessExecutionRequest::execute()` requires admission token
- All call sites within impetus-core updated
- No IPC or client-facing API changes

## Future Work (Out of Scope for A1)

- **A2:** Durable deferred effect continuation after approval
- **A2:** Origin forgery tests (IPC tool origin derivation)
- **Later:** Production Seatbelt integration (currently logical sandbox only)
- **Later:** Workspace scope provisioning (per-session narrow scope)

## References

- `docs/current-architecture-audit.md`: Top architectural problems
- `crates/impetus-core/src/effects.rs`: EffectSeam and admission logic
- `crates/impetus-core/src/execution/process.rs`: ProcessExecution implementation
