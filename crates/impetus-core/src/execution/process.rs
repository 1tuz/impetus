//! Process execution with bounded output and artifact capture.

use crate::{
    Action, ActionKind, ActionOrigin, EffectAdmission, EffectCapability, EffectSeam,
    NormalizedEffect, SandboxCommandRequest, SandboxDecision, SandboxError, SandboxProvider,
    production_sandbox_provider,
};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::Duration;
use thiserror::Error;
use tokio::io::AsyncReadExt;
use tokio::process::Child;
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;

/// Maximum bytes captured from stdout/stderr before truncation.
pub const MAX_PROCESS_OUTPUT_BYTES: usize = 2 * 1024 * 1024;

/// Default execution timeout (2 minutes).
pub const DEFAULT_EXECUTION_TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Debug, Error)]
pub enum ProcessExecutionError {
    #[error("policy denied process execution: {0}")]
    PolicyDenied(String),
    #[error("approval required but not granted")]
    ApprovalRequired,
    #[error("process execution failed: {0}")]
    ExecutionFailed(String),
    #[error("required OS sandbox is unavailable")]
    SandboxUnavailable,
    #[error("OS sandbox denied execution: {reason_code}")]
    SandboxDenied { reason_code: String },
    #[error("process timed out after {0:?}")]
    Timeout(Duration),
    #[error("process execution was cancelled")]
    Cancelled,
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessOutput {
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub truncated: bool,
    pub duration_ms: u64,
}

/// Process execution request with policy check and bounded output.
#[derive(Debug, Clone)]
pub struct ProcessExecutionRequest {
    pub command: String,
    pub args: Vec<String>,
    pub working_dir: Option<PathBuf>,
    pub env: Vec<(String, String)>,
    pub origin: ActionOrigin,
    pub intent_revision: u64,
    pub timeout: Duration,
}

impl ProcessExecutionRequest {
    pub fn new(
        command: impl Into<String>,
        args: Vec<String>,
        origin: ActionOrigin,
        intent_revision: u64,
    ) -> Self {
        Self {
            command: command.into(),
            args,
            working_dir: None,
            env: Vec::new(),
            origin,
            intent_revision,
            timeout: DEFAULT_EXECUTION_TIMEOUT,
        }
    }

    pub fn with_working_dir(mut self, dir: PathBuf) -> Self {
        self.working_dir = Some(dir);
        self
    }

    pub fn with_env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.push((key.into(), value.into()));
        self
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Prepare process execution through policy and effect seam.
    /// Returns either immediate Allow, NeedsApproval with deferred effect, or Deny.
    pub fn request(&self, seam: &EffectSeam) -> Result<EffectAdmission, ProcessExecutionError> {
        let summary = format!("{} {}", self.command, self.args.join(" "));
        let target = self
            .working_dir
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| ".".to_string());

        let _action = Action {
            origin: self.origin,
            kind: ActionKind::SpawnProcess,
            summary: summary.clone(),
            target: Some(target.clone()),
        };

        let effect = NormalizedEffect::process_spawn(self.origin, summary, target);

        Ok(seam.request(effect, self.intent_revision))
    }

    /// Execute the process after policy approval.
    /// Output is bounded to MAX_PROCESS_OUTPUT_BYTES.
    /// Requires AdmittedOperation token proving the effect passed admission.
    pub async fn execute(
        &self,
        admission: &crate::AdmittedOperation,
    ) -> Result<ProcessOutput, ProcessExecutionError> {
        let provider = production_sandbox_provider();
        self.execute_with_provider(admission, provider.as_ref())
            .await
    }

    pub async fn execute_with_provider(
        &self,
        admission: &crate::AdmittedOperation,
        provider: &dyn SandboxProvider,
    ) -> Result<ProcessOutput, ProcessExecutionError> {
        self.execute_with_provider_and_cancellation(
            admission,
            provider,
            CancellationToken::new(),
            |_| Ok(()),
        )
        .await
    }

    /// Execute through an injected OS backend and report a secret-free sandbox
    /// decision before the child can start.
    pub async fn execute_with_provider_and_cancellation(
        &self,
        admission: &crate::AdmittedOperation,
        provider: &dyn SandboxProvider,
        cancellation: CancellationToken,
        observe: impl FnOnce(&SandboxDecision) -> Result<(), ProcessExecutionError>,
    ) -> Result<ProcessOutput, ProcessExecutionError> {
        let start = std::time::Instant::now();
        self.validate_admission(admission)?;
        let working_dir =
            self.working_dir
                .as_deref()
                .ok_or_else(|| ProcessExecutionError::SandboxDenied {
                    reason_code: "missing_working_directory".into(),
                })?;
        let sandbox_request = SandboxCommandRequest {
            executable: &self.command,
            args: &self.args,
            workspace_root: &admission.sandbox_scope().workspace_root,
            working_dir,
            explicit_env: &self.env,
            allow_network: false,
        };
        let mut prepared = match provider.prepare(&sandbox_request) {
            Ok(prepared) => prepared,
            Err(error) => {
                let decision =
                    SandboxDecision::denied(provider.backend_name(), &sandbox_request, &error);
                observe(&decision)?;
                return Err(map_sandbox_error(error));
            }
        };
        observe(prepared.decision())?;

        let mut child = prepared
            .command_mut()
            .spawn()
            .map_err(|e| ProcessExecutionError::ExecutionFailed(e.to_string()))?;
        let process_group = child.id();

        let stdout = child.stdout.take().expect("stdout piped");
        let stderr = child.stderr.take().expect("stderr piped");

        let stdout_task = tokio::spawn(capture_stream(stdout, MAX_PROCESS_OUTPUT_BYTES));
        let stderr_task = tokio::spawn(capture_stream(stderr, MAX_PROCESS_OUTPUT_BYTES));

        enum WaitOutcome {
            Exited(std::io::Result<std::process::ExitStatus>),
            TimedOut,
            Cancelled,
        }
        let wait_result = tokio::select! {
            result = child.wait() => WaitOutcome::Exited(result),
            _ = tokio::time::sleep(self.timeout) => WaitOutcome::TimedOut,
            _ = cancellation.cancelled() => WaitOutcome::Cancelled,
        };

        let exit_status = match wait_result {
            WaitOutcome::Exited(Ok(status)) => {
                terminate_remaining_process_group(process_group).await;
                status
            }
            WaitOutcome::Exited(Err(error)) => {
                terminate_process_tree(&mut child, process_group).await;
                drain_capture_tasks(stdout_task, stderr_task).await?;
                return Err(ProcessExecutionError::ExecutionFailed(error.to_string()));
            }
            WaitOutcome::TimedOut => {
                terminate_process_tree(&mut child, process_group).await;
                drain_capture_tasks(stdout_task, stderr_task).await?;
                return Err(ProcessExecutionError::Timeout(self.timeout));
            }
            WaitOutcome::Cancelled => {
                terminate_process_tree(&mut child, process_group).await;
                drain_capture_tasks(stdout_task, stderr_task).await?;
                return Err(ProcessExecutionError::Cancelled);
            }
        };

        let (stdout_output, stdout_truncated) = stdout_task
            .await
            .map_err(|e| ProcessExecutionError::ExecutionFailed(e.to_string()))?;
        let (stderr_output, stderr_truncated) = stderr_task
            .await
            .map_err(|e| ProcessExecutionError::ExecutionFailed(e.to_string()))?;

        let duration_ms = start.elapsed().as_millis() as u64;

        Ok(ProcessOutput {
            exit_code: exit_status.code(),
            stdout: stdout_output,
            stderr: stderr_output,
            truncated: stdout_truncated || stderr_truncated,
            duration_ms,
        })
    }

    fn validate_admission(
        &self,
        admission: &crate::AdmittedOperation,
    ) -> Result<(), ProcessExecutionError> {
        let target = self
            .working_dir
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| ".".into());
        let effect = admission.effect();
        if effect.capability != EffectCapability::ProcessSpawn
            || effect.action.kind != ActionKind::SpawnProcess
            || effect.origin != self.origin
            || effect.action.origin != self.origin
            || effect.action.target.as_deref() != Some(target.as_str())
            || admission.intent_revision() != self.intent_revision
        {
            return Err(ProcessExecutionError::SandboxDenied {
                reason_code: "admission_mismatch".into(),
            });
        }
        Ok(())
    }
}

/// Capture stream output up to max_bytes, returning (content, truncated).
async fn capture_stream(
    mut stream: impl tokio::io::AsyncRead + Unpin,
    max_bytes: usize,
) -> (String, bool) {
    let mut buffer = Vec::with_capacity(max_bytes.min(8192));
    let mut chunk = [0_u8; 8192];
    let mut truncated = false;

    loop {
        let read = stream.read(&mut chunk).await.unwrap_or(0);
        if read == 0 {
            break;
        }
        let remaining = max_bytes.saturating_sub(buffer.len());
        let retained = remaining.min(read);
        buffer.extend_from_slice(&chunk[..retained]);
        if retained < read {
            truncated = true;
        }
    }

    let content = String::from_utf8_lossy(&buffer).into_owned();
    (content, truncated)
}

fn map_sandbox_error(error: SandboxError) -> ProcessExecutionError {
    match error {
        SandboxError::Unavailable => ProcessExecutionError::SandboxUnavailable,
        other => ProcessExecutionError::SandboxDenied {
            reason_code: other.reason_code().into(),
        },
    }
}

async fn drain_capture_tasks(
    stdout_task: tokio::task::JoinHandle<(String, bool)>,
    stderr_task: tokio::task::JoinHandle<(String, bool)>,
) -> Result<(), ProcessExecutionError> {
    stdout_task
        .await
        .map_err(|error| ProcessExecutionError::ExecutionFailed(error.to_string()))?;
    stderr_task
        .await
        .map_err(|error| ProcessExecutionError::ExecutionFailed(error.to_string()))?;
    Ok(())
}

#[cfg(unix)]
async fn terminate_process_tree(child: &mut Child, process_group: Option<u32>) {
    signal_process_group(process_group, 15);
    if timeout(Duration::from_millis(500), child.wait())
        .await
        .is_err()
    {
        signal_process_group(process_group, 9);
        let _ = child.wait().await;
    }
    signal_process_group(process_group, 9);
}

#[cfg(not(unix))]
async fn terminate_process_tree(child: &mut Child, _process_group: Option<u32>) {
    let _ = child.kill().await;
    let _ = child.wait().await;
}

#[cfg(unix)]
async fn terminate_remaining_process_group(process_group: Option<u32>) {
    signal_process_group(process_group, 15);
    tokio::time::sleep(Duration::from_millis(20)).await;
    signal_process_group(process_group, 9);
}

#[cfg(not(unix))]
async fn terminate_remaining_process_group(_process_group: Option<u32>) {}

#[cfg(unix)]
fn signal_process_group(process_group: Option<u32>, signal: i32) {
    unsafe extern "C" {
        fn kill(pid: i32, signal: i32) -> i32;
    }
    let Some(process_group) = process_group.and_then(|pid| i32::try_from(pid).ok()) else {
        return;
    };
    // The child starts a fresh process group. A negative pid addresses the
    // complete group, including descendants that outlive the shell process.
    let _ = unsafe { kill(-process_group, signal) };
}

/// ProcessExecution wraps the request/execute lifecycle.
pub struct ProcessExecution {
    seam: EffectSeam,
}

impl ProcessExecution {
    pub fn new(seam: EffectSeam) -> Self {
        Self { seam }
    }

    /// Request process execution and return admission decision.
    pub fn request(
        &self,
        req: &ProcessExecutionRequest,
    ) -> Result<EffectAdmission, ProcessExecutionError> {
        req.request(&self.seam)
    }

    /// Execute after approval (or immediate Allow).
    /// Returns the admission token on Allow, which must be passed to req.execute().
    pub async fn execute_with_admission(
        &self,
        req: &ProcessExecutionRequest,
    ) -> Result<ProcessOutput, ProcessExecutionError> {
        let admission = self.request(req)?;
        match admission {
            crate::EffectAdmission::Allow(token) => req.execute(&token).await,
            crate::EffectAdmission::NeedsApproval(_) => {
                Err(ProcessExecutionError::ApprovalRequired)
            }
            crate::EffectAdmission::Deny { reason } => {
                Err(ProcessExecutionError::PolicyDenied(reason))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{PolicyEngine, Sandbox, SandboxScope};

    fn test_seam() -> EffectSeam {
        let workspace = std::env::temp_dir();
        let policy = PolicyEngine::new(SandboxScope::local_workspace(workspace.clone()));
        EffectSeam::with_sandbox(policy, Sandbox::workspace(workspace))
    }

    #[test]
    fn process_request_creates_correct_action() {
        let seam = test_seam();
        let request =
            ProcessExecutionRequest::new("echo", vec!["test".into()], ActionOrigin::Agent, 1);

        let result = request.request(&seam);
        assert!(result.is_ok());

        match result.unwrap() {
            EffectAdmission::NeedsApproval(deferred) => {
                let action = &deferred.approval().action;
                assert_eq!(action.kind, ActionKind::SpawnProcess);
                assert_eq!(action.origin, ActionOrigin::Agent);
                assert!(action.summary.contains("echo test"));
            }
            _ => panic!("expected needs approval for agent process spawn"),
        }
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn process_execution_captures_output() {
        let seam = test_seam();
        let request =
            ProcessExecutionRequest::new("/bin/echo", vec!["hello".into()], ActionOrigin::User, 1)
                .with_working_dir(std::env::temp_dir());

        let admission = request.request(&seam).unwrap();
        let token = match admission {
            crate::EffectAdmission::Allow(t) => t,
            _ => panic!("expected Allow for user echo"),
        };

        let result = request.execute(&token).await;
        assert!(result.is_ok());

        let output = result.unwrap();
        assert_eq!(output.exit_code, Some(0));
        assert!(output.stdout.contains("hello"));
        assert!(!output.truncated);
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn process_execution_handles_failure() {
        let seam = test_seam();
        let request = ProcessExecutionRequest::new("/usr/bin/false", vec![], ActionOrigin::User, 1)
            .with_working_dir(std::env::temp_dir());

        let admission = request.request(&seam).unwrap();
        let token = match admission {
            crate::EffectAdmission::Allow(t) => t,
            _ => panic!("expected Allow for user false"),
        };

        let result = request.execute(&token).await;
        assert!(result.is_ok());

        let output = result.unwrap();
        assert_ne!(output.exit_code, Some(0));
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn process_execution_respects_timeout() {
        let seam = test_seam();
        let request =
            ProcessExecutionRequest::new("/bin/sleep", vec!["10".into()], ActionOrigin::User, 1)
                .with_working_dir(std::env::temp_dir())
                .with_timeout(Duration::from_millis(100));

        let admission = request.request(&seam).unwrap();
        let token = match admission {
            crate::EffectAdmission::Allow(t) => t,
            _ => panic!("expected Allow for user sleep"),
        };

        let result = request.execute(&token).await;
        assert!(matches!(result, Err(ProcessExecutionError::Timeout(_))));
    }

    #[tokio::test]
    async fn unadmitted_process_cannot_execute() {
        // Regression test for A1: execute() requires admission token.
        // Without calling request(), there's no way to get AdmittedOperation,
        // so direct execute() is a compile error. This test proves the API contract.
        let request =
            ProcessExecutionRequest::new("echo", vec!["bypass".into()], ActionOrigin::Agent, 1);

        // This would not compile:
        // let _ = request.execute().await;
        // Error: method execute requires &AdmittedOperation parameter

        // The only way to execute is through request() -> Allow(token) -> execute(&token)
        // This test documents the contract; actual enforcement is type-level.
        let seam = test_seam();
        let admission = request.request(&seam).unwrap();
        match admission {
            crate::EffectAdmission::NeedsApproval(_) => {
                // Agent origin requires approval; cannot execute without user resolution
            }
            crate::EffectAdmission::Allow(token) => {
                // If policy allows, token proves admission
                let _ = request.execute(&token).await;
            }
            crate::EffectAdmission::Deny { .. } => {
                // Policy denied; no token, no execution
            }
        }
    }

    #[tokio::test]
    async fn agent_origin_requires_approval() {
        // Regression test: agent-origin process spawn must not auto-Allow
        let seam = test_seam();
        let request = ProcessExecutionRequest::new(
            "rm",
            vec!["-rf".into(), "/".into()],
            ActionOrigin::Agent,
            1,
        );

        let admission = request.request(&seam).unwrap();
        match admission {
            crate::EffectAdmission::Allow(_) => {
                panic!("agent origin process spawn should require approval, got Allow")
            }
            crate::EffectAdmission::NeedsApproval(deferred) => {
                assert_eq!(deferred.effect().origin, ActionOrigin::Agent);
                assert_eq!(
                    deferred.effect().capability,
                    crate::EffectCapability::ProcessSpawn
                );
            }
            crate::EffectAdmission::Deny { .. } => {
                // Deny is also acceptable for dangerous commands
            }
        }
    }
}
