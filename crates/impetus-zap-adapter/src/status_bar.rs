//! Live session status bar for Zap adapter.
//!
//! Tracks harness state and renders status bar updates via OSC sequences.

use impetus_core::{BackendEvent, Event, EventPayload, RuntimeStatus};
use std::sync::{Arc, Mutex};

/// Session status aggregator
#[derive(Debug, Clone)]
pub struct StatusBar {
    state: Arc<Mutex<StatusBarState>>,
}

#[derive(Debug, Clone)]
struct StatusBarState {
    runtime_status: RuntimeStatus,
    current_action: Option<String>,
    pending_approvals: usize,
    last_error: Option<String>,
}

impl StatusBar {
    pub fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(StatusBarState {
                runtime_status: RuntimeStatus::Idle,
                current_action: None,
                pending_approvals: 0,
                last_error: None,
            })),
        }
    }

    /// Update state from harness event
    pub fn update_from_event(&self, event: &Event) {
        let mut state = self.state.lock().unwrap();

        match &event.payload {
            EventPayload::Run(run_event) => {
                use impetus_core::RunEvent;
                match run_event {
                    RunEvent::Started { .. } => {
                        state.runtime_status = RuntimeStatus::Running;
                        state.current_action = Some("Running".to_string());
                    }
                    RunEvent::Completed { .. } => {
                        state.runtime_status = RuntimeStatus::Completed;
                        state.current_action = None;
                    }
                    RunEvent::Failed { reason, .. } => {
                        state.runtime_status = RuntimeStatus::Failed;
                        state.last_error = Some(reason.clone());
                    }
                    RunEvent::Cancelled { .. } => {
                        state.runtime_status = RuntimeStatus::Idle;
                        state.current_action = None;
                    }
                    RunEvent::InterruptedUnknown { .. } => {
                        state.runtime_status = RuntimeStatus::Failed;
                        state.last_error = Some("Interrupted".to_string());
                    }
                }
            }
            EventPayload::Tool(tool_event) => {
                use impetus_core::ToolEvent;
                match tool_event {
                    ToolEvent::Started { name } => {
                        state.current_action = Some(format!("Tool: {}", name));
                    }
                    ToolEvent::Finished { .. } => {
                        state.current_action = None;
                    }
                    ToolEvent::Observed { tool_name, .. } => {
                        state.current_action = Some(format!("Tool: {tool_name}"));
                    }
                    ToolEvent::Deferred { tool_name, .. } => {
                        state.current_action = Some(format!("Approval required: {tool_name}"));
                    }
                }
            }
            EventPayload::Approval(approval_event) => {
                use impetus_core::ApprovalEvent;
                match approval_event {
                    ApprovalEvent::Requested { .. } => {
                        state.pending_approvals += 1;
                        state.runtime_status = RuntimeStatus::AwaitingApproval;
                    }
                    ApprovalEvent::Resolved { .. } => {
                        state.pending_approvals = state.pending_approvals.saturating_sub(1);
                        if state.pending_approvals == 0 {
                            state.runtime_status = RuntimeStatus::Running;
                        }
                    }
                }
            }
            EventPayload::Backend(BackendEvent::ProviderUnavailable { reason, .. })
            | EventPayload::Backend(BackendEvent::KeychainUnavailable { reason }) => {
                state.last_error = Some(reason.clone());
            }
            _ => {}
        }
    }

    /// Render current status as string
    pub fn render(&self) -> String {
        let state = self.state.lock().unwrap();
        let status_symbol = match state.runtime_status {
            RuntimeStatus::Idle => "○",
            RuntimeStatus::Running => "●",
            RuntimeStatus::AwaitingApproval => "⏸",
            RuntimeStatus::Completed => "✓",
            RuntimeStatus::Failed => "✗",
            RuntimeStatus::Cancelled => "⊗",
            RuntimeStatus::InterruptedUnknown => "?",
        };

        let mut parts = vec![status_symbol.to_string()];

        if let Some(action) = &state.current_action {
            parts.push(action.clone());
        }

        if state.pending_approvals > 0 {
            parts.push(format!("({} pending)", state.pending_approvals));
        }

        if let Some(error) = &state.last_error {
            parts.push(format!("Error: {}", error));
        }

        parts.join(" ")
    }

    /// Send status bar update via OSC sequence
    pub fn send_update(&self) {
        let status = self.render();
        crate::osc::send_state(&status, None);
    }
}

impl Default for StatusBar {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use impetus_core::{ApprovalEvent, ApprovalRequest, RunEvent};
    use uuid::Uuid;

    fn make_event(payload: EventPayload) -> Event {
        Event::new(Uuid::new_v4(), 1, payload)
    }

    #[test]
    fn status_bar_tracks_run_lifecycle() {
        let bar = StatusBar::new();
        assert!(bar.render().contains("○")); // Idle

        bar.update_from_event(&make_event(EventPayload::Run(RunEvent::Started {
            run_id: Uuid::new_v4(),
        })));
        assert!(bar.render().contains("●")); // Running

        bar.update_from_event(&make_event(EventPayload::Run(RunEvent::Completed {
            run_id: Uuid::new_v4(),
        })));
        assert!(bar.render().contains("✓")); // Completed
    }

    #[test]
    fn status_bar_tracks_approvals() {
        let bar = StatusBar::new();

        let approval = ApprovalRequest::pending(
            impetus_core::Action {
                origin: impetus_core::ActionOrigin::Agent,
                kind: impetus_core::ActionKind::ReadFile,
                summary: "Read config file".to_string(),
                target: Some("config.toml".to_string()),
            },
            "Need to read config".to_string(),
            1, // intent_revision
        );

        bar.update_from_event(&make_event(EventPayload::Approval(
            ApprovalEvent::Requested {
                request: approval.clone(),
            },
        )));
        let status = bar.render();
        assert!(status.contains("⏸")); // AwaitingApproval
        assert!(status.contains("(1 pending)"));

        bar.update_from_event(&make_event(EventPayload::Approval(
            ApprovalEvent::Resolved { request: approval },
        )));
        let status = bar.render();
        assert!(!status.contains("pending"));
    }

    #[test]
    fn status_bar_shows_current_action() {
        let bar = StatusBar::new();

        bar.update_from_event(&make_event(EventPayload::Tool(
            impetus_core::ToolEvent::Started {
                name: "read_file".to_string(),
            },
        )));
        assert!(bar.render().contains("Tool: read_file"));
    }
}
