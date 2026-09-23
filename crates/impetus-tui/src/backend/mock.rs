use anyhow::{Result, anyhow};
use async_trait::async_trait;
use impetus_client::protocol::{
    CheckpointInfo, DurableArtifactMeta, DurableArtifactRef, GitBranchInfo, GitChangeKind,
    GitChangedFile, GitCurrentBranch, GitDiffPayload, GitStatusSnapshot, ModelAvailability,
    ModelProviderHealthLabel, ModelProviderStatus, SessionModelSelection, WorkspaceDirEntry,
    WorkspaceDirListing, WorkspaceFileContent,
};
use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::{Mutex, mpsc};
use uuid::Uuid;

use super::{UiBackend, UiEventStream};
use crate::model::{
    ApprovalCard, ApprovalDetailView, BudgetState, ConnectionInfo, ExecutionMode, SessionSummary,
    UiEvent, UiEventKind,
};

#[derive(Clone)]
pub struct MockBackend {
    inner: Arc<MockInner>,
}

fn spawn_detached(future: impl std::future::Future<Output = ()> + Send + 'static) {
    drop(tokio::spawn(future));
}

impl Default for MockBackend {
    fn default() -> Self {
        Self::new()
    }
}

struct MockInner {
    sessions: Mutex<Vec<SessionSummary>>,
    checkpoints: Mutex<Vec<CheckpointInfo>>,
    execution_modes: Mutex<HashMap<Uuid, ExecutionMode>>,
    session_models: Mutex<HashMap<Uuid, SessionModelSelection>>,
    provider_catalog: Mutex<Vec<ModelProviderStatus>>,
    branches: Mutex<Vec<GitBranchInfo>>,
    subscribers: Mutex<HashMap<Uuid, Vec<mpsc::Sender<Vec<UiEvent>>>>>,
    approval_details: Mutex<HashMap<Uuid, ApprovalDetailView>>,
    sequence: AtomicU64,
    pty_next_id: AtomicU64,
    pty_pending: Mutex<HashMap<u64, Vec<u8>>>,
}

impl MockBackend {
    pub fn new() -> Self {
        let first = Uuid::from_u128(0x4d8aa5ef_e33f_4ce8_8e99_0e1af5042d31);
        let second = Uuid::from_u128(0x2b5f4dea_7339_45b9_a826_c98af4e352ad);
        Self {
            inner: Arc::new(MockInner {
                sessions: Mutex::new(vec![
                    SessionSummary {
                        id: first,
                        label: "TUI architecture".to_owned(),
                        status: "working".to_owned(),
                        workspace: Some("~/dev/impetus".to_owned()),
                    },
                    SessionSummary {
                        id: second,
                        label: "Router hardening".to_owned(),
                        status: "saved".to_owned(),
                        workspace: Some("~/dev/impetus".to_owned()),
                    },
                ]),
                checkpoints: Mutex::new(Vec::new()),
                execution_modes: Mutex::new(HashMap::new()),
                session_models: Mutex::new(HashMap::new()),
                provider_catalog: Mutex::new(demo_provider_catalog()),
                branches: Mutex::new(vec![
                    GitBranchInfo {
                        name: "main".to_owned(),
                        current: true,
                        upstream: Some("origin/main".to_owned()),
                    },
                    GitBranchInfo {
                        name: "feature/demo".to_owned(),
                        current: false,
                        upstream: None,
                    },
                ]),
                subscribers: Mutex::new(HashMap::new()),
                approval_details: Mutex::new(HashMap::new()),
                sequence: AtomicU64::new(1),
                pty_next_id: AtomicU64::new(1),
                pty_pending: Mutex::new(HashMap::new()),
            }),
        }
    }

    fn next_event(&self, kind: UiEventKind) -> UiEvent {
        UiEvent {
            sequence: self.inner.sequence.fetch_add(1, Ordering::Relaxed),
            at_unix_ms: now_ms(),
            kind,
        }
    }

    async fn publish(&self, session_id: Uuid, events: Vec<UiEvent>) {
        let senders = {
            let mut subscribers = self.inner.subscribers.lock().await;
            subscribers.remove(&session_id).unwrap_or_default()
        };
        let mut retained = Vec::with_capacity(senders.len());
        for sender in senders {
            if sender.send(events.clone()).await.is_ok() {
                retained.push(sender);
            }
        }
        if !retained.is_empty() {
            self.inner
                .subscribers
                .lock()
                .await
                .entry(session_id)
                .or_default()
                .extend(retained);
        }
    }

    fn seed_events(&self) -> Vec<UiEvent> {
        let run_id = Uuid::from_u128(0x93dcaf73_6990_45d5_8f7e_954f186233bb);
        vec![
            self.next_event(UiEventKind::SessionCreated),
            self.next_event(UiEventKind::SessionWorkspace {
                workspace: "/Users/anton/dev/impetus".to_owned(),
            }),
            self.next_event(UiEventKind::UserInput {
                text: "Create a production-ready standalone TUI without moving runtime authority into the client.".to_owned(),
                artifact: None,
            }),
            self.next_event(UiEventKind::Plan {
                summary: "1. Keep `impetusd` authoritative.\n2. Add a transport-neutral presentation backend.\n3. Render typed event cards, approvals, sessions, modes and diagnostics.\n4. Verify narrow and wide terminal layouts.".to_owned(),
            }),
            self.next_event(UiEventKind::RunStarted { run_id }),
            self.next_event(UiEventKind::ToolObserved {
                call_id: "tool-001".to_owned(),
                name: "read_file".to_owned(),
                arguments: "crates/impetus-client/src/lib.rs".to_owned(),
                outcome: "Success".to_owned(),
                preview: "HarnessClient exposes create, resume, prompt, cancel, approval and durable subscription operations.".to_owned(),
                artifact: None,
                error: None,
            }),
            self.next_event(UiEventKind::ToolObserved {
                call_id: "tool-002".to_owned(),
                name: "git_diff".to_owned(),
                arguments: "HEAD~1..HEAD".to_owned(),
                outcome: "Success".to_owned(),
                preview: concat!(
                    "{\n",
                    "  \"summary\": \"1 file changed, 2 insertions(+), 1 deletion(-)\",\n",
                    "  \"files_changed\": 1,\n",
                    "  \"insertions\": 2,\n",
                    "  \"deletions\": 1,\n",
                    "  \"hunks\": [{\n",
                    "    \"file\": \"crates/impetus/src/tui.rs\",\n",
                    "    \"old_start\": 10,\n",
                    "    \"old_lines\": 3,\n",
                    "    \"new_start\": 10,\n",
                    "    \"new_lines\": 4,\n",
                    "    \"preview\": \" pub async fn run(...) {\\n-    old_loop()\\n+    impetus_tui::run(...).await\\n+    // paced stream + diff view\\n }\"\n",
                    "  }]\n",
                    "}"
                )
                .to_owned(),
                artifact: Some("artifact:diff-demo".to_owned()),
                error: None,
            }),
            self.next_event(UiEventKind::AgentFinal {
                run_id,
                text: "The UI shell is now isolated behind `UiBackend`. The real adapter uses `HarnessClient`; the demo adapter drives exactly the same widgets.\n\n```rust\npub trait UiBackend: Send + Sync {\n    async fn subscribe(&self, session: Uuid, after: u64) -> Result<Box<dyn UiEventStream>>;\n}\n```\n\nPress **F1** for the keymap, **F2** for sessions, **F4** for execution modes, or type `/` for commands.".to_owned(),
            }),
            self.next_event(UiEventKind::BudgetUpdated(BudgetState {
                turns_used: 4,
                tokens_used: 12_480,
                context_used_percent: 31,
                compactions: 0,
                warning: None,
            })),
            self.next_event(UiEventKind::RunCompleted { run_id }),
        ]
    }

    async fn publish_prompt(
        &self,
        session_id: Uuid,
        text: String,
        artifact: Option<DurableArtifactRef>,
    ) -> Result<String> {
        let backend = self.clone();
        spawn_detached(async move {
            let run_id = Uuid::new_v4();
            backend
                .publish(
                    session_id,
                    vec![
                        backend.next_event(UiEventKind::UserInput {
                            text: text.clone(),
                            artifact,
                        }),
                        backend.next_event(UiEventKind::RunStarted { run_id }),
                    ],
                )
                .await;
            tokio::time::sleep(std::time::Duration::from_millis(180)).await;
            backend
                .publish(
                    session_id,
                    vec![backend.next_event(UiEventKind::AgentChunk {
                        run_id,
                        chunk_id: 1,
                        text: "I inspected the request and mapped it onto the existing client contract. ".to_owned(),
                    })],
                )
                .await;
            tokio::time::sleep(std::time::Duration::from_millis(220)).await;
            backend
                .publish(
                    session_id,
                    vec![backend.next_event(UiEventKind::AgentChunk {
                        run_id,
                        chunk_id: 2,
                        text: "Read-only work can proceed immediately; state-changing work remains approval-gated.".to_owned(),
                    })],
                )
                .await;

            if text.to_lowercase().contains("write")
                || text.to_lowercase().contains("approval")
                || text.to_lowercase().contains("измен")
            {
                let approval_id = Uuid::new_v4();
                let _ = backend.inner.approval_details.lock().await.insert(
                    approval_id,
                    ApprovalDetailView {
                        diff_preview: Some(
                            "--- a/crates/impetus/src/tui.rs\n+++ b/crates/impetus/src/tui.rs\n@@\n-pub async fn run(...) { old_loop() }\n+pub async fn run(...) { impetus_tui::run(...).await }"
                                .to_owned(),
                        ),
                        diff_observation: None,
                        affected_files: vec!["crates/impetus/src/tui.rs".to_owned()],
                        estimated_scope: Some("Lines(2)".to_owned()),
                        attachment_refs: vec![],
                        attachments: vec![],
                    },
                );
                let approval = ApprovalCard {
                    id: approval_id,
                    action_kind: "WriteFile".to_owned(),
                    summary: "replace the legacy line-oriented TUI wrapper".to_owned(),
                    target: Some("crates/impetus/src/tui.rs".to_owned()),
                    reason: "changes workspace files".to_owned(),
                    fingerprint: "demo:8f48…e21c".to_owned(),
                    detail: None,
                };
                backend
                    .publish(
                        session_id,
                        vec![backend.next_event(UiEventKind::ApprovalRequested { approval })],
                    )
                    .await;
            } else {
                tokio::time::sleep(std::time::Duration::from_millis(180)).await;
                backend
                    .publish(
                        session_id,
                        vec![
                            backend.next_event(UiEventKind::AgentFinal {
                                run_id,
                                text: "Done. This response is streamed through the same event path used by the real daemon adapter.".to_owned(),
                            }),
                            backend.next_event(UiEventKind::RunCompleted { run_id }),
                        ],
                    )
                    .await;
            }
        });
        Ok("Running".to_owned())
    }
}

#[async_trait]
impl UiBackend for MockBackend {
    async fn connection_info(&self) -> Result<ConnectionInfo> {
        Ok(ConnectionInfo {
            protocol_version: 4,
            capabilities: [
                "session_create",
                "session_attach",
                "session_list",
                "session_fork",
                "session_checkpoint",
                "prompt",
                "cancel",
                "subscribe",
                "resolve_approval",
                "get_approval_detail",
                "diagnostics",
                "artifact_upload",
                "artifact_read",
                "git",
                "pty",
                "list_providers",
                "session_model",
            ]
            .into_iter()
            .map(str::to_owned)
            .collect::<BTreeSet<_>>(),
            label: "demo backend · no daemon required".to_owned(),
        })
    }

    async fn list_sessions(&self) -> Result<Vec<SessionSummary>> {
        Ok(self.inner.sessions.lock().await.clone())
    }

    async fn create_session(&self, workspace_root: PathBuf) -> Result<Uuid> {
        let id = Uuid::new_v4();
        self.inner.sessions.lock().await.push(SessionSummary {
            id,
            label: format!("New session {}", crate::model::short_id(id)),
            status: "ready".to_owned(),
            workspace: Some(workspace_root.display().to_string()),
        });
        Ok(id)
    }

    async fn fork_session(&self, session_id: Uuid, up_to_sequence: u64) -> Result<Uuid> {
        let mut sessions = self.inner.sessions.lock().await;
        if !sessions.iter().any(|session| session.id == session_id) {
            return Err(anyhow!("demo session not found: {session_id}"));
        }
        let workspace = sessions
            .iter()
            .find(|session| session.id == session_id)
            .and_then(|session| session.workspace.clone());
        let id = Uuid::new_v4();
        sessions.push(SessionSummary::from_session_info(
            id,
            Some(session_id),
            Some(up_to_sequence),
            None,
            None,
            workspace,
        ));
        Ok(id)
    }

    async fn create_checkpoint(
        &self,
        session_id: Uuid,
        name: String,
        sequence: Option<u64>,
    ) -> Result<CheckpointInfo> {
        let sessions = self.inner.sessions.lock().await;
        if !sessions.iter().any(|session| session.id == session_id) {
            return Err(anyhow!("demo session not found: {session_id}"));
        }
        drop(sessions);
        let sequence = sequence.unwrap_or_else(|| self.inner.sequence.load(Ordering::Relaxed));
        let checkpoint = CheckpointInfo {
            id: Uuid::new_v4(),
            session_id,
            name,
            sequence,
            created_at_unix_ms: now_ms(),
        };
        self.inner.checkpoints.lock().await.push(checkpoint.clone());
        Ok(checkpoint)
    }

    async fn list_checkpoints(&self, session_id: Uuid) -> Result<Vec<CheckpointInfo>> {
        Ok(self
            .inner
            .checkpoints
            .lock()
            .await
            .iter()
            .filter(|checkpoint| checkpoint.session_id == session_id)
            .cloned()
            .collect())
    }

    async fn restore_checkpoint(&self, checkpoint_id: Uuid) -> Result<Uuid> {
        let checkpoint = self
            .inner
            .checkpoints
            .lock()
            .await
            .iter()
            .find(|checkpoint| checkpoint.id == checkpoint_id)
            .cloned()
            .ok_or_else(|| anyhow!("demo checkpoint not found: {checkpoint_id}"))?;
        self.fork_session(checkpoint.session_id, checkpoint.sequence)
            .await
    }

    async fn resume_session(&self, session_id: Uuid) -> Result<String> {
        if self
            .inner
            .sessions
            .lock()
            .await
            .iter()
            .any(|session| session.id == session_id)
        {
            Ok("Ready".to_owned())
        } else {
            Err(anyhow!("demo session not found: {session_id}"))
        }
    }

    async fn send_message(
        &self,
        session_id: Uuid,
        text: String,
        intent: impetus_client::protocol::UserPromptIntent,
        artifact: Option<DurableArtifactRef>,
    ) -> Result<String> {
        let _ = intent;
        self.publish_prompt(session_id, text, artifact).await
    }

    async fn send_large_paste(
        &self,
        session_id: Uuid,
        label: String,
        body: Vec<u8>,
        intent: impetus_client::protocol::UserPromptIntent,
    ) -> Result<String> {
        // Demo backend never stores bytes; only the compact label enters the timeline.
        let _ = (body, intent);
        self.publish_prompt(session_id, label, None).await
    }

    async fn upload_artifact(
        &self,
        _session_id: Uuid,
        bytes: Vec<u8>,
        content_type: Option<String>,
    ) -> Result<DurableArtifactRef> {
        let _ = content_type;
        Ok(DurableArtifactRef {
            id: format!("demo-{}", Uuid::new_v4()),
            byte_count: bytes.len(),
        })
    }

    async fn get_artifact_metadata(&self, artifact_id: String) -> Result<DurableArtifactMeta> {
        Ok(DurableArtifactMeta {
            id: artifact_id,
            byte_count: 0,
            created_unix_ms: 0,
            sha256: "demo".into(),
            content_type: Some("application/octet-stream".into()),
        })
    }

    async fn cancel(&self, session_id: Uuid) -> Result<String> {
        let run_id = Uuid::new_v4();
        self.publish(
            session_id,
            vec![self.next_event(UiEventKind::RunCancelled { run_id })],
        )
        .await;
        Ok("Cancelled".to_owned())
    }

    async fn resolve_approval(
        &self,
        session_id: Uuid,
        approval_id: Uuid,
        accepted: bool,
    ) -> Result<()> {
        let run_id = Uuid::new_v4();
        self.publish(
            session_id,
            vec![
                self.next_event(UiEventKind::ApprovalResolved {
                    approval_id,
                    accepted,
                }),
                self.next_event(UiEventKind::AgentFinal {
                    run_id,
                    text: if accepted {
                        "The exact reviewed action was approved once and resumed.".to_owned()
                    } else {
                        "The action was rejected; no mutation was performed.".to_owned()
                    },
                }),
                self.next_event(UiEventKind::RunCompleted { run_id }),
            ],
        )
        .await;
        Ok(())
    }

    async fn approval_detail(
        &self,
        _session_id: Uuid,
        approval_id: Uuid,
    ) -> Result<ApprovalDetailView> {
        self.inner
            .approval_details
            .lock()
            .await
            .get(&approval_id)
            .cloned()
            .ok_or_else(|| anyhow!("approval detail not found"))
    }

    async fn get_attachment(
        &self,
        _session_id: Uuid,
        attachment_id: Uuid,
    ) -> Result<(String, Vec<u8>)> {
        Ok((
            "text/plain".into(),
            format!("demo attachment {attachment_id}").into_bytes(),
        ))
    }

    async fn diagnostics(&self) -> Result<String> {
        Ok(serde_json::json!({
            "daemon": { "status": "demo", "protocol": 3 },
            "event_store": { "status": "ok", "durable": false },
            "sandbox": { "status": "simulated" },
            "policy": { "status": "ok", "mode": "deny | allow | needs_approval" },
            "provider_registry": { "status": "demo" }
        })
        .to_string())
    }

    async fn list_child_runs(&self, _session_id: Uuid) -> Result<String> {
        Ok("demo-child  Explore  completed  mock-ok".into())
    }

    async fn get_execution_mode(&self, session_id: Uuid) -> Result<ExecutionMode> {
        Ok(self
            .inner
            .execution_modes
            .lock()
            .await
            .get(&session_id)
            .copied()
            .unwrap_or(ExecutionMode::Ask))
    }

    async fn set_execution_mode(
        &self,
        session_id: Uuid,
        mode: ExecutionMode,
    ) -> Result<ExecutionMode> {
        self.inner
            .execution_modes
            .lock()
            .await
            .insert(session_id, mode);
        Ok(mode)
    }

    async fn list_providers(&self) -> Result<Vec<ModelProviderStatus>> {
        Ok(self.inner.provider_catalog.lock().await.clone())
    }

    async fn get_session_model(&self, session_id: Uuid) -> Result<SessionModelSelection> {
        if let Some(selection) = self
            .inner
            .session_models
            .lock()
            .await
            .get(&session_id)
            .cloned()
        {
            return Ok(selection);
        }
        let catalog = self.inner.provider_catalog.lock().await;
        let default = catalog
            .iter()
            .find(|row| row.is_default)
            .or_else(|| catalog.first())
            .ok_or_else(|| anyhow!("demo catalog empty"))?;
        Ok(SessionModelSelection {
            provider_id: default.provider_id.clone(),
            model_id: default.model_id.clone(),
            reasoning_effort: default.default_reasoning_effort.clone(),
            service_tier: None,
            provider_options: serde_json::Value::Null,
        })
    }

    async fn set_session_model(
        &self,
        session_id: Uuid,
        provider_id: String,
        model_id: String,
        reasoning_effort: Option<String>,
    ) -> Result<SessionModelSelection> {
        let catalog = self.inner.provider_catalog.lock().await;
        let row = catalog
            .iter()
            .find(|row| row.provider_id == provider_id && row.model_id == model_id)
            .ok_or_else(|| anyhow!("unknown provider/model in demo catalog"))?;
        if matches!(row.availability, ModelAvailability::Unavailable)
            || matches!(row.health, ModelProviderHealthLabel::Unavailable { .. })
        {
            return Err(anyhow!("model unavailable"));
        }
        if let Some(effort) = reasoning_effort.as_ref()
            && !row.reasoning_efforts.is_empty()
            && !row.reasoning_efforts.iter().any(|e| e == effort)
        {
            return Err(anyhow!("reasoning effort not advertised for model"));
        }
        let selection = SessionModelSelection {
            provider_id,
            model_id,
            reasoning_effort,
            service_tier: None,
            provider_options: serde_json::Value::Null,
        };
        drop(catalog);
        self.inner
            .session_models
            .lock()
            .await
            .insert(session_id, selection.clone());
        Ok(selection)
    }

    async fn list_workspace_dir(
        &self,
        _session_id: Uuid,
        path: PathBuf,
    ) -> Result<WorkspaceDirListing> {
        Ok(demo_list_workspace_dir(&path))
    }

    async fn read_workspace_file(
        &self,
        _session_id: Uuid,
        path: PathBuf,
        _max_bytes: Option<usize>,
    ) -> Result<WorkspaceFileContent> {
        demo_read_workspace_file(&path)
    }

    async fn search_workspace_files(
        &self,
        _session_id: Uuid,
        _path: PathBuf,
        pattern: String,
    ) -> Result<impetus_client::protocol::WorkspaceSearchResult> {
        let needle = pattern.to_ascii_lowercase();
        let hits = [
            ("src/main.rs", 12u32, "fn main() { /* needle demo */ }"),
            ("README.md", 3u32, "Search hits come from harness IPC."),
        ]
        .into_iter()
        .filter(|(_, _, text)| text.to_ascii_lowercase().contains(&needle) || needle.is_empty())
        .map(
            |(path, line, text)| impetus_client::protocol::WorkspaceSearchHit {
                path: path.to_owned(),
                line,
                text: text.to_owned(),
            },
        )
        .collect::<Vec<_>>();
        Ok(impetus_client::protocol::WorkspaceSearchResult {
            path: ".".to_owned(),
            pattern,
            hits,
            truncated: false,
        })
    }

    async fn list_branches(&self, _session_id: Uuid) -> Result<Vec<GitBranchInfo>> {
        Ok(self.inner.branches.lock().await.clone())
    }

    async fn get_current_branch(&self, _session_id: Uuid) -> Result<GitCurrentBranch> {
        let branches = self.inner.branches.lock().await;
        let current = branches.iter().find(|branch| branch.current);
        Ok(GitCurrentBranch {
            name: current.map(|branch| branch.name.clone()),
            detached: false,
            head_sha: Some("demo".to_owned()),
        })
    }

    async fn create_branch(
        &self,
        _session_id: Uuid,
        name: String,
        checkout: bool,
    ) -> Result<GitCurrentBranch> {
        let mut branches = self.inner.branches.lock().await;
        if branches.iter().any(|branch| branch.name == name) {
            return Err(anyhow!("branch already exists: {name}"));
        }
        if checkout {
            for branch in branches.iter_mut() {
                branch.current = false;
            }
        }
        branches.push(GitBranchInfo {
            name: name.clone(),
            current: checkout,
            upstream: None,
        });
        Ok(GitCurrentBranch {
            name: Some(name),
            detached: false,
            head_sha: Some("demo".to_owned()),
        })
    }

    async fn switch_branch(&self, _session_id: Uuid, name: String) -> Result<GitCurrentBranch> {
        let mut branches = self.inner.branches.lock().await;
        if !branches.iter().any(|branch| branch.name == name) {
            return Err(anyhow!("unknown branch: {name}"));
        }
        for branch in branches.iter_mut() {
            branch.current = branch.name == name;
        }
        Ok(GitCurrentBranch {
            name: Some(name),
            detached: false,
            head_sha: Some("demo".to_owned()),
        })
    }

    async fn git_status(&self, _session_id: Uuid) -> Result<GitStatusSnapshot> {
        let branch = self.get_current_branch(_session_id).await?;
        let files = self.list_changed_files(_session_id).await?;
        Ok(GitStatusSnapshot {
            branch,
            dirty: !files.is_empty(),
            conflict_in_progress: false,
            files,
        })
    }

    async fn list_changed_files(&self, _session_id: Uuid) -> Result<Vec<GitChangedFile>> {
        Ok(vec![
            GitChangedFile {
                path: PathBuf::from("crates/impetus-tui/src/review.rs"),
                kind: GitChangeKind::Added,
                status_code: Some("A ".into()),
            },
            GitChangedFile {
                path: PathBuf::from("crates/impetus/src/tui.rs"),
                kind: GitChangeKind::Modified,
                status_code: Some(" M".into()),
            },
        ])
    }

    async fn get_diff(
        &self,
        _session_id: Uuid,
        base_ref: Option<String>,
    ) -> Result<GitDiffPayload> {
        Ok(GitDiffPayload {
            base_ref,
            path: None,
            patch: DEMO_REVIEW_PATCH.to_owned(),
            truncated: false,
            files_changed: 2,
            observation: None,
        })
    }

    async fn get_file_diff(
        &self,
        _session_id: Uuid,
        path: PathBuf,
        base_ref: Option<String>,
    ) -> Result<GitDiffPayload> {
        let patch = if path.ends_with("review.rs") {
            DEMO_REVIEW_FILE_A
        } else {
            DEMO_REVIEW_FILE_B
        };
        Ok(GitDiffPayload {
            base_ref,
            path: Some(path),
            patch: patch.to_owned(),
            truncated: false,
            files_changed: 1,
            observation: None,
        })
    }

    async fn pty_start(
        &self,
        _session_id: Uuid,
        command: String,
        _args: Vec<String>,
        _working_dir: Option<PathBuf>,
        cols: Option<u16>,
        rows: Option<u16>,
    ) -> Result<impetus_client::PtySessionView> {
        let pty_id = self.inner.pty_next_id.fetch_add(1, Ordering::Relaxed);
        let banner =
            format!("demo PTY #{pty_id} ({command})\r\nCtrl+] detaches back to Impetus TUI.\r\n");
        self.inner
            .pty_pending
            .lock()
            .await
            .insert(pty_id, banner.into_bytes());
        Ok(impetus_client::PtySessionView {
            pty_id,
            owner_session_id: _session_id,
            state: impetus_client::protocol::PtySessionState::Running { pid: 0 },
            command,
            cols: cols.unwrap_or(80),
            rows: rows.unwrap_or(24),
        })
    }

    async fn pty_input(&self, _session_id: Uuid, pty_id: u64, data: &[u8]) -> Result<()> {
        let mut pending = self.inner.pty_pending.lock().await;
        let Some(buf) = pending.get_mut(&pty_id) else {
            return Err(anyhow!("unknown demo pty {pty_id}"));
        };
        // Echo typed bytes so demo passthrough feels alive.
        buf.extend_from_slice(data);
        Ok(())
    }

    async fn pty_output(
        &self,
        _session_id: Uuid,
        pty_id: u64,
        max_bytes: Option<usize>,
    ) -> Result<impetus_client::PtyOutputView> {
        let mut pending = self.inner.pty_pending.lock().await;
        let Some(buf) = pending.get_mut(&pty_id) else {
            return Err(anyhow!("unknown demo pty {pty_id}"));
        };
        let take = max_bytes.unwrap_or(16 * 1024).min(buf.len());
        let data = buf.drain(..take).collect::<Vec<_>>();
        Ok(impetus_client::PtyOutputView {
            pty_id,
            data,
            dropped_total: 0,
            eof: false,
            spill_artifact: None,
        })
    }

    async fn pty_resize(
        &self,
        _session_id: Uuid,
        pty_id: u64,
        _cols: u16,
        _rows: u16,
    ) -> Result<()> {
        let pending = self.inner.pty_pending.lock().await;
        if !pending.contains_key(&pty_id) {
            return Err(anyhow!("unknown demo pty {pty_id}"));
        }
        Ok(())
    }

    async fn pty_detach(&self, _session_id: Uuid, pty_id: u64) -> Result<()> {
        self.inner.pty_pending.lock().await.remove(&pty_id);
        Ok(())
    }

    async fn subscribe(
        &self,
        session_id: Uuid,
        after_sequence: u64,
    ) -> Result<Box<dyn UiEventStream>> {
        let (tx, rx) = mpsc::channel(32);
        self.inner
            .subscribers
            .lock()
            .await
            .entry(session_id)
            .or_default()
            .push(tx.clone());
        if after_sequence == 0 {
            let _ = tx.send(self.seed_events()).await;
        }
        Ok(Box::new(MockEventStream { receiver: rx }))
    }
}

struct MockEventStream {
    receiver: mpsc::Receiver<Vec<UiEvent>>,
}

#[async_trait]
impl UiEventStream for MockEventStream {
    async fn next_batch(&mut self) -> Result<Vec<UiEvent>> {
        self.receiver
            .recv()
            .await
            .ok_or_else(|| anyhow!("demo event stream closed"))
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn demo_provider_catalog() -> Vec<ModelProviderStatus> {
    let mut mock_fast =
        ModelProviderStatus::basic("mock", "mock-fast", ModelProviderHealthLabel::Healthy, true);
    mock_fast.reasoning_efforts = vec!["low".into(), "medium".into(), "high".into()];
    mock_fast.default_reasoning_effort = Some("medium".into());
    mock_fast.service_tiers = vec!["flex".into(), "default".into()];
    mock_fast.provider_options = serde_json::json!({ "region": ["local", "ci"] });

    let mut mock_deep = ModelProviderStatus::basic(
        "mock",
        "mock-deep",
        ModelProviderHealthLabel::Healthy,
        false,
    );
    mock_deep.reasoning_efforts = vec!["medium".into(), "high".into()];
    mock_deep.default_reasoning_effort = Some("high".into());

    let mut offline = ModelProviderStatus::basic(
        "offline",
        "offline-model",
        ModelProviderHealthLabel::Unavailable {
            last_error_redacted: "provider unreachable".into(),
        },
        false,
    );
    offline.availability = ModelAvailability::Unavailable;

    let other = ModelProviderStatus::basic(
        "other",
        "other-base",
        ModelProviderHealthLabel::Unknown,
        false,
    );

    vec![mock_fast, mock_deep, offline, other]
}

fn demo_entry(name: &str, path: &str, is_dir: bool) -> WorkspaceDirEntry {
    WorkspaceDirEntry {
        name: name.to_owned(),
        path: path.to_owned(),
        is_dir,
        is_symlink: false,
        is_file: !is_dir,
    }
}

fn demo_list_workspace_dir(path: &Path) -> WorkspaceDirListing {
    let key = path.to_str().unwrap_or(".").trim().trim_end_matches('/');
    let key = if key.is_empty() { "." } else { key };
    let entries = match key {
        "." => vec![
            demo_entry("README.md", "README.md", false),
            demo_entry("src", "src", true),
            demo_entry("docs", "docs", true),
        ],
        "src" => vec![
            demo_entry("main.rs", "src/main.rs", false),
            demo_entry("lib.rs", "src/lib.rs", false),
        ],
        "docs" => vec![demo_entry("overview.md", "docs/overview.md", false)],
        _ => Vec::new(),
    };
    WorkspaceDirListing {
        path: key.to_owned(),
        entries,
    }
}

fn demo_read_workspace_file(path: &Path) -> Result<WorkspaceFileContent> {
    let key = path.to_str().unwrap_or("").replace('\\', "/");
    let content = match key.as_str() {
        "README.md" => "# Demo workspace\n\nFiles overlay talks to harness IPC only.\n",
        "src/main.rs" => "fn main() {\n    println!(\"demo\");\n}\n",
        "src/lib.rs" => "pub fn hello() -> &'static str {\n    \"demo\"\n}\n",
        "docs/overview.md" => "# Overview\n\nMock backend file preview.\n",
        other => {
            return Err(anyhow!("demo file not found: {other}"));
        }
    };
    Ok(WorkspaceFileContent {
        path: key,
        byte_count: content.len(),
        content: content.to_owned(),
    })
}

const DEMO_REVIEW_PATCH: &str = "\
diff --git a/crates/impetus-tui/src/review.rs b/crates/impetus-tui/src/review.rs
--- /dev/null
+++ b/crates/impetus-tui/src/review.rs
@@ -0,0 +1,3 @@
+//! Review helpers
+pub fn ok() {}
+
diff --git a/crates/impetus/src/tui.rs b/crates/impetus/src/tui.rs
--- a/crates/impetus/src/tui.rs
+++ b/crates/impetus/src/tui.rs
@@ -1,2 +1,2 @@
-pub async fn run() { old() }
+pub async fn run() { new() }
";

const DEMO_REVIEW_FILE_A: &str = "\
--- /dev/null
+++ b/crates/impetus-tui/src/review.rs
@@ -0,0 +1,3 @@
+//! Review helpers
+pub fn ok() {}
+
";

const DEMO_REVIEW_FILE_B: &str = "\
--- a/crates/impetus/src/tui.rs
+++ b/crates/impetus/src/tui.rs
@@ -1,2 +1,2 @@
-pub async fn run() { old() }
+pub async fn run() { new() }
";
