//! Process execution with bounded output and artifact capture.

use crate::{Action, ActionKind, ActionOrigin, EffectAdmission, EffectSeam, NormalizedEffect};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;
use thiserror::Error;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::time::timeout;

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
    #[error("process timed out after {0:?}")]
    Timeout(Duration),
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
        _admission: &crate::AdmittedOperation,
    ) -> Result<ProcessOutput, ProcessExecutionError> {
        let start = std::time::Instant::now();

        let mut cmd = Command::new(&self.command);
        cmd.args(&self.args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        if let Some(dir) = &self.working_dir {
            cmd.current_dir(dir);
        }

        for (key, value) in &self.env {
            cmd.env(key, value);
        }

        let mut child = cmd
            .spawn()
            .map_err(|e| ProcessExecutionError::ExecutionFailed(e.to_string()))?;

        let stdout = child.stdout.take().expect("stdout piped");
        let stderr = child.stderr.take().expect("stderr piped");

        let stdout_task = tokio::spawn(capture_stream(stdout, MAX_PROCESS_OUTPUT_BYTES));
        let stderr_task = tokio::spawn(capture_stream(stderr, MAX_PROCESS_OUTPUT_BYTES));

        let wait_result = timeout(self.timeout, child.wait()).await;

        let exit_status = match wait_result {
            Ok(Ok(status)) => status,
            Ok(Err(e)) => {
                return Err(ProcessExecutionError::ExecutionFailed(e.to_string()));
            }
            Err(_) => {
                // Timeout: kill the process
                let _ = child.kill().await;
                return Err(ProcessExecutionError::Timeout(self.timeout));
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
}

/// Capture stream output up to max_bytes, returning (content, truncated).
async fn capture_stream(
    stream: impl tokio::io::AsyncRead + Unpin,
    max_bytes: usize,
) -> (String, bool) {
    let mut reader = BufReader::new(stream);
    let mut buffer = Vec::with_capacity(8192);
    let mut total_bytes = 0;
    let mut truncated = false;

    loop {
        let chunk_size = reader.read_until(b'\n', &mut buffer).await.unwrap_or(0);

        if chunk_size == 0 {
            break;
        }

        total_bytes += chunk_size;

        if total_bytes > max_bytes {
            truncated = true;
            buffer.truncate(max_bytes);
            break;
        }
    }

    let content = String::from_utf8_lossy(&buffer).into_owned();
    (content, truncated)
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

    #[tokio::test]
    async fn process_execution_captures_output() {
        let seam = test_seam();
        let request =
            ProcessExecutionRequest::new("echo", vec!["hello".into()], ActionOrigin::User, 1);

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

    #[tokio::test]
    async fn process_execution_handles_failure() {
        let seam = test_seam();
        let request = ProcessExecutionRequest::new("false", vec![], ActionOrigin::User, 1);

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

    #[tokio::test]
    async fn process_execution_respects_timeout() {
        let seam = test_seam();
        let request =
            ProcessExecutionRequest::new("sleep", vec!["10".into()], ActionOrigin::User, 1)
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
