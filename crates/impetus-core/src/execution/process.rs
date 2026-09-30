//! Process execution with bounded output and artifact capture.

use crate::{
    Action, ActionKind, ActionOrigin, DurableArtifactRef, DurableArtifactStore, EffectAdmission,
    EffectFenceLedger, EffectSeam, HookPrefilter, NormalizedEffect, SpawnStubOutcome, spawn_stub,
};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::Duration;
use thiserror::Error;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::time::timeout;

#[cfg(not(target_os = "macos"))]
use std::process::Stdio;
#[cfg(not(target_os = "macos"))]
use tokio::process::Command;

#[cfg(target_os = "macos")]
use super::sandbox::{
    PreparedSandboxCommand, SandboxCommandRequest, SandboxError, production_sandbox_provider,
};

/// Maximum UTF-8 bytes kept inline in process stdout/stderr preview (and in the
/// combined observation body). Full captured output is stored as a durable
/// artifact when this limit is exceeded — same bound as tool previews.
pub const MAX_PROCESS_PREVIEW_BYTES: usize = 16 * 1024;

/// Maximum bytes captured from each of stdout/stderr before capture stops.
///
/// ponytail: capture still buffers in RAM up to this ceiling per stream
/// (~2 MiB). Mid-stream spill to DurableArtifactStore would remove the RAM
/// ceiling; upgrade path is chunked hash-and-append while reading.
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
    #[error("artifact store error: {0}")]
    Artifact(String),
    #[error("sandbox backend unavailable")]
    SandboxUnavailable,
    #[error("sandbox denied: {0}")]
    SandboxDenied(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("hook prefilter denied spawn for label {0}")]
    PrefilterDenied(String),
    #[error(
        "security-critical hook for label `{label}` requires InDaemon trust; External matcher refused"
    )]
    PrefilterExternalForCritical { label: String },
    #[error("effect fence denied process execution: {0}")]
    EffectFence(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessOutput {
    pub exit_code: Option<i32>,
    /// Bounded stdout preview. Full stream lives in [`Self::artifact`] when large.
    pub stdout: String,
    /// Bounded stderr preview. Full stream lives in [`Self::artifact`] when large.
    pub stderr: String,
    /// True when capture hit [`MAX_PROCESS_OUTPUT_BYTES`] or preview was replaced
    /// by an artifact-backed body.
    pub truncated: bool,
    pub duration_ms: u64,
    /// Full redacted process body (`exit_code` + stdout + stderr) when larger
    /// than [`MAX_PROCESS_PREVIEW_BYTES`] or capture-truncated.
    pub artifact: Option<DurableArtifactRef>,
    /// Seatbelt (or other) prepare evidence when spawn was sandboxed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sandbox_decision: Option<crate::SandboxDecision>,
}

/// Process execution request with policy check and bounded output.
#[derive(Debug, Clone)]
pub struct ProcessExecutionRequest {
    pub command: String,
    pub args: Vec<String>,
    pub working_dir: Option<PathBuf>,
    pub workspace_root: Option<PathBuf>,
    /// When true, macOS Seatbelt allows `network*`. Default false for shell.
    pub allow_network: bool,
    pub env: Vec<(String, String)>,
    pub origin: ActionOrigin,
    pub intent_revision: u64,
    pub timeout: Duration,
    /// Optional performance prefilter (not RiskGate). When absent, spawn proceeds unchanged.
    hook_prefilter: Option<HookPrefilter>,
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
            workspace_root: None,
            allow_network: false,
            env: Vec::new(),
            origin,
            intent_revision,
            timeout: DEFAULT_EXECUTION_TIMEOUT,
            hook_prefilter: None,
        }
    }

    /// Attach a performance-only hook prefilter catalog (optional inject).
    pub fn with_hook_prefilter(mut self, prefilter: HookPrefilter) -> Self {
        self.hook_prefilter = Some(prefilter);
        self
    }

    /// Executable basename used for hook prefilter label match (exact equality).
    pub fn spawn_label(&self) -> &str {
        self.command.rsplit('/').next().unwrap_or(&self.command)
    }

    pub fn with_working_dir(mut self, dir: PathBuf) -> Self {
        self.working_dir = Some(dir);
        self
    }

    pub fn with_workspace_root(mut self, root: PathBuf) -> Self {
        self.workspace_root = Some(root);
        self
    }

    pub fn with_allow_network(mut self, allow: bool) -> Self {
        self.allow_network = allow;
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

    /// Normalized effect identity used for admission and Effect Fence digests.
    pub fn fence_effect(&self) -> NormalizedEffect {
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
        NormalizedEffect::process_spawn(self.origin, summary, target)
    }

    /// Prepare process execution through policy and effect seam.
    /// Returns either immediate Allow, NeedsApproval with deferred effect, or Deny.
    pub fn request(&self, seam: &EffectSeam) -> Result<EffectAdmission, ProcessExecutionError> {
        let effect = self.fence_effect();

        let mut argv = vec![self.command.clone()];
        argv.extend(self.args.iter().cloned());

        Ok(seam.request_with_argv(effect, self.intent_revision, Some(&argv)))
    }

    /// Execute the process after policy approval.
    ///
    /// Capture is bounded to [`MAX_PROCESS_OUTPUT_BYTES`] per stream. Bodies that
    /// exceed [`MAX_PROCESS_PREVIEW_BYTES`] (or hit the capture ceiling) are stored
    /// in `artifacts`; the returned strings stay preview-sized.
    /// Requires AdmittedOperation token proving the effect passed admission.
    ///
    /// On macOS, spawn goes through Seatbelt (`production_sandbox_provider`).
    /// Non-macOS keeps path-scope admission only (direct spawn).
    pub async fn execute(
        &self,
        _admission: &crate::AdmittedOperation,
        artifacts: &DurableArtifactStore,
    ) -> Result<ProcessOutput, ProcessExecutionError> {
        let start = std::time::Instant::now();
        // Always run performance prefilter (empty catalog = AllowContinue).
        // Optional `with_hook_prefilter` replaces the default catalog.
        let default_prefilter = HookPrefilter::default();
        let prefilter = self.hook_prefilter.as_ref().unwrap_or(&default_prefilter);
        match spawn_stub(prefilter, self.spawn_label())? {
            SpawnStubOutcome::WouldSpawn => {}
            SpawnStubOutcome::Skipped => {
                return Ok(skipped_by_prefilter_output(
                    start.elapsed().as_millis() as u64,
                    self.spawn_label(),
                ));
            }
        }
        let mut spawned = self.spawn_child()?;
        let sandbox_decision = spawned.sandbox_decision.take();
        let child = &mut spawned.child;

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
        let capture_truncated = stdout_truncated || stderr_truncated;

        finalize_process_output(
            exit_status.code(),
            stdout_output,
            stderr_output,
            capture_truncated,
            duration_ms,
            artifacts,
            sandbox_decision,
        )
    }

    fn spawn_child(&self) -> Result<SpawnedChild, ProcessExecutionError> {
        #[cfg(target_os = "macos")]
        {
            self.spawn_sandboxed()
        }
        #[cfg(not(target_os = "macos"))]
        {
            self.spawn_direct()
        }
    }

    #[cfg(not(target_os = "macos"))]
    fn spawn_direct(&self) -> Result<SpawnedChild, ProcessExecutionError> {
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

        let child = cmd
            .spawn()
            .map_err(|e| ProcessExecutionError::ExecutionFailed(e.to_string()))?;
        Ok(SpawnedChild {
            child,
            _sandbox: None,
            sandbox_decision: None,
        })
    }

    #[cfg(target_os = "macos")]
    fn spawn_sandboxed(&self) -> Result<SpawnedChild, ProcessExecutionError> {
        let workspace_root = self.workspace_root.as_ref().ok_or_else(|| {
            ProcessExecutionError::SandboxDenied(
                "workspace_root required for macOS Seatbelt spawn".into(),
            )
        })?;
        let working_dir = self.working_dir.as_ref().unwrap_or(workspace_root);

        let request = SandboxCommandRequest {
            executable: &self.command,
            args: &self.args,
            workspace_root,
            working_dir,
            explicit_env: &self.env,
            allow_network: self.allow_network,
        };

        let mut prepared = production_sandbox_provider()
            .prepare(&request)
            .map_err(map_sandbox_error)?;
        let sandbox_decision = Some(prepared.decision().clone());
        let child = prepared
            .command_mut()
            .spawn()
            .map_err(|e| ProcessExecutionError::ExecutionFailed(e.to_string()))?;
        Ok(SpawnedChild {
            child,
            _sandbox: Some(prepared),
            sandbox_decision,
        })
    }
}

/// Holds the child and optional Seatbelt session temp until capture finishes.
struct SpawnedChild {
    child: tokio::process::Child,
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    _sandbox: Option<SandboxKeepAlive>,
    sandbox_decision: Option<crate::SandboxDecision>,
}

#[cfg(target_os = "macos")]
type SandboxKeepAlive = PreparedSandboxCommand;

#[cfg(not(target_os = "macos"))]
type SandboxKeepAlive = ();

#[cfg(target_os = "macos")]
fn map_sandbox_error(error: SandboxError) -> ProcessExecutionError {
    match error {
        SandboxError::Unavailable => ProcessExecutionError::SandboxUnavailable,
        other => ProcessExecutionError::SandboxDenied(other.to_string()),
    }
}

/// Build a durable process observation: keep a bounded preview, store the full
/// redacted body when it exceeds the preview bound or capture truncated.
fn finalize_process_output(
    exit_code: Option<i32>,
    stdout: String,
    stderr: String,
    capture_truncated: bool,
    duration_ms: u64,
    artifacts: &DurableArtifactStore,
    sandbox_decision: Option<crate::SandboxDecision>,
) -> Result<ProcessOutput, ProcessExecutionError> {
    let full = crate::tools::redact_text(&format!(
        "exit_code={exit_code:?}\nstdout:\n{stdout}\nstderr:\n{stderr}"
    ));
    let needs_artifact = capture_truncated || full.len() > MAX_PROCESS_PREVIEW_BYTES;

    let artifact = if needs_artifact {
        Some(
            artifacts
                .store(full.as_bytes())
                .map_err(|e| ProcessExecutionError::Artifact(e.to_string()))?,
        )
    } else {
        None
    };

    let (stdout_preview, stderr_preview) = if needs_artifact {
        (truncate_preview(&stdout), truncate_preview(&stderr))
    } else {
        (stdout, stderr)
    };

    Ok(ProcessOutput {
        exit_code,
        stdout: stdout_preview,
        stderr: stderr_preview,
        truncated: needs_artifact,
        duration_ms,
        artifact,
        sandbox_decision,
    })
}

fn skipped_by_prefilter_output(duration_ms: u64, label: &str) -> ProcessOutput {
    ProcessOutput {
        exit_code: None,
        stdout: String::new(),
        stderr: format!("hook prefilter skipped spawn for label `{label}`"),
        truncated: false,
        duration_ms,
        artifact: None,
        sandbox_decision: None,
    }
}

impl From<crate::SpawnStubError> for ProcessExecutionError {
    fn from(err: crate::SpawnStubError) -> Self {
        match err {
            crate::SpawnStubError::Denied(label) => Self::PrefilterDenied(label),
            crate::SpawnStubError::ExternalForCritical { label } => {
                Self::PrefilterExternalForCritical { label }
            }
        }
    }
}

fn truncate_preview(input: &str) -> String {
    if input.len() <= MAX_PROCESS_PREVIEW_BYTES {
        return input.to_owned();
    }
    let mut end = MAX_PROCESS_PREVIEW_BYTES;
    while end > 0 && !input.is_char_boundary(end) {
        end -= 1;
    }
    let mut preview = input[..end].to_owned();
    preview.push_str("\n...[preview truncated; full result is stored as an artifact]");
    preview
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
///
/// Agent-origin Allow with an attached [`EffectFenceLedger`] runs under
/// write-ahead Effect Fence (`prepare_fence_for_allowed` → spawn → observe/fail).
/// User-origin Allow stays unfenced (ledger ignored). ToolOrchestrator bash
/// already fences via `execute_after_approval_*_fence` and calls
/// [`ProcessExecutionRequest::execute`] directly — do not double-fence there.
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

    /// Execute after approval (or immediate Allow), without Effect Fence.
    /// Prefer [`Self::execute_with_admission_and_fence`] for agent-origin callers
    /// that own a session ledger.
    pub async fn execute_with_admission(
        &self,
        req: &ProcessExecutionRequest,
        artifacts: &DurableArtifactStore,
    ) -> Result<ProcessOutput, ProcessExecutionError> {
        self.execute_with_admission_and_optional_fence(req, artifacts, None)
            .await
    }

    /// Execute after Allow. Agent-origin + ledger → write-ahead fence.
    /// User-origin stays unfenced even when `ledger` is present.
    pub async fn execute_with_admission_and_fence(
        &self,
        req: &ProcessExecutionRequest,
        artifacts: &DurableArtifactStore,
        ledger: &EffectFenceLedger,
    ) -> Result<ProcessOutput, ProcessExecutionError> {
        self.execute_with_admission_and_optional_fence(req, artifacts, Some(ledger))
            .await
    }

    async fn execute_with_admission_and_optional_fence(
        &self,
        req: &ProcessExecutionRequest,
        artifacts: &DurableArtifactStore,
        ledger: Option<&EffectFenceLedger>,
    ) -> Result<ProcessOutput, ProcessExecutionError> {
        let admission = self.request(req)?;
        match admission {
            crate::EffectAdmission::Allow(token) => match (req.origin, ledger) {
                (ActionOrigin::Agent, Some(ledger)) => {
                    self.execute_agent_under_fence(req, &token, artifacts, ledger)
                        .await
                }
                _ => req.execute(&token, artifacts).await,
            },
            crate::EffectAdmission::NeedsApproval(_) => {
                Err(ProcessExecutionError::ApprovalRequired)
            }
            crate::EffectAdmission::Deny { reason } => {
                Err(ProcessExecutionError::PolicyDenied(reason))
            }
        }
    }

    /// Post-admission agent spawn: write-ahead fence then OS execute.
    async fn execute_agent_under_fence(
        &self,
        req: &ProcessExecutionRequest,
        admission: &crate::AdmittedOperation,
        artifacts: &DurableArtifactStore,
        ledger: &EffectFenceLedger,
    ) -> Result<ProcessOutput, ProcessExecutionError> {
        let effect = req.fence_effect();
        let identity = self
            .seam
            .prepare_fence_for_allowed(&effect, ledger)
            .map_err(ProcessExecutionError::EffectFence)?;
        match req.execute(admission, artifacts).await {
            Ok(output) => {
                if let Err(err) = ledger.observe(&identity, "process executed") {
                    let _ = ledger.mark_unknown(
                        &identity,
                        format!("observe persist failed after process execute: {err}"),
                    );
                    return Err(ProcessExecutionError::EffectFence(format!(
                        "effect fence observe failed after process execute: {err}"
                    )));
                }
                Ok(output)
            }
            Err(error) => {
                let _ = ledger.mark_failed(&identity, error.to_string());
                Err(error)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        HookAction, HookPrefilter, HookRule, HookTrustLevel, PolicyEngine, Sandbox, SandboxScope,
    };

    fn test_seam() -> EffectSeam {
        let workspace = test_workspace();
        let policy = PolicyEngine::new(SandboxScope::local_workspace(workspace.clone()));
        EffectSeam::with_sandbox(policy, Sandbox::workspace(workspace))
    }

    fn test_workspace() -> PathBuf {
        std::env::temp_dir()
    }

    fn temp_artifacts() -> (tempfile::TempDir, DurableArtifactStore) {
        let root = tempfile::tempdir().expect("artifact root");
        let store = DurableArtifactStore::open(root.path()).expect("open artifact store");
        (root, store)
    }

    fn user_request(command: &str, args: Vec<String>) -> ProcessExecutionRequest {
        ProcessExecutionRequest::new(command, args, ActionOrigin::User, 1)
            .with_workspace_root(test_workspace())
            .with_working_dir(test_workspace())
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
        let (_root, artifacts) = temp_artifacts();
        let request = user_request("echo", vec!["hello".into()]);

        let admission = request.request(&seam).unwrap();
        let token = match admission {
            crate::EffectAdmission::Allow(t) => t,
            _ => panic!("expected Allow for user echo"),
        };

        let result = request.execute(&token, &artifacts).await;
        assert!(result.is_ok());

        let output = result.unwrap();
        assert_eq!(output.exit_code, Some(0));
        assert!(output.stdout.contains("hello"));
        assert!(!output.truncated);
        assert!(output.artifact.is_none());
    }

    #[tokio::test]
    async fn process_execution_handles_failure() {
        let seam = test_seam();
        let (_root, artifacts) = temp_artifacts();
        let request = user_request("false", vec![]);

        let admission = request.request(&seam).unwrap();
        let token = match admission {
            crate::EffectAdmission::Allow(t) => t,
            _ => panic!("expected Allow for user false"),
        };

        let result = request.execute(&token, &artifacts).await;
        assert!(result.is_ok());

        let output = result.unwrap();
        assert_ne!(output.exit_code, Some(0));
    }

    #[tokio::test]
    async fn process_execution_respects_timeout() {
        let seam = test_seam();
        let (_root, artifacts) = temp_artifacts();
        let request =
            user_request("sleep", vec!["10".into()]).with_timeout(Duration::from_millis(100));

        let admission = request.request(&seam).unwrap();
        let token = match admission {
            crate::EffectAdmission::Allow(t) => t,
            _ => panic!("expected Allow for user sleep"),
        };

        let result = request.execute(&token, &artifacts).await;
        assert!(matches!(result, Err(ProcessExecutionError::Timeout(_))));
    }

    #[tokio::test]
    async fn unadmitted_process_cannot_execute() {
        // Regression test for A1: execute() requires admission token.
        // Without calling request(), there's no way to get AdmittedOperation,
        // so direct execute() is a compile error. This test proves the API contract.
        let request =
            ProcessExecutionRequest::new("echo", vec!["bypass".into()], ActionOrigin::Agent, 1);
        let (_root, artifacts) = temp_artifacts();

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
                let _ = request.execute(&token, &artifacts).await;
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

    #[tokio::test]
    async fn large_stdout_is_stored_as_durable_artifact() {
        let seam = test_seam();
        let (artifact_root, artifacts) = temp_artifacts();
        // Synthetic large output well above preview bound, well below capture ceiling.
        let byte_count = MAX_PROCESS_PREVIEW_BYTES + 4096;
        let request = ProcessExecutionRequest::new(
            "/bin/sh",
            vec![
                "-c".into(),
                format!("yes x | tr -d '\\n' | head -c {byte_count}"),
            ],
            ActionOrigin::User,
            1,
        )
        .with_workspace_root(test_workspace())
        .with_working_dir(test_workspace());

        let admission = request.request(&seam).unwrap();
        let token = match admission {
            crate::EffectAdmission::Allow(t) => t,
            _ => panic!("expected Allow for user shell"),
        };

        let output = request
            .execute(&token, &artifacts)
            .await
            .expect("execute large stdout");
        assert!(output.truncated, "large stdout must mark truncated");
        let artifact = output.artifact.expect("artifact ref for large stdout");
        assert!(
            output.stdout.len()
                <= MAX_PROCESS_PREVIEW_BYTES
                    + "\n...[preview truncated; full result is stored as an artifact]".len()
        );

        let reopened =
            DurableArtifactStore::open(artifact_root.path()).expect("reopen artifact store");
        let full = reopened
            .read(&artifact.id)
            .expect("read durable process body");
        let full_text = String::from_utf8(full).expect("utf8 artifact");
        assert!(full_text.contains("stdout:"));
        assert!(full_text.len() > MAX_PROCESS_PREVIEW_BYTES);
        assert!(full_text.matches('x').count() > MAX_PROCESS_PREVIEW_BYTES);
    }

    #[test]
    fn spawn_label_uses_executable_basename() {
        let request =
            ProcessExecutionRequest::new("/bin/echo", vec!["hi".into()], ActionOrigin::User, 1);
        assert_eq!(request.spawn_label(), "echo");
    }

    #[tokio::test]
    async fn spawn_path_invokes_hook_prefilter_skip_spawn() {
        let seam = test_seam();
        let (_root, artifacts) = temp_artifacts();
        let prefilter = HookPrefilter::new(vec![HookRule::new("echo", HookAction::SkipSpawn)]);
        let request = user_request("echo", vec!["hello".into()]).with_hook_prefilter(prefilter);

        let admission = request.request(&seam).unwrap();
        let token = match admission {
            crate::EffectAdmission::Allow(t) => t,
            _ => panic!("expected Allow for user echo"),
        };

        let output = request
            .execute(&token, &artifacts)
            .await
            .expect("skip spawn is not an error");
        assert_eq!(output.exit_code, None);
        assert!(output.stdout.is_empty());
        assert!(output.stderr.contains("skipped spawn"));
        assert!(output.stderr.contains("echo"));
        assert!(!output.stdout.contains("hello"));
    }

    #[tokio::test]
    async fn spawn_path_invokes_hook_prefilter_deny() {
        let seam = test_seam();
        let (_root, artifacts) = temp_artifacts();
        let prefilter = HookPrefilter::new(vec![HookRule::new("sleep", HookAction::Deny)]);
        let request = user_request("sleep", vec!["10".into()]).with_hook_prefilter(prefilter);

        let admission = request.request(&seam).unwrap();
        let token = match admission {
            crate::EffectAdmission::Allow(t) => t,
            _ => panic!("expected Allow for user sleep"),
        };

        let err = request.execute(&token, &artifacts).await.unwrap_err();
        assert!(matches!(err, ProcessExecutionError::PrefilterDenied(label) if label == "sleep"));
    }

    #[tokio::test]
    async fn spawn_path_hook_prefilter_external_critical_fail_closed() {
        let seam = test_seam();
        let (_root, artifacts) = temp_artifacts();
        let prefilter = HookPrefilter::new(vec![
            HookRule::security_critical("false", HookAction::Deny)
                .with_trust(HookTrustLevel::External),
        ]);
        let request = user_request("false", vec![]).with_hook_prefilter(prefilter);

        let admission = request.request(&seam).unwrap();
        let token = match admission {
            crate::EffectAdmission::Allow(t) => t,
            _ => panic!("expected Allow for user false"),
        };

        let err = request.execute(&token, &artifacts).await.unwrap_err();
        assert!(matches!(
            err,
            ProcessExecutionError::PrefilterExternalForCritical { label } if label == "false"
        ));
    }

    #[tokio::test]
    async fn small_stdout_stays_inline_without_artifact() {
        let seam = test_seam();
        let (_root, artifacts) = temp_artifacts();
        let request = user_request("printf", vec!["tiny".into()]);

        let admission = request.request(&seam).unwrap();
        let token = match admission {
            crate::EffectAdmission::Allow(t) => t,
            _ => panic!("expected Allow"),
        };

        let output = request
            .execute(&token, &artifacts)
            .await
            .expect("execute small");
        assert!(!output.truncated);
        assert!(output.artifact.is_none());
        assert_eq!(output.stdout, "tiny");
    }

    fn agent_allow_seam(workspace: &std::path::Path) -> EffectSeam {
        use crate::{PolicyConfig, PolicyEngine, Sandbox, SandboxScope};
        let config =
            PolicyConfig::parse(r#"{"version":1,"overrides":{"spawn_process":"allow"}}"#).unwrap();
        let policy = PolicyEngine::with_config(SandboxScope::local_workspace(workspace), config);
        EffectSeam::with_sandbox(policy, Sandbox::workspace(workspace))
    }

    fn agent_allow_request(command: &str, args: Vec<String>) -> ProcessExecutionRequest {
        ProcessExecutionRequest::new(command, args, ActionOrigin::Agent, 1)
            .with_workspace_root(test_workspace())
            .with_working_dir(test_workspace())
    }

    #[tokio::test]
    async fn agent_process_allow_refuses_when_prior_unknown() {
        use crate::{
            EffectFenceLedger, EffectInvocationIdentity, EventStore, FenceState, MemoryEventStore,
            records_from_events,
        };
        use std::sync::Arc;
        use uuid::Uuid;

        let workspace = test_workspace();
        let seam = agent_allow_seam(&workspace);
        let exec = ProcessExecution::new(seam);
        let (_root, artifacts) = temp_artifacts();
        let store: Arc<dyn EventStore> = Arc::new(MemoryEventStore::default());
        let session = Uuid::new_v4();
        let ledger = EffectFenceLedger::new(Arc::clone(&store), session);

        let request = agent_allow_request("true", vec![]);
        let effect = request.fence_effect();
        let identity = EffectInvocationIdentity::from_effect(&effect);
        ledger.prepare_and_start(&identity).expect("prepare");
        ledger
            .mark_unknown(&identity, "crash mid-flight")
            .expect("unknown");

        let err = exec
            .execute_with_admission_and_fence(&request, &artifacts, &ledger)
            .await
            .expect_err("Unknown fence must refuse blind replay");
        assert!(
            matches!(err, ProcessExecutionError::EffectFence(ref reason) if reason.contains("Unknown") || reason.contains("refuse")),
            "expected fence refuse, got {err:?}"
        );

        let records = records_from_events(&store.list(session).expect("list"));
        assert!(
            records.values().any(|r| r.state == FenceState::Unknown
                && r.identity.args_digest == identity.args_digest),
            "durable Unknown fence must remain"
        );
    }

    #[tokio::test]
    async fn agent_process_allow_emits_fence_observed() {
        use crate::{
            EffectFenceLedger, EventStore, FenceState, MemoryEventStore, args_digest_for_effect,
            records_from_events,
        };
        use std::sync::Arc;
        use uuid::Uuid;

        let workspace = test_workspace();
        let seam = agent_allow_seam(&workspace);
        let exec = ProcessExecution::new(seam);
        let (_root, artifacts) = temp_artifacts();
        let store: Arc<dyn EventStore> = Arc::new(MemoryEventStore::default());
        let session = Uuid::new_v4();
        let ledger = EffectFenceLedger::new(Arc::clone(&store), session);

        let request = agent_allow_request("true", vec![]);
        let output = exec
            .execute_with_admission_and_fence(&request, &artifacts, &ledger)
            .await
            .expect("agent allow + fence execute");
        assert_eq!(output.exit_code, Some(0));

        let effect = request.fence_effect();
        let records = records_from_events(&store.list(session).expect("list"));
        assert!(
            records.values().any(|r| r.state == FenceState::Observed
                && r.identity.args_digest == args_digest_for_effect(&effect)),
            "expected Observed fence for agent ProcessExecution; records={records:?}"
        );
    }

    #[tokio::test]
    async fn user_process_allow_stays_unfenced_with_ledger() {
        use crate::{
            EffectFenceLedger, EventStore, FenceState, MemoryEventStore, records_from_events,
        };
        use std::sync::Arc;
        use uuid::Uuid;

        let seam = test_seam();
        let exec = ProcessExecution::new(seam);
        let (_root, artifacts) = temp_artifacts();
        let store: Arc<dyn EventStore> = Arc::new(MemoryEventStore::default());
        let session = Uuid::new_v4();
        let ledger = EffectFenceLedger::new(Arc::clone(&store), session);

        let request = user_request("true", vec![]);
        let output = exec
            .execute_with_admission_and_fence(&request, &artifacts, &ledger)
            .await
            .expect("user allow unfenced");
        assert_eq!(output.exit_code, Some(0));

        let records = records_from_events(&store.list(session).expect("list"));
        assert!(
            records
                .values()
                .all(|r| !matches!(r.state, FenceState::Observed | FenceState::Started)),
            "user-origin must not write Effect Fence; records={records:?}"
        );
    }
}
