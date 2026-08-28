//! Reference implementation for execution seam completion.
//!
//! This file contains the complete implementation of stale approval detection
//! and fail-closed sandbox verification for v0.2 Step 2/3.
//!
//! Integration instructions:
//! 1. Read crates/orbit-core/src/effects.rs
//! 2. Identify what parts are already implemented
//! 3. Merge missing parts from this reference into effects.rs
//! 4. Add missing error variants to EffectError
//! 5. Add tests at the bottom

use crate::{
    Action, ActionFingerprint, ApprovalId, ApprovalRequest, ApprovalResolution,
    PolicyDecision, PolicyEngine, SandboxScope,
};
use std::path::Path;
use thiserror::Error;

// ============================================================================
// Error Types
// ============================================================================

#[derive(Debug, Error)]
pub enum EffectError {
    #[error("Approval {0} not found")]
    ApprovalNotFound(ApprovalId),
    
    #[error("Stale approval {approval_id}: action fingerprint changed from {original} to {current}")]
    StaleApproval {
        approval_id: ApprovalId,
        original: ActionFingerprint,
        current: ActionFingerprint,
    },
    
    #[error("Intent revision mismatch: expected {expected}, got {got}")]
    IntentRevisionMismatch { expected: u64, got: u64 },
    
    #[error("Approval {0} was rejected")]
    ApprovalRejected(ApprovalId),
    
    #[error("Sandbox unavailable: {reason}")]
    SandboxUnavailable { reason: String },
    
    #[error("Capability not available for this effect")]
    CapabilityUnavailable,
    
    #[error("Policy denied: {0}")]
    PolicyDenied(String),
    
    #[error("Effect execution failed: {0}")]
    ExecutionFailed(String),
}

// ============================================================================
// Core Types
// ============================================================================

/// Normalized representation of an effect that needs policy review.
#[derive(Debug, Clone)]
pub struct NormalizedEffect {
    pub kind: EffectKind,
    pub target: Option<String>,
    pub summary: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffectKind {
    ReadFile,
    WriteFile,
    SpawnProcess,
    NetworkConnect,
}

impl NormalizedEffect {
    pub fn to_action(&self, origin: ActionOrigin) -> Action {
        use crate::ActionKind;
        
        let kind = match self.kind {
            EffectKind::ReadFile => ActionKind::ReadFile,
            EffectKind::WriteFile => ActionKind::WriteFile,
            EffectKind::SpawnProcess => ActionKind::SpawnProcess,
            EffectKind::NetworkConnect => ActionKind::NetworkConnect,
        };
        
        Action {
            origin,
            kind,
            summary: self.summary.clone(),
            target: self.target.clone(),
        }
    }
}

/// The decision after policy evaluation.
#[derive(Debug)]
pub enum EffectAdmission {
    /// Effect may proceed immediately (e.g. origin=user, or safe read)
    Allow,
    
    /// Effect requires explicit approval
    NeedsApproval(ApprovalRequest),
    
    /// Effect is denied by policy
    Deny { reason: String },
}

/// Result after effect execution.
#[derive(Debug)]
pub enum EffectExecution {
    Success { outcome: ToolOutcome },
    Denied { reason: String },
    Failed { error: String },
}

#[derive(Debug, Clone)]
pub struct ToolOutcome {
    pub success: bool,
    pub output: String,
}

// ============================================================================
// Sandbox
// ============================================================================

/// Fail-closed workspace sandbox.
#[derive(Debug, Clone)]
pub struct ReadOnlySandbox {
    scope: SandboxScope,
}

impl ReadOnlySandbox {
    pub fn for_scope(scope: SandboxScope) -> Option<Self> {
        // Verify workspace root exists and is accessible
        if scope.workspace_root.exists() && scope.workspace_root.is_dir() {
            Some(Self { scope })
        } else {
            None
        }
    }
    
    pub fn is_available(&self) -> bool {
        // Check if sandbox is still valid
        self.scope.workspace_root.exists()
    }
    
    pub fn scope(&self) -> &SandboxScope {
        &self.scope
    }
    
    pub fn contains(&self, path: &Path) -> bool {
        self.scope.contains(path)
    }
}

// ============================================================================
// Effect Seam - The Core Execution Path
// ============================================================================

/// The narrow, fail-closed boundary for harness capabilities.
pub struct EffectSeam {
    policy: PolicyEngine,
    approval_store: Box<dyn ApprovalStore>,
}

impl EffectSeam {
    pub fn new(policy: PolicyEngine, approval_store: Box<dyn ApprovalStore>) -> Self {
        Self {
            policy,
            approval_store,
        }
    }
    
    /// Step 1: Admit effect through policy
    pub async fn admit_effect(
        &self,
        effect: NormalizedEffect,
        origin: ActionOrigin,
        intent_revision: u64,
    ) -> EffectAdmission {
        let action = effect.to_action(origin);
        let decision = self.policy.evaluate(&action);
        
        match decision {
            PolicyDecision::Allow => EffectAdmission::Allow,
            
            PolicyDecision::NeedsApproval { reason } => {
                let request = ApprovalRequest::pending(action, reason, intent_revision);
                EffectAdmission::NeedsApproval(request)
            }
            
            PolicyDecision::Deny { reason } => {
                EffectAdmission::Deny { reason }
            }
        }
    }
    
    /// Step 2: Execute effect with approved resolution
    pub async fn execute_with_approval(
        &self,
        effect: NormalizedEffect,
        resolution: ApprovalResolution,
        origin: ActionOrigin,
    ) -> Result<ToolOutcome, EffectError> {
        // 1. Retrieve stored approval request
        let stored_request = self
            .approval_store
            .get_request(resolution.id)
            .await
            .ok_or(EffectError::ApprovalNotFound(resolution.id))?;
        
        // 2. Verify action fingerprint matches (stale approval check)
        let current_action = effect.to_action(origin);
        let current_fingerprint = current_action.fingerprint();
        
        if stored_request.action_fingerprint != current_fingerprint {
            tracing::warn!(
                approval_id = %resolution.id,
                original = %stored_request.action_fingerprint.as_str(),
                current = %current_fingerprint.as_str(),
                "Stale approval detected: action changed after approval"
            );
            
            return Err(EffectError::StaleApproval {
                approval_id: resolution.id,
                original: stored_request.action_fingerprint,
                current: current_fingerprint,
            });
        }
        
        // 3. Verify intent revision matches
        if resolution.intent_revision != stored_request.intent_revision {
            return Err(EffectError::IntentRevisionMismatch {
                expected: stored_request.intent_revision,
                got: resolution.intent_revision,
            });
        }
        
        // 4. Check if approval was accepted
        if !resolution.accepted {
            return Err(EffectError::ApprovalRejected(resolution.id));
        }
        
        // 5. Proceed to sandbox-checked execution
        self.execute_approved_effect(effect).await
    }
    
    /// Step 3: Execute with fail-closed sandbox check
    async fn execute_approved_effect(
        &self,
        effect: NormalizedEffect,
    ) -> Result<ToolOutcome, EffectError> {
        // Fail-closed: no sandbox = no execution
        let sandbox = self.check_sandbox(&effect)?;
        
        // Capability dispatch
        self.execute_capability(effect, sandbox).await
    }
    
    /// Fail-closed sandbox verification
    fn check_sandbox(&self, effect: &NormalizedEffect) -> Result<ReadOnlySandbox, EffectError> {
        let scope = self.policy.scope().clone();
        
        match ReadOnlySandbox::for_scope(scope) {
            Some(sandbox) if sandbox.is_available() => {
                tracing::debug!(
                    workspace = ?sandbox.scope().workspace_root,
                    "Sandbox check passed"
                );
                Ok(sandbox)
            }
            Some(_) => {
                tracing::error!(
                    effect = ?effect,
                    "Sandbox became unavailable during execution"
                );
                Err(EffectError::SandboxUnavailable {
                    reason: "workspace sandbox became unavailable".into(),
                })
            }
            None => {
                tracing::error!(
                    effect = ?effect,
                    "Sandbox initialization failed"
                );
                Err(EffectError::SandboxUnavailable {
                    reason: "workspace sandbox could not be initialized".into(),
                })
            }
        }
    }
    
    /// Execute effect within sandbox boundary
    async fn execute_capability(
        &self,
        effect: NormalizedEffect,
        sandbox: ReadOnlySandbox,
    ) -> Result<ToolOutcome, EffectError> {
        match effect.kind {
            EffectKind::ReadFile => {
                // Verify target is in sandbox
                let target = effect.target.as_ref()
                    .ok_or_else(|| EffectError::ExecutionFailed("no target".into()))?;
                
                let path = Path::new(target);
                if !sandbox.contains(path) {
                    return Err(EffectError::ExecutionFailed(
                        format!("target outside sandbox: {}", target)
                    ));
                }
                
                // Execute read
                match std::fs::read_to_string(path) {
                    Ok(content) => Ok(ToolOutcome {
                        success: true,
                        output: content,
                    }),
                    Err(e) => Err(EffectError::ExecutionFailed(e.to_string())),
                }
            }
            
            EffectKind::WriteFile | EffectKind::SpawnProcess | EffectKind::NetworkConnect => {
                // For v0.2, only read capabilities are implemented
                Err(EffectError::CapabilityUnavailable)
            }
        }
    }
}

// ============================================================================
// Approval Store Trait
// ============================================================================

#[async_trait::async_trait]
pub trait ApprovalStore: Send + Sync {
    async fn store_request(&self, request: &ApprovalRequest) -> Result<(), String>;
    async fn get_request(&self, id: ApprovalId) -> Option<ApprovalRequest>;
    async fn list_pending(&self, session_id: uuid::Uuid) -> Vec<ApprovalRequest>;
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use std::collections::HashMap;
    
    struct MemoryApprovalStore {
        requests: Arc<Mutex<HashMap<ApprovalId, ApprovalRequest>>>,
    }
    
    impl MemoryApprovalStore {
        fn new() -> Self {
            Self {
                requests: Arc::new(Mutex::new(HashMap::new())),
            }
        }
    }
    
    #[async_trait::async_trait]
    impl ApprovalStore for MemoryApprovalStore {
        async fn store_request(&self, request: &ApprovalRequest) -> Result<(), String> {
            self.requests.lock().unwrap().insert(request.id, request.clone());
            Ok(())
        }
        
        async fn get_request(&self, id: ApprovalId) -> Option<ApprovalRequest> {
            self.requests.lock().unwrap().get(&id).cloned()
        }
        
        async fn list_pending(&self, _session_id: uuid::Uuid) -> Vec<ApprovalRequest> {
            self.requests.lock().unwrap().values().cloned().collect()
        }
    }
    
    #[tokio::test]
    async fn stale_approval_is_rejected() {
        let workspace = std::env::current_dir().unwrap();
        let policy = PolicyEngine::new(SandboxScope::local_workspace(workspace));
        let store = Box::new(MemoryApprovalStore::new());
        let seam = EffectSeam::new(policy, store);
        
        // 1. Request approval for action A
        let effect_a = NormalizedEffect {
            kind: EffectKind::WriteFile,
            target: Some("config.toml".into()),
            summary: "update config".into(),
        };
        
        let admission = seam.admit_effect(
            effect_a.clone(),
            ActionOrigin::Agent,
            1,
        ).await;
        
        let request = match admission {
            EffectAdmission::NeedsApproval(req) => req,
            _ => panic!("expected approval request"),
        };
        
        // 2. Store the request
        seam.approval_store.store_request(&request).await.unwrap();
        
        // 3. Create approval resolution
        let resolution = ApprovalResolution::user(&request, true);
        
        // 4. Try to execute with MODIFIED effect B
        let effect_b = NormalizedEffect {
            kind: EffectKind::WriteFile,
            target: Some("Cargo.toml".into()), // DIFFERENT TARGET
            summary: "update config".into(),
        };
        
        let result = seam.execute_with_approval(
            effect_b,
            resolution,
            ActionOrigin::Agent,
        ).await;
        
        // 5. Must be rejected as stale
        assert!(matches!(result, Err(EffectError::StaleApproval { .. })));
    }
    
    #[tokio::test]
    async fn unavailable_sandbox_blocks_execution() {
        // Create policy with non-existent workspace
        let policy = PolicyEngine::new(SandboxScope::local_workspace("/nonexistent/workspace"));
        let store = Box::new(MemoryApprovalStore::new());
        let seam = EffectSeam::new(policy, store);
        
        let effect = NormalizedEffect {
            kind: EffectKind::ReadFile,
            target: Some("test.txt".into()),
            summary: "read file".into(),
        };
        
        // This should fail at sandbox check, not at execution
        let result = seam.execute_approved_effect(effect).await;
        
        assert!(matches!(result, Err(EffectError::SandboxUnavailable { .. })));
    }
    
    #[tokio::test]
    async fn valid_approval_executes_successfully() {
        let workspace = std::env::current_dir().unwrap();
        let policy = PolicyEngine::new(SandboxScope::local_workspace(&workspace));
        let store = Box::new(MemoryApprovalStore::new());
        let seam = EffectSeam::new(policy, store);
        
        // Create a test file
        let test_file = workspace.join("test_approval_exec.txt");
        std::fs::write(&test_file, "test content").unwrap();
        
        // 1. Request approval
        let effect = NormalizedEffect {
            kind: EffectKind::ReadFile,
            target: Some(test_file.to_str().unwrap().to_string()),
            summary: "read test file".into(),
        };
        
        let admission = seam.admit_effect(
            effect.clone(),
            ActionOrigin::Agent,
            1,
        ).await;
        
        // ReadFile from agent should need approval or be allowed depending on policy
        let request = match admission {
            EffectAdmission::Allow => {
                // If allowed, execute directly
                let outcome = seam.execute_approved_effect(effect).await.unwrap();
                assert!(outcome.success);
                assert_eq!(outcome.output, "test content");
                
                // Cleanup
                std::fs::remove_file(test_file).ok();
                return;
            }
            EffectAdmission::NeedsApproval(req) => req,
            EffectAdmission::Deny { .. } => panic!("unexpected denial"),
        };
        
        // 2. Store and approve
        seam.approval_store.store_request(&request).await.unwrap();
        let resolution = ApprovalResolution::user(&request, true);
        
        // 3. Execute with SAME effect
        let outcome = seam.execute_with_approval(
            effect,
            resolution,
            ActionOrigin::Agent,
        ).await.unwrap();
        
        assert!(outcome.success);
        assert_eq!(outcome.output, "test content");
        
        // Cleanup
        std::fs::remove_file(test_file).ok();
    }
}
