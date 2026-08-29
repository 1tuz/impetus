use crate::events::Event;
use anyhow::Result;
use async_trait::async_trait;

/// Service contract for agent loop strategy
#[async_trait]
pub trait AgentLoopStrategy: Send + Sync {
    /// Execute one turn of the agent loop
    async fn execute_turn(&self, session_id: uuid::Uuid, prompt: String) -> Result<Vec<Event>>;

    /// Get strategy name
    fn name(&self) -> &str;

    /// Get strategy version
    fn version(&self) -> &str;

    /// Check if strategy supports a capability
    fn supports_capability(&self, capability: &str) -> bool;
}

/// Service contract for agent scheduler
#[async_trait]
pub trait AgentScheduler: Send + Sync {
    /// Schedule a task for execution
    async fn schedule(&self, task: ScheduledTask) -> Result<String>;

    /// Cancel a scheduled task
    async fn cancel(&self, task_id: &str) -> Result<()>;

    /// Get scheduler name
    fn name(&self) -> &str;

    /// Get scheduler version
    fn version(&self) -> &str;
}

/// Scheduled task definition
#[derive(Debug, Clone)]
pub struct ScheduledTask {
    pub session_id: uuid::Uuid,
    pub prompt: String,
    pub schedule: TaskSchedule,
}

/// Task scheduling specification
#[derive(Debug, Clone)]
pub enum TaskSchedule {
    Immediate,
    Delayed { seconds: u64 },
    Cron { expression: String },
}

/// Service registry for runtime components
pub struct ServiceRegistry {
    agent_loop: Option<Box<dyn AgentLoopStrategy>>,
    scheduler: Option<Box<dyn AgentScheduler>>,
}

impl ServiceRegistry {
    pub fn new() -> Self {
        Self {
            agent_loop: None,
            scheduler: None,
        }
    }

    /// Register an agent loop strategy
    pub fn register_agent_loop(&mut self, strategy: Box<dyn AgentLoopStrategy>) {
        self.agent_loop = Some(strategy);
    }

    /// Register a scheduler
    pub fn register_scheduler(&mut self, scheduler: Box<dyn AgentScheduler>) {
        self.scheduler = Some(scheduler);
    }

    /// Get registered agent loop strategy
    pub fn agent_loop(&self) -> Option<&dyn AgentLoopStrategy> {
        self.agent_loop.as_ref().map(|s| s.as_ref())
    }

    /// Get registered scheduler
    pub fn scheduler(&self) -> Option<&dyn AgentScheduler> {
        self.scheduler.as_ref().map(|s| s.as_ref())
    }
}

impl Default for ServiceRegistry {
    fn default() -> Self {
        Self::new()
    }
}
