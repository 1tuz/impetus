use impetus_client::protocol::SessionInfo;
use std::collections::{BTreeSet, VecDeque};
use std::time::{Duration, Instant};
use uuid::Uuid;

use crate::composer::Composer;

pub const MAX_TIMELINE_ITEMS: usize = 1_000;
pub const MAX_BODY_CHARS: usize = 40_000;
pub const LARGE_PASTE_BYTES: usize = 8 * 1024;
pub const MAX_DIRECT_PROMPT_BYTES: usize = 48 * 1024;

#[derive(Clone, Debug)]
pub struct RunOptions {
    pub demo: bool,
    pub inline: bool,
    pub inline_rows: u16,
    pub mouse: bool,
    pub tick_rate: Duration,
}

impl RunOptions {
    pub fn from_env() -> Self {
        Self {
            demo: env_flag("IMPETUS_TUI_DEMO"),
            inline: env_flag("IMPETUS_TUI_INLINE"),
            inline_rows: env_u16("IMPETUS_TUI_INLINE_ROWS", 24).clamp(14, 80),
            mouse: !env_flag("IMPETUS_TUI_NO_MOUSE"),
            tick_rate: Duration::from_millis(33),
        }
    }
}

fn env_flag(name: &str) -> bool {
    std::env::var(name)
        .map(|value| matches!(value.as_str(), "1" | "true" | "yes" | "on"))
        .unwrap_or(false)
}

fn env_u16(name: &str, default: u16) -> u16 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse::<u16>().ok())
        .unwrap_or(default)
}

#[derive(Clone, Debug, Default)]
pub struct ConnectionInfo {
    pub protocol_version: u16,
    pub capabilities: BTreeSet<String>,
    pub label: String,
}

#[derive(Clone, Debug)]
pub struct SessionSummary {
    pub id: Uuid,
    pub label: String,
    pub status: String,
    pub workspace: Option<String>,
}

impl SessionSummary {
    pub fn from_branch(session: SessionInfo) -> Self {
        let label = session
            .branch_name
            .unwrap_or_else(|| format!("session {}", short_id(session.id)));
        let status = match (session.parent_session_id, session.fork_sequence) {
            (Some(parent), Some(sequence)) => {
                format!("branch of {} at {sequence}", short_id(parent))
            }
            _ => "saved".to_owned(),
        };
        Self {
            id: session.id,
            label,
            status,
            workspace: None,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ExecutionMode {
    Plan,
    #[default]
    Ask,
    AutoSafe,
    AcceptEdits,
    FullAuto,
}

impl ExecutionMode {
    pub const ALL: [Self; 5] = [
        Self::Plan,
        Self::Ask,
        Self::AutoSafe,
        Self::AcceptEdits,
        Self::FullAuto,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Plan => "PLAN",
            Self::Ask => "ASK",
            Self::AutoSafe => "AUTO-SAFE",
            Self::AcceptEdits => "ACCEPT EDITS",
            Self::FullAuto => "FULL AUTO",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Self::Plan => "Research and plan only; do not request mutating tools.",
            Self::Ask => "Proceed normally and show every daemon-requested approval.",
            Self::AutoSafe => {
                "Run policy-allowed read-only work; mutations still require approval."
            }
            Self::AcceptEdits => "Future scoped grant for file edits; requires daemon capability.",
            Self::FullAuto => "Future broad grant; disabled until the daemon owns a durable scope.",
        }
    }

    pub fn is_available(self, capabilities: &BTreeSet<String>) -> bool {
        match self {
            Self::Plan | Self::Ask | Self::AutoSafe => true,
            Self::AcceptEdits => capabilities.contains("approval_scope_file_edits"),
            Self::FullAuto => capabilities.contains("approval_scope_full_auto"),
        }
    }

    pub fn prompt_prefix(self) -> Option<&'static str> {
        match self {
            Self::Plan => Some(
                "[Impetus UI mode: PLAN. Produce a concrete plan and inspect safely. Do not request mutating tools until the user switches mode.]\n\n",
            ),
            Self::AutoSafe => Some(
                "[Impetus UI mode: AUTO-SAFE. Proceed autonomously with read-only or daemon-policy-allowed actions. Mutating, network, process and other approval-gated actions still require explicit daemon approval.]\n\n",
            ),
            Self::Ask | Self::AcceptEdits | Self::FullAuto => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunState {
    Idle,
    Working,
    WaitingApproval,
    Cancelling,
    Failed,
    Unknown,
}

impl RunState {
    pub fn label(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Working => "working",
            Self::WaitingApproval => "approval",
            Self::Cancelling => "cancelling",
            Self::Failed => "failed",
            Self::Unknown => "unknown outcome",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Focus {
    Timeline,
    Composer,
    Inspector,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ItemKind {
    User,
    Assistant,
    Plan,
    Tool,
    Approval,
    Notice,
    Error,
    Budget,
}

#[derive(Clone, Debug)]
pub struct TimelineItem {
    pub sequence: u64,
    pub at_unix_ms: u64,
    pub kind: ItemKind,
    pub title: String,
    pub body: String,
    pub details: String,
    pub collapsed: bool,
    pub streaming_key: Option<String>,
}

impl TimelineItem {
    pub fn new(sequence: u64, at_unix_ms: u64, kind: ItemKind, title: impl Into<String>) -> Self {
        Self {
            sequence,
            at_unix_ms,
            kind,
            title: title.into(),
            body: String::new(),
            details: String::new(),
            collapsed: false,
            streaming_key: None,
        }
    }

    pub fn with_body(mut self, body: impl Into<String>) -> Self {
        self.body = bounded(body.into(), MAX_BODY_CHARS);
        self
    }

    pub fn with_details(mut self, details: impl Into<String>) -> Self {
        self.details = bounded(details.into(), MAX_BODY_CHARS);
        self
    }

    pub fn collapsed(mut self) -> Self {
        self.collapsed = true;
        self
    }
}

#[derive(Clone, Debug)]
pub struct ApprovalCard {
    pub id: Uuid,
    pub action_kind: String,
    pub summary: String,
    pub target: Option<String>,
    pub reason: String,
    pub fingerprint: String,
    pub detail: Option<ApprovalDetailView>,
}

#[derive(Clone, Debug, Default)]
pub struct ApprovalDetailView {
    pub diff_preview: Option<String>,
    pub affected_files: Vec<String>,
    pub estimated_scope: Option<String>,
    pub attachment_refs: Vec<Uuid>,
}

#[derive(Clone, Debug, Default)]
pub struct BudgetState {
    pub turns_used: u32,
    pub tokens_used: u64,
    pub context_used_percent: u8,
    pub compactions: u32,
    pub warning: Option<String>,
}

#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug)]
pub enum UiEventKind {
    SessionCreated,
    SessionWorkspace {
        workspace: String,
    },
    SessionAttached,
    UserInput {
        text: String,
    },
    Plan {
        summary: String,
    },
    RunStarted {
        run_id: Uuid,
    },
    RunCompleted {
        run_id: Uuid,
    },
    RunFailed {
        run_id: Uuid,
        reason: String,
    },
    RunCancelled {
        run_id: Uuid,
    },
    RunUnknown {
        run_id: Uuid,
    },
    AgentChunk {
        run_id: Uuid,
        chunk_id: u64,
        text: String,
    },
    AgentFinal {
        run_id: Uuid,
        text: String,
    },
    ToolStarted {
        name: String,
    },
    ToolFinished {
        name: String,
        summary: String,
    },
    ToolObserved {
        call_id: String,
        name: String,
        arguments: String,
        outcome: String,
        preview: String,
        artifact: Option<String>,
        error: Option<String>,
    },
    ToolDeferred {
        approval_id: Uuid,
        call_id: String,
        name: String,
        arguments: String,
    },
    ApprovalRequested {
        approval: ApprovalCard,
    },
    ApprovalResolved {
        approval_id: Uuid,
        accepted: bool,
    },
    Backend {
        title: String,
        detail: String,
        healthy: bool,
    },
    BudgetUpdated(BudgetState),
    BudgetWarning {
        message: String,
    },
    Notice {
        title: String,
        message: String,
        error: bool,
    },
    Retry {
        title: String,
        message: String,
        failed: bool,
    },
}

#[derive(Clone, Debug)]
pub struct UiEvent {
    pub sequence: u64,
    pub at_unix_ms: u64,
    pub kind: UiEventKind,
}

#[derive(Clone, Debug, Default)]
pub enum Overlay {
    #[default]
    None,
    Help,
    Sessions {
        selected: usize,
        query: String,
    },
    Commands {
        selected: usize,
        query: String,
    },
    Modes {
        selected: usize,
    },
    Approval {
        selected: usize,
    },
    ApprovalDetail,
    LargePaste,
    Diagnostics {
        text: String,
    },
    Message {
        title: String,
        body: String,
        error: bool,
    },
}

#[derive(Clone, Debug)]
pub struct Toast {
    pub text: String,
    pub error: bool,
    pub expires_at: Instant,
}

#[derive(Debug)]
pub struct AppState {
    pub connection: ConnectionInfo,
    pub sessions: Vec<SessionSummary>,
    pub active_session: Option<Uuid>,
    pub timeline: VecDeque<TimelineItem>,
    pub selected_item: Option<usize>,
    pub line_scroll_from_bottom: usize,
    pub follow_tail: bool,
    pub composer: Composer,
    pub focus: Focus,
    pub mode: ExecutionMode,
    pub run_state: RunState,
    pub budget: BudgetState,
    pub overlay: Overlay,
    pub approval_queue: VecDeque<ApprovalCard>,
    pub show_sessions: bool,
    pub show_inspector: bool,
    pub should_quit: bool,
    pub dirty: bool,
    pub last_sequence: u64,
    pub pending_large_paste: Option<String>,
    pub toast: Option<Toast>,
    pub status_message: String,
    pub subscription_generation: u64,
}

impl AppState {
    pub fn new(connection: ConnectionInfo) -> Self {
        Self {
            connection,
            sessions: Vec::new(),
            active_session: None,
            timeline: VecDeque::new(),
            selected_item: None,
            line_scroll_from_bottom: 0,
            follow_tail: true,
            composer: Composer::default(),
            focus: Focus::Composer,
            mode: ExecutionMode::Ask,
            run_state: RunState::Idle,
            budget: BudgetState::default(),
            overlay: Overlay::None,
            approval_queue: VecDeque::new(),
            show_sessions: true,
            show_inspector: true,
            should_quit: false,
            dirty: true,
            last_sequence: 0,
            pending_large_paste: None,
            toast: None,
            status_message: "ready".to_owned(),
            subscription_generation: 0,
        }
    }

    pub fn active_session_label(&self) -> String {
        self.active_session
            .map(short_id)
            .unwrap_or_else(|| "no-session".to_owned())
    }

    pub fn push_item(&mut self, item: TimelineItem) {
        if self.timeline.len() >= MAX_TIMELINE_ITEMS {
            let _ = self.timeline.pop_front();
            self.selected_item = self.selected_item.map(|index| index.saturating_sub(1));
        }
        self.last_sequence = self.last_sequence.max(item.sequence);
        self.timeline.push_back(item);
        if self.follow_tail {
            self.line_scroll_from_bottom = 0;
            self.selected_item = self.timeline.len().checked_sub(1);
        }
        self.dirty = true;
    }

    pub fn show_toast(&mut self, text: impl Into<String>, error: bool) {
        self.toast = Some(Toast {
            text: text.into(),
            error,
            expires_at: Instant::now() + Duration::from_secs(if error { 7 } else { 4 }),
        });
        self.dirty = true;
    }

    pub fn expire_transients(&mut self) {
        if self
            .toast
            .as_ref()
            .is_some_and(|toast| toast.expires_at <= Instant::now())
        {
            self.toast = None;
            self.dirty = true;
        }
    }
}

pub fn short_id(id: Uuid) -> String {
    id.to_string().chars().take(8).collect()
}

pub fn bounded(value: String, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value;
    }
    let mut output: String = value.chars().take(max_chars).collect();
    output.push_str(
        "\n… output truncated in TUI; use the attached artifact/raw view for full content",
    );
    output
}
