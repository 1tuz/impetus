use anyhow::{Result, anyhow};
use async_trait::async_trait;
use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::{Mutex, mpsc};
use uuid::Uuid;

use super::{UiBackend, UiEventStream};
use crate::model::{
    ApprovalCard, ApprovalDetailView, BudgetState, ConnectionInfo, SessionSummary, UiEvent,
    UiEventKind,
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
    subscribers: Mutex<HashMap<Uuid, Vec<mpsc::Sender<Vec<UiEvent>>>>>,
    approval_details: Mutex<HashMap<Uuid, ApprovalDetailView>>,
    sequence: AtomicU64,
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
                subscribers: Mutex::new(HashMap::new()),
                approval_details: Mutex::new(HashMap::new()),
                sequence: AtomicU64::new(1),
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
}

#[async_trait]
impl UiBackend for MockBackend {
    async fn connection_info(&self) -> Result<ConnectionInfo> {
        Ok(ConnectionInfo {
            protocol_version: 3,
            capabilities: [
                "session_create",
                "session_attach",
                "session_list",
                "prompt",
                "cancel",
                "subscribe",
                "resolve_approval",
                "get_approval_detail",
                "diagnostics",
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

    async fn send_message(&self, session_id: Uuid, text: String) -> Result<String> {
        let backend = self.clone();
        spawn_detached(async move {
            let run_id = Uuid::new_v4();
            backend
                .publish(
                    session_id,
                    vec![
                        backend.next_event(UiEventKind::UserInput { text: text.clone() }),
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
                        affected_files: vec!["crates/impetus/src/tui.rs".to_owned()],
                        estimated_scope: Some("Lines(2)".to_owned()),
                        attachment_refs: vec![],
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
