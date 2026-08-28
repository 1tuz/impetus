# Execution Seam Design — v0.2 Step 2/3

## Цель

Реализовать fail-closed execution path: `NormalizedEffect → Policy → Approval → Sandbox → Capability → Execution` с exact action fingerprint и stale approval rejection.

## Требования из TODO.md

- [ ] Normalized effect только через Policy → Approval → Sandbox → Capability → Execution
- [ ] Exact action fingerprint/revision; stale approval reject
- [ ] Unavailable sandbox fail closed для durable approval → execution path

## Текущее состояние (из lib.rs exports)

Уже есть типы:
- `EffectSeam`, `NormalizedEffect`, `EffectDecision`, `EffectAdmission`, `EffectExecution`
- `PolicyEngine`, `PolicyDecision`, `Action`, `ActionOrigin`, `ActionKind`
- `ApprovalId`, `ApprovalRequest`, `ApprovalResolution`, `ApprovalResolver`, `ApprovalState`
- `ReadOnlySandbox`, `EffectCapability`, `SandboxScope`

Из компактированного effects.rs видел тест stale approval — часть логики уже реализована.

## Архитектурные решения

### 1. Action Fingerprinting

**Требование:** детерминистический идентификатор действия для проверки stale approvals.

**Решение:**
```rust
use sha2::{Sha256, Digest};
use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ActionFingerprint(String);

impl ActionFingerprint {
    pub fn compute(action: &Action) -> Self {
        let canonical = serde_json::to_string(&CanonicalAction::from(action))
            .expect("action serialization is infallible");
        
        let mut hasher = Sha256::new();
        hasher.update(canonical.as_bytes());
        let hash = hasher.finalize();
        
        ActionFingerprint(format!("{:x}", hash))
    }
}

#[derive(Serialize)]
struct CanonicalAction {
    kind: ActionKind,
    origin: ActionOrigin,
    // Sorted params для детерминизма
    params: BTreeMap<String, serde_json::Value>,
}
```

**Свойства:**
- Детерминистический: одинаковое действие → одинаковый hash
- Любое изменение параметров → другой fingerprint
- Независим от порядка вставки params (используем BTreeMap)

### 2. Stale Approval Detection

**Требование:** execution должен отклонять approval если действие изменилось.

**Решение:**
```rust
impl EffectSeam {
    pub async fn execute_with_approval(
        &self,
        effect: NormalizedEffect,
        approval: ApprovalResolution,
    ) -> Result<ToolOutcome, EffectError> {
        // 1. Получаем сохранённый approval request
        let stored_request = self.approval_store
            .get_request(approval.approval_id)
            .await
            .ok_or(EffectError::ApprovalNotFound(approval.approval_id))?;
        
        // 2. Вычисляем fingerprint текущего действия
        let action = effect.to_action();
        let current_fingerprint = ActionFingerprint::compute(&action);
        
        // 3. Сравниваем с fingerprint из approval request
        if stored_request.action_fingerprint != current_fingerprint {
            return Err(EffectError::StaleApproval {
                approval_id: approval.approval_id,
                original_fingerprint: stored_request.action_fingerprint,
                current_fingerprint,
            });
        }
        
        // 4. Approval не истёк по времени (опционально)
        if approval.resolved_at < stored_request.created_at {
            return Err(EffectError::InvalidApprovalTimestamp);
        }
        
        // 5. Продолжаем execution через sandbox
        self.execute_approved_effect(effect).await
    }
}
```

**Fail modes:**
- Approval не найден → `ApprovalNotFound`
- Fingerprint не совпадает → `StaleApproval` с обоими fingerprints для audit
- Неверный timestamp → `InvalidApprovalTimestamp`

### 3. Fail-Closed Sandbox

**Требование:** unavailable sandbox блокирует execution.

**Решение:**
```rust
#[derive(Debug, Clone)]
pub enum SandboxAvailability {
    Available(ReadOnlySandbox),
    Unavailable { reason: String },
}

impl EffectSeam {
    fn check_sandbox(&self, effect: &NormalizedEffect) -> Result<ReadOnlySandbox, EffectError> {
        let scope = SandboxScope::from_effect(effect);
        
        match self.sandbox_resolver.resolve(scope) {
            SandboxAvailability::Available(sandbox) => Ok(sandbox),
            SandboxAvailability::Unavailable { reason } => {
                tracing::error!(
                    effect = ?effect,
                    reason = %reason,
                    "Sandbox unavailable, execution blocked"
                );
                Err(EffectError::SandboxUnavailable { reason })
            }
        }
    }
    
    async fn execute_approved_effect(
        &self,
        effect: NormalizedEffect,
    ) -> Result<ToolOutcome, EffectError> {
        // Fail-closed: без sandbox нет execution
        let sandbox = self.check_sandbox(&effect)?;
        
        // Capability dispatch
        let capability = self.capability_for_effect(&effect)?;
        capability.execute(effect, sandbox).await
    }
}
```

**Гарантии:**
- Нет sandbox → нет execution, всегда `Err`
- Sandbox degradation → explicit unavailable, logged
- Capability не может обойти sandbox check

### 4. Full Execution Flow

```
User/Agent Effect Request
         ↓
   NormalizedEffect
         ↓
   PolicyEngine.decide(Action)
         ↓
    ┌────┴─────┐
    │          │
  Allow    NeedsApproval  ───→  Deny
    │          │                  ↓
    │    Store ApprovalRequest   Reject
    │    (with fingerprint)
    │          │
    │    Send to Client IPC
    │          │
    │    Wait for resolution
    │          │
    │    ┌────┴─────┐
    │  Approved  Rejected
    │    │          │
    └────┴──→  Check fingerprint
         │          │
      Match?   Mismatch
         │          │
         │     StaleApproval
         │          ↓
    Check Sandbox  Reject
         │
    Available?
         │
     Yes │  No
         │   │
         │   └─→ SandboxUnavailable
         │          ↓
    Capability     Reject
         │
    Execute
         ↓
    ToolOutcome
```

## Storage Schema

### ApprovalRequest (SQLite)

```sql
CREATE TABLE approval_requests (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL,
    action_fingerprint TEXT NOT NULL,
    action_json TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    resolved_at INTEGER,
    approved INTEGER,
    FOREIGN KEY (session_id) REFERENCES sessions(id)
);

CREATE INDEX idx_approval_session ON approval_requests(session_id);
CREATE INDEX idx_approval_fingerprint ON approval_requests(action_fingerprint);
CREATE INDEX idx_approval_pending ON approval_requests(session_id, resolved_at) 
    WHERE resolved_at IS NULL;
```

## IPC Extension

### New Request Types

```rust
pub enum IpcRequest {
    // ... existing
    
    /// Client resolves pending approval
    ResolveApproval {
        session_id: Uuid,
        approval_id: ApprovalId,
        approved: bool,
    },
    
    /// Client queries pending approvals
    ListPendingApprovals {
        session_id: Uuid,
    },
    
    /// Client queries specific approval details
    GetApprovalRequest {
        approval_id: ApprovalId,
    },
}
```

### New Response Types

```rust
pub enum IpcResponse {
    // ... existing
    
    ApprovalResolved {
        approval_id: ApprovalId,
        timestamp: u64,
    },
    
    PendingApprovals {
        requests: Vec<ApprovalRequest>,
    },
    
    ApprovalDetails {
        request: ApprovalRequest,
    },
}
```

### New Event Types

```rust
pub enum EventPayload {
    // ... existing
    
    Approval(ApprovalEvent),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ApprovalEvent {
    Requested {
        approval_id: ApprovalId,
        action_fingerprint: ActionFingerprint,
        action: Action,
    },
    Resolved {
        approval_id: ApprovalId,
        approved: bool,
        resolved_by: ResolvedBy, // User | Timeout | PolicyChange
    },
    Stale {
        approval_id: ApprovalId,
        original_fingerprint: ActionFingerprint,
        current_fingerprint: ActionFingerprint,
    },
}
```

## Error Types

```rust
#[derive(Debug, thiserror::Error)]
pub enum EffectError {
    #[error("Approval {0} not found")]
    ApprovalNotFound(ApprovalId),
    
    #[error("Stale approval {approval_id}: action changed from {original_fingerprint} to {current_fingerprint}")]
    StaleApproval {
        approval_id: ApprovalId,
        original_fingerprint: ActionFingerprint,
        current_fingerprint: ActionFingerprint,
    },
    
    #[error("Sandbox unavailable: {reason}")]
    SandboxUnavailable { reason: String },
    
    #[error("Capability {0} not available")]
    CapabilityUnavailable(EffectCapability),
    
    #[error("Invalid approval timestamp")]
    InvalidApprovalTimestamp,
    
    #[error("Policy denied: {0}")]
    PolicyDenied(String),
}
```

## Testing Strategy

### Unit Tests

```rust
#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn action_fingerprint_is_deterministic() {
        let action1 = Action { /* ... */ };
        let action2 = action1.clone();
        
        assert_eq!(
            ActionFingerprint::compute(&action1),
            ActionFingerprint::compute(&action2)
        );
    }
    
    #[test]
    fn action_fingerprint_detects_changes() {
        let action1 = Action { kind: ActionKind::ReadFile, /* ... */ };
        let action2 = Action { kind: ActionKind::WriteFile, /* ... */ };
        
        assert_ne!(
            ActionFingerprint::compute(&action1),
            ActionFingerprint::compute(&action2)
        );
    }
    
    #[tokio::test]
    async fn stale_approval_is_rejected() {
        let seam = EffectSeam::new(/* ... */);
        
        // 1. Create approval for action A
        let action_a = Action { /* ... */ };
        let effect_a = NormalizedEffect::from(action_a);
        let approval_req = seam.request_approval(effect_a).await.unwrap();
        
        // 2. Approve it
        let approval = seam.resolve_approval(approval_req.id, true).await.unwrap();
        
        // 3. Try to execute with modified action B
        let action_b = Action { /* modified */ };
        let effect_b = NormalizedEffect::from(action_b);
        
        let result = seam.execute_with_approval(effect_b, approval).await;
        
        assert!(matches!(result, Err(EffectError::StaleApproval { .. })));
    }
    
    #[tokio::test]
    async fn unavailable_sandbox_blocks_execution() {
        let seam = EffectSeam::with_unavailable_sandbox();
        let effect = NormalizedEffect::read_file("/test");
        
        let result = seam.execute_approved_effect(effect).await;
        
        assert!(matches!(result, Err(EffectError::SandboxUnavailable { .. })));
    }
}
```

### Integration Tests

```rust
#[tokio::test]
async fn full_approval_flow() {
    // 1. Start harness
    let harness = Harness::new(/* ... */).await;
    let session = harness.create_session().await.unwrap();
    
    // 2. Agent requests effect needing approval
    let effect = NormalizedEffect::write_file("/tmp/test", "content");
    let admission = harness.admit_effect(session.id, effect.clone()).await.unwrap();
    
    let approval_id = match admission {
        EffectAdmission::NeedsApproval(req) => req.id,
        _ => panic!("expected approval request"),
    };
    
    // 3. Client approves via IPC
    harness.resolve_approval(approval_id, true).await.unwrap();
    
    // 4. Execute with original effect
    let outcome = harness.execute_approved(session.id, effect, approval_id).await.unwrap();
    assert!(outcome.success);
    
    // 5. Try stale execution with modified effect
    let modified_effect = NormalizedEffect::write_file("/tmp/test", "DIFFERENT");
    let stale_result = harness.execute_approved(session.id, modified_effect, approval_id).await;
    
    assert!(matches!(stale_result, Err(EffectError::StaleApproval { .. })));
}
```

## Implementation Checklist

- [ ] 1. Добавить `ActionFingerprint` в `policy.rs`
- [ ] 2. Расширить `ApprovalRequest` полем `action_fingerprint`
- [ ] 3. Добавить `ApprovalStore` trait с SQLite impl
- [ ] 4. Реализовать stale approval check в `EffectSeam`
- [ ] 5. Добавить fail-closed sandbox check
- [ ] 6. Расширить `IpcRequest`/`IpcResponse` для approvals
- [ ] 7. Добавить `ApprovalEvent` в event log
- [ ] 8. Написать unit tests для fingerprinting
- [ ] 9. Написать unit tests для stale approval rejection
- [ ] 10. Написать unit tests для sandbox unavailable
- [ ] 11. Написать integration test полного approval flow
- [ ] 12. Обновить harness IPC handler для approval resolution
- [ ] 13. Проверить `task verify`
- [ ] 14. Проверить `task ci:local` если есть `.gitlab-ci.yml`
- [ ] 15. Обновить TODO.md отметкой завершения шага 2/3

## Security Properties

1. **No bypass:** effect не может выполниться без прохождения policy → sandbox → capability chain
2. **Stale rejection:** approval нельзя переиспользовать для изменённого действия
3. **Fail-closed:** отсутствие sandbox блокирует execution, не разрешает
4. **Audit trail:** все approval decisions и stale rejections записываются в event log
5. **Immutable approvals:** resolved approval нельзя изменить retroactively

## Performance Considerations

- Fingerprint computation: O(n) где n — размер action params, cached в ApprovalRequest
- Approval lookup: indexed by approval_id, O(1)
- Fingerprint verification: string comparison, O(k) где k — длина hash (64 chars)
- Sandbox check: O(1) для read-only workspace scope

## Next Step After This

После завершения execution seam переходим к **v0.2 Step 3/3: Measured Limits**:
- RSS/queue/artifact baselines
- Headless dependency graph verification (no GPUI/Metal/PTY)
- Context/token accounting
