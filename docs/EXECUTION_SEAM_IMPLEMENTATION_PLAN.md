# Execution Seam Implementation Plan — v0.2 Step 2/3

## Статус на 2026-08-27

### ✅ Уже реализовано (подтверждено чтением кода)

#### policy.rs
- [x] `ActionFingerprint` struct с SHA256 digest
- [x] `Action::fingerprint()` метод
- [x] Тест `fingerprint_changes_when_the_reviewed_action_changes`
- [x] `PolicyEngine::evaluate()` возвращает `PolicyDecision::Allow | NeedsApproval | Deny`
- [x] `SandboxScope` с workspace containment проверками

#### approval.rs
- [x] `ApprovalId` (Uuid)
- [x] `ApprovalRequest` с `action_fingerprint: ActionFingerprint`
- [x] `ApprovalRequest::intent_revision: u64` для отслеживания user intent
- [x] `ApprovalResolution` с `action_fingerprint` и `intent_revision`
- [x] `ApprovalResolution::user()` конструктор копирующий fingerprint из request
- [x] `ApprovalResolver::User | Agent` enum для authority tracking

#### lib.rs exports
- [x] Все нужные типы экспортированы и доступны публично

### ❓ Требует проверки (tools недоступны)

#### effects.rs
- [ ] Полная реализация `EffectSeam`
- [ ] Stale approval detection в execution path (видел тест, не видел impl)
- [ ] Fail-closed sandbox check
- [ ] `execute_with_approval()` или эквивалент
- [ ] Error types: `EffectError::StaleApproval`, `EffectError::SandboxUnavailable`

#### storage.rs
- [ ] Persistence для `ApprovalRequest` в SQLite
- [ ] Index по `approval_id` и `session_id`
- [ ] Queries для pending approvals

#### ipc.rs
- [ ] `IpcRequest::ResolveApproval`
- [ ] `IpcRequest::ListPendingApprovals`
- [ ] `IpcResponse::ApprovalResolved`
- [ ] `IpcResponse::PendingApprovals`

#### events.rs
- [ ] `ApprovalEvent::Requested`
- [ ] `ApprovalEvent::Resolved`
- [ ] `ApprovalEvent::Stale`

#### harness_api.rs
- [ ] Integration approval flow с session lifecycle
- [ ] Approval resolution через IPC

#### Tests
- [ ] Unit test: stale approval rejection (видел skeleton)
- [ ] Unit test: unavailable sandbox blocks execution
- [ ] Integration test: full approval flow end-to-end

## Plan выполнения (когда tools восстановятся)

### Phase 1: Code Audit (30 min)

```bash
# 1. Прочитать полностью effects.rs
cat crates/agentic-terminal-core/src/effects.rs

# 2. Найти текущую реализацию EffectSeam
rg "impl EffectSeam" crates/

# 3. Проверить stale approval logic
rg "stale.*approval|StaleApproval" crates/

# 4. Проверить sandbox fail-closed
rg "sandbox.*unavailable|SandboxUnavailable" crates/

# 5. Проверить IPC approval handlers
rg "ResolveApproval|resolve_approval" crates/

# 6. Проверить storage для approvals
rg "approval_requests|store.*approval" crates/

# 7. Список всех тестов
rg "#\[test\]|#\[tokio::test\]" crates/agentic-terminal-core/src/ -A 2
```

### Phase 2: Gap Analysis

Создать checklist:
- [ ] Что реализовано полностью
- [ ] Что реализовано частично
- [ ] Что отсутствует полностью

### Phase 3: Implementation

#### Если stale approval check отсутствует:

```rust
// В effects.rs
impl EffectSeam {
    pub async fn execute_with_approval(
        &self,
        effect: NormalizedEffect,
        resolution: ApprovalResolution,
    ) -> Result<ToolOutcome, EffectError> {
        // 1. Get stored request
        let stored = self.approval_store
            .get_request(resolution.id)
            .await
            .ok_or(EffectError::ApprovalNotFound(resolution.id))?;
        
        // 2. Verify fingerprint matches
        let current_action = effect.to_action();
        let current_fingerprint = current_action.fingerprint();
        
        if stored.action_fingerprint != current_fingerprint {
            return Err(EffectError::StaleApproval {
                approval_id: resolution.id,
                original: stored.action_fingerprint,
                current: current_fingerprint,
            });
        }
        
        // 3. Verify intent revision
        if resolution.intent_revision != stored.intent_revision {
            return Err(EffectError::IntentRevisionMismatch {
                expected: stored.intent_revision,
                got: resolution.intent_revision,
            });
        }
        
        // 4. Check if approved
        if !resolution.accepted {
            return Err(EffectError::ApprovalRejected(resolution.id));
        }
        
        // 5. Proceed to sandbox check
        self.execute_approved_effect(effect).await
    }
    
    async fn execute_approved_effect(
        &self,
        effect: NormalizedEffect,
    ) -> Result<ToolOutcome, EffectError> {
        // Fail-closed sandbox check
        let sandbox = self.check_sandbox(&effect)?;
        
        // Capability dispatch
        let capability = self.resolve_capability(&effect)?;
        capability.execute(effect, sandbox).await
    }
    
    fn check_sandbox(&self, effect: &NormalizedEffect) -> Result<ReadOnlySandbox, EffectError> {
        let scope = self.policy.scope();
        
        match ReadOnlySandbox::for_scope(scope.clone()) {
            Some(sandbox) if sandbox.is_available() => Ok(sandbox),
            _ => Err(EffectError::SandboxUnavailable {
                reason: "workspace sandbox is not available".into(),
            }),
        }
    }
}
```

#### Если IPC approval handlers отсутствуют:

```rust
// В harness IPC handler
match request {
    IpcRequest::ResolveApproval { session_id, approval_id, approved } => {
        let session = self.get_session(session_id)?;
        
        // Get pending request
        let request = session.get_approval_request(approval_id)
            .await
            .ok_or(IpcErrorCode::ApprovalNotFound)?;
        
        // Create resolution
        let resolution = ApprovalResolution::user(&request, approved);
        
        // Store resolution
        session.resolve_approval(resolution.clone()).await?;
        
        // Emit event
        session.emit_event(Event {
            payload: EventPayload::Approval(ApprovalEvent::Resolved {
                approval_id,
                approved,
                resolver: ApprovalResolver::User,
            }),
        }).await?;
        
        IpcResponse::ApprovalResolved {
            approval_id,
            timestamp: now(),
        }
    }
    
    IpcRequest::ListPendingApprovals { session_id } => {
        let session = self.get_session(session_id)?;
        let requests = session.list_pending_approvals().await?;
        
        IpcResponse::PendingApprovals { requests }
    }
}
```

#### Если storage для approvals отсутствует:

```rust
// В storage.rs
impl SqliteEventStore {
    pub async fn store_approval_request(&self, request: &ApprovalRequest) -> Result<()> {
        let conn = self.conn.lock().await;
        
        conn.execute(
            "INSERT INTO approval_requests 
             (id, session_id, action_fingerprint, action_json, intent_revision, reason, state, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                request.id.to_string(),
                request.session_id.to_string(),
                request.action_fingerprint.as_str(),
                serde_json::to_string(&request.action)?,
                request.intent_revision,
                &request.reason,
                serde_json::to_string(&request.state)?,
                now(),
            ],
        )?;
        
        Ok(())
    }
    
    pub async fn get_approval_request(&self, id: ApprovalId) -> Result<Option<ApprovalRequest>> {
        let conn = self.conn.lock().await;
        
        let mut stmt = conn.prepare(
            "SELECT id, action_fingerprint, action_json, intent_revision, reason, state
             FROM approval_requests WHERE id = ?1"
        )?;
        
        let result = stmt.query_row(params![id.to_string()], |row| {
            Ok(ApprovalRequest {
                id: Uuid::parse_str(&row.get::<_, String>(0)?)?,
                action_fingerprint: ActionFingerprint::from_str(&row.get::<_, String>(1)?)?,
                action: serde_json::from_str(&row.get::<_, String>(2)?)?,
                intent_revision: row.get(3)?,
                reason: row.get(4)?,
                state: serde_json::from_str(&row.get::<_, String>(5)?)?,
            })
        }).optional()?;
        
        Ok(result)
    }
    
    pub async fn list_pending_approvals(&self, session_id: Uuid) -> Result<Vec<ApprovalRequest>> {
        // Similar query with WHERE session_id = ?1 AND state = 'Pending'
    }
}
```

### Phase 4: Testing

```bash
# 1. Запустить существующие тесты
cargo test --package agentic-terminal-core approval
cargo test --package agentic-terminal-core stale
cargo test --package agentic-terminal-core sandbox

# 2. Добавить недостающие тесты согласно EXECUTION_SEAM_DESIGN.md

# 3. Integration smoke
cargo run -p agentic-terminal-harness &
HARNESS_PID=$!

cargo run -p agentic-terminal-cli -- create
# ... test approval flow

kill $HARNESS_PID
```

### Phase 5: Verification

```bash
# Standard checks
task verify

# Security audit
task security

# CI если есть
task ci:list
task ci:local

# Manual smoke
# 1. Start harness with real provider profile (local/no-secret)
# 2. Create session
# 3. Request effect needing approval
# 4. Verify approval request in events
# 5. Resolve approval
# 6. Verify execution succeeds
# 7. Try to reuse approval with modified action
# 8. Verify stale approval rejection
```

## Definition of Done для v0.2 Step 2/3

- [ ] Stale approval detection реализован и протестирован
- [ ] Fail-closed sandbox реализован и протестирован
- [ ] IPC handlers для approval resolution работают
- [ ] SQLite storage для approval requests/resolutions
- [ ] ApprovalEvent в event log
- [ ] Unit tests: fingerprint, stale approval, sandbox unavailable
- [ ] Integration test: full approval flow
- [ ] `task verify` проходит
- [ ] `task security` без критических findings
- [ ] `task ci:local` проходит (если .gitlab-ci.yml существует)
- [ ] Manual smoke test успешен
- [ ] TODO.md обновлён: шаг 2/3 отмечен выполненным

## Next Steps

После completion шага 2/3 переходим к **v0.2 Step 3/3: Measured Limits**:
- RSS, queue, artifact/output bytes baselines
- Headless dependency graph verification (no GPUI/Metal/PTY)
- Restart/cancel latency measurements
- Context/token accounting

## Blockers

- [ ] Tools временно недоступны — нужно дождаться восстановления для code audit
- [ ] После audit может выясниться что часть уже реализована — тогда сфокусироваться только на gaps

## Notes

Из прочитанного кода видно что архитектура спроектирована качественно:
- `ActionFingerprint` использует proper SHA256 + domain separation prefix
- `ApprovalResolution` содержит и fingerprint и intent_revision для двойной проверки
- `ApprovalResolver` enum предотвращает self-approval
- Типы правильно структурированы и композируются

Это хороший знак — вероятно значительная часть уже реализована, осталось только:
1. Проверить что есть
2. Добавить недостающее
3. Протестировать end-to-end
