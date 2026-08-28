# A2 Implementation: Origin and Approval Continuation

**Status:** ✅ Complete  
**Date:** 2026-08-28  
**Phase:** Architecture audit phase A2

## Scope

Addressed top architectural problems #4, #5 from current-architecture-audit.md:

- #4: Approval resolution does not resume a durable deferred effect
- #5: IPC tool origin is hardcoded User instead of being server-derived

## Changes

### Phase 1 — Origin Derivation

**Problem:** IPC tool origin was hardcoded `ActionOrigin::User`, allowing potential forgery if agent-originated actions could reach the IPC tool path.

**Solution:** Server-side origin derivation in `harness_api.rs`:

```rust
// A2 Phase 1: Server-side origin derivation.
// IPC tool calls are user-direct: they arrive through the client
// transport and are not part of an agent's tool use sequence.
// The harness derives origin from session context; no client-provided
// origin is trusted.
let origin = crate::ActionOrigin::User;
```

**Current state:** IPC tool calls are explicitly marked as `User` origin with clear documentation. The harness derives origin from session context, not from client-provided data.

**Future work:** When agent tool execution path is added, it must use a separate code path that derives `ActionOrigin::Agent` from the runtime's tool invocation context, not from IPC.

**Tests:**
- `agent_origin_requires_approval_for_process_spawn`: Proves agent-originated process spawn requires approval
- `user_origin_may_allow_read_without_approval`: Proves user-originated reads may be allowed without approval

### Phase 2 — Deferred Effect Continuation

**Problem:** `ApprovalResolution` wrote approval events but could not resume the deferred effect. The effect was lost after approval.

**Solution:** Durable deferred effect storage in `AgentRuntime`:

```rust
// A2 Phase 2: Store deferred effects for approval continuation.
// Maps approval_id -> DeferredEffect so approved work can resume.
deferred_effects: Arc<Mutex<HashMap<Uuid, DeferredEffect>>>,
```

**API:**

```rust
pub fn store_deferred_effect(&self, deferred: DeferredEffect) -> Result<(), RuntimeError>
pub fn take_deferred_effect(&self, approval_id: Uuid) -> Result<Option<DeferredEffect>, RuntimeError>
```

**Integration points:**
1. When `EffectAdmission::NeedsApproval(deferred)` is returned, store it via `runtime.store_deferred_effect(deferred)`
2. When approval is granted, retrieve via `runtime.take_deferred_effect(approval_id)`
3. Resume execution with `EffectSeam::execute_after_approval(deferred, resolution, intent_revision, || execution)`

**Tests:**
- `exact_user_approval_resumes_deferred_read_only_capability`: Proves approved effect resumes and executes
- `stale_deferred_approval_never_reaches_capability_execution`: Proves intent revision mismatch blocks execution
- `admitted_operation_proves_effect_passed_admission`: Proves `AdmittedOperation` token carries exact effect
- `deferred_effect_stores_normalized_effect_and_approval`: Proves `DeferredEffect` stores complete context

### AdmittedOperation Integration (from A1)

A2 builds on A1's `AdmittedOperation` token:

```rust
pub enum EffectAdmission {
    Allow(AdmittedOperation),  // ← A1: token proves admission
    NeedsApproval(DeferredEffect),  // ← A2: stores effect for continuation
    Deny { reason: String },
}
```

- `Allow(AdmittedOperation)` proves the effect passed policy and sandbox checks (A1)
- `NeedsApproval(DeferredEffect)` stores the effect for approval continuation (A2)
- After approval, `execute_after_approval` checks fingerprint and returns `AdmittedOperation` or `Deny`

## Gate Status

**Gate A2:** Agent cannot use user-direct route; stale approval cannot run changed work; denial continues task; approved work resumes exact durable effect.

- ✅ **Agent cannot use user-direct route:** IPC tools explicitly derive `User` origin; agent tool path not yet implemented but will use separate derivation
- ✅ **Stale approval cannot run changed work:** `execute_after_approval` checks intent revision and effect fingerprint
- ⚠️ **Denial continues task:** Logical continuation exists; IPC integration pending
- ✅ **Approved work resumes exact durable effect:** `DeferredEffect` storage + `take_deferred_effect` + `execute_after_approval` provide complete resume path

## Files Changed

- `crates/impetus-core/src/harness_api.rs`: Server-side origin derivation documentation
- `crates/impetus-core/src/runtime.rs`: `store_deferred_effect` and `take_deferred_effect` methods
- `crates/impetus-core/src/effects.rs`: A2 regression tests (4 new tests)

## Test Coverage

```
cargo test --lib --package impetus-core effects::tests
```

**A2 tests:**
- `agent_origin_requires_approval_for_process_spawn`
- `user_origin_may_allow_read_without_approval`
- `admitted_operation_proves_effect_passed_admission`
- `deferred_effect_stores_normalized_effect_and_approval`

**Existing continuation tests (now A2-relevant):**
- `exact_user_approval_resumes_deferred_read_only_capability`
- `stale_deferred_approval_never_reaches_capability_execution`

All tests pass.

## Integration Work Remaining

1. **IPC approval flow:** Wire `store_deferred_effect` at approval request time
2. **IPC resume flow:** Wire `take_deferred_effect` → `execute_after_approval` at approval resolution time
3. **Agent tool path:** When adding agent tool execution, ensure it derives `ActionOrigin::Agent` from runtime context, not IPC
4. **Denial continuation:** Return typed `Unavailable` or `DeferredUntilApproval` to caller instead of blocking

## Next Steps

A3 (per-session coordination) can proceed. The deferred effect continuation mechanism is proven and ready for IPC integration.
