use crate::{AgentRuntime, RunEvent, RuntimeError, RuntimeStatus};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU8, Ordering},
};
use std::time::Duration;
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Clone)]
pub enum MockStreamItem {
    Chunk { chunk_id: u64, text: String },
    Delay(Duration),
    Complete,
    Fail { reason: String },
    Disconnect,
}

#[derive(Debug, Clone, Default)]
pub struct MockStreamingProvider {
    items: Vec<MockStreamItem>,
}

impl MockStreamingProvider {
    pub fn new(items: impl IntoIterator<Item = MockStreamItem>) -> Self {
        Self {
            items: items.into_iter().collect(),
        }
    }
}

#[derive(Debug, Error)]
pub enum SupervisorError {
    #[error(transparent)]
    Runtime(#[from] RuntimeError),
    #[error("mock provider disconnected")]
    ProviderDisconnected,
}

#[derive(Clone)]
pub struct SessionSupervisor {
    runtime: Arc<AgentRuntime>,
    soft_interrupt: Arc<AtomicBool>,
    hard_cancel: Arc<AtomicBool>,
    max_restart_attempts: u8,
    restart_attempts: Arc<AtomicU8>,
}

impl SessionSupervisor {
    pub fn new(runtime: Arc<AgentRuntime>) -> Self {
        Self::with_restart_limit(runtime, 1)
    }

    pub fn with_restart_limit(runtime: Arc<AgentRuntime>, max_restart_attempts: u8) -> Self {
        Self {
            runtime,
            soft_interrupt: Arc::new(AtomicBool::new(false)),
            hard_cancel: Arc::new(AtomicBool::new(false)),
            max_restart_attempts,
            restart_attempts: Arc::new(AtomicU8::new(0)),
        }
    }

    pub fn request_soft_interrupt(&self) {
        self.soft_interrupt.store(true, Ordering::Release);
    }

    pub fn request_hard_cancel(&self) {
        self.hard_cancel.store(true, Ordering::Release);
    }

    pub async fn start_mock(
        &self,
        provider: &MockStreamingProvider,
    ) -> Result<Uuid, SupervisorError> {
        self.soft_interrupt.store(false, Ordering::Release);
        self.hard_cancel.store(false, Ordering::Release);
        self.restart_attempts.store(0, Ordering::Release);
        let run_id = self.runtime.start_run()?;
        self.resume_mock(run_id, provider).await?;
        Ok(run_id)
    }

    pub async fn resume_mock(
        &self,
        run_id: Uuid,
        provider: &MockStreamingProvider,
    ) -> Result<RuntimeStatus, SupervisorError> {
        if self.hard_cancel.load(Ordering::Acquire) || self.soft_interrupt.load(Ordering::Acquire) {
            self.runtime.finish_run(RunEvent::Cancelled { run_id })?;
            return Ok(RuntimeStatus::Cancelled);
        }
        for item in &provider.items {
            if self.hard_cancel.load(Ordering::Acquire)
                || self.soft_interrupt.load(Ordering::Acquire)
            {
                self.runtime.finish_run(RunEvent::Cancelled { run_id })?;
                return Ok(RuntimeStatus::Cancelled);
            }
            match item {
                MockStreamItem::Chunk { chunk_id, text } => {
                    self.runtime.record_agent_chunk(run_id, *chunk_id, text)?;
                }
                MockStreamItem::Delay(duration) => tokio::time::sleep(*duration).await,
                MockStreamItem::Complete => {
                    self.runtime.finish_run(RunEvent::Completed { run_id })?;
                    return Ok(RuntimeStatus::Completed);
                }
                MockStreamItem::Fail { reason } => {
                    self.runtime.finish_run(RunEvent::Failed {
                        run_id,
                        reason: reason.clone(),
                    })?;
                    return Ok(RuntimeStatus::Failed);
                }
                MockStreamItem::Disconnect => {
                    let attempt = self.restart_attempts.fetch_add(1, Ordering::AcqRel) + 1;
                    if attempt > self.max_restart_attempts {
                        self.runtime.finish_run(RunEvent::Failed {
                            run_id,
                            reason: "provider restart limit exhausted".into(),
                        })?;
                        return Ok(RuntimeStatus::Failed);
                    }
                    return Err(SupervisorError::ProviderDisconnected);
                }
            }
        }
        Ok(self.runtime.status()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{MemoryEventStore, PolicyEngine, SandboxScope};

    #[tokio::test]
    async fn restart_skips_already_durable_chunks() {
        let runtime = Arc::new(AgentRuntime::new(
            Arc::new(MemoryEventStore::default()),
            PolicyEngine::new(SandboxScope::local_workspace(".")),
        ));
        let supervisor = SessionSupervisor::new(runtime.clone());
        let first = MockStreamingProvider::new([
            MockStreamItem::Chunk {
                chunk_id: 1,
                text: "one".into(),
            },
            MockStreamItem::Disconnect,
        ]);
        let run_id = runtime.start_run().expect("start run");
        assert!(matches!(
            supervisor.resume_mock(run_id, &first).await,
            Err(SupervisorError::ProviderDisconnected)
        ));
        let restarted = MockStreamingProvider::new([
            MockStreamItem::Chunk {
                chunk_id: 1,
                text: "one".into(),
            },
            MockStreamItem::Chunk {
                chunk_id: 2,
                text: " two".into(),
            },
            MockStreamItem::Complete,
        ]);
        assert_eq!(
            supervisor
                .resume_mock(run_id, &restarted)
                .await
                .expect("resume"),
            RuntimeStatus::Completed
        );
        assert_eq!(
            runtime
                .events()
                .expect("events")
                .iter()
                .filter(|event| matches!(
                    event.payload,
                    crate::EventPayload::Agent(crate::AgentEvent::Chunk { .. })
                ))
                .count(),
            2
        );
        assert_eq!(
            runtime
                .events()
                .expect("events")
                .iter()
                .filter_map(|event| match &event.payload {
                    crate::EventPayload::Agent(crate::AgentEvent::Chunk { text, .. }) => {
                        Some(text.as_str())
                    }
                    _ => None,
                })
                .collect::<String>(),
            "one two"
        );
    }

    #[tokio::test]
    async fn soft_interrupt_cancels_at_next_safe_boundary() {
        let runtime = Arc::new(AgentRuntime::new(
            Arc::new(MemoryEventStore::default()),
            PolicyEngine::new(SandboxScope::local_workspace(".")),
        ));
        let supervisor = SessionSupervisor::new(runtime);
        let worker = supervisor.clone();
        let provider = MockStreamingProvider::new([
            MockStreamItem::Delay(Duration::from_millis(20)),
            MockStreamItem::Chunk {
                chunk_id: 1,
                text: "never emitted".into(),
            },
        ]);
        let task = tokio::spawn(async move { worker.start_mock(&provider).await });
        tokio::time::sleep(Duration::from_millis(1)).await;
        supervisor.request_soft_interrupt();
        task.await.expect("supervisor task").expect("cancel");
        assert_eq!(
            supervisor.runtime.status().expect("cancelled status"),
            RuntimeStatus::Cancelled
        );
    }

    #[tokio::test]
    async fn provider_failure_is_durable() {
        let runtime = Arc::new(AgentRuntime::new(
            Arc::new(MemoryEventStore::default()),
            PolicyEngine::new(SandboxScope::local_workspace(".")),
        ));
        let supervisor = SessionSupervisor::new(runtime.clone());
        let provider = MockStreamingProvider::new([MockStreamItem::Fail {
            reason: "upstream unavailable".into(),
        }]);
        supervisor.start_mock(&provider).await.expect("run failure");
        assert_eq!(
            runtime.status().expect("failed status"),
            RuntimeStatus::Failed
        );
    }

    #[tokio::test]
    async fn hard_cancel_ends_active_run() {
        let runtime = Arc::new(AgentRuntime::new(
            Arc::new(MemoryEventStore::default()),
            PolicyEngine::new(SandboxScope::local_workspace(".")),
        ));
        let supervisor = SessionSupervisor::new(runtime.clone());
        let run_id = runtime.start_run().expect("start run");
        supervisor.request_hard_cancel();
        assert_eq!(
            supervisor
                .resume_mock(run_id, &MockStreamingProvider::default())
                .await
                .expect("hard cancel"),
            RuntimeStatus::Cancelled
        );
        assert_eq!(
            runtime.status().expect("cancelled status"),
            RuntimeStatus::Cancelled
        );
    }

    #[tokio::test]
    async fn provider_restart_limit_ends_run_as_failed() {
        let runtime = Arc::new(AgentRuntime::new(
            Arc::new(MemoryEventStore::default()),
            PolicyEngine::new(SandboxScope::local_workspace(".")),
        ));
        let supervisor = SessionSupervisor::with_restart_limit(runtime.clone(), 1);
        let run_id = runtime.start_run().expect("start run");
        let provider = MockStreamingProvider::new([MockStreamItem::Disconnect]);
        assert!(matches!(
            supervisor.resume_mock(run_id, &provider).await,
            Err(SupervisorError::ProviderDisconnected)
        ));
        assert_eq!(
            supervisor
                .resume_mock(run_id, &provider)
                .await
                .expect("restart limit"),
            RuntimeStatus::Failed
        );
        assert_eq!(
            runtime.status().expect("failed status"),
            RuntimeStatus::Failed
        );
    }
}
