use std::collections::{BTreeSet, VecDeque};
use std::time::{Duration, Instant};
use uuid::Uuid;

use crate::composer::Composer;
use crate::hit::{HitTarget, PointerClick};
use crate::stream_buffer::StreamBuffer;

pub const MAX_TIMELINE_ITEMS: usize = 1_000;
pub const MAX_BODY_CHARS: usize = 40_000;
/// Pastes above this size use compact composer placeholder + upload path.
pub const LARGE_PASTE_BYTES: usize = 8 * 1024;
/// Hard cap aligned with daemon `MAX_ARTIFACT_UPLOAD_BYTES` (8 MiB).
pub const MAX_PASTE_UPLOAD_BYTES: usize = 8 * 1024 * 1024;

/// Normalize terminal paste newlines (CRLF/CR → LF).
pub fn normalize_paste(text: &str) -> String {
    if !text.as_bytes().contains(&b'\r') {
        return text.to_owned();
    }
    text.replace("\r\n", "\n").replace('\r', "\n")
}

/// Line count for paste stats (counts a trailing newline as its own line boundary).
pub fn paste_line_count(text: &str) -> usize {
    if text.is_empty() {
        0
    } else {
        text.bytes().filter(|&b| b == b'\n').count() + 1
    }
}

/// Compact composer placeholder for oversized paste.
pub fn format_paste_placeholder(bytes: usize, lines: usize) -> String {
    let kb = bytes.saturating_add(1023) / 1024;
    format!("[Pasted text · {kb} KB · {lines} lines]")
}

pub fn is_paste_placeholder(text: &str) -> bool {
    let trimmed = text.trim();
    trimmed.starts_with("[Pasted text · ") && trimmed.ends_with(']')
}

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
    /// Map durable `SessionInfo` branch metadata into picker rows.
    ///
    /// Harness `SessionInfo` today carries id / timestamps / parent / fork only.
    /// Optional `label` / `status` / `workspace` override when a richer list DTO
    /// is available; otherwise parent/fork drive label+status and workspace stays
    /// unset.
    pub fn from_session_info(
        id: Uuid,
        parent_session_id: Option<Uuid>,
        fork_sequence: Option<u64>,
        label: Option<String>,
        status: Option<String>,
        workspace: Option<String>,
    ) -> Self {
        let derived_label = match (parent_session_id, fork_sequence) {
            (Some(parent), Some(seq)) => format!("fork@{seq} ← {}", short_id(parent)),
            (Some(parent), None) => format!("fork ← {}", short_id(parent)),
            _ => format!("session {}", short_id(id)),
        };
        let derived_status = if parent_session_id.is_some() {
            "fork"
        } else {
            "saved"
        };
        Self {
            id,
            label: label
                .filter(|value| !value.trim().is_empty())
                .unwrap_or(derived_label),
            status: status
                .filter(|value| !value.trim().is_empty())
                .unwrap_or_else(|| derived_status.to_owned()),
            workspace: workspace.filter(|value| !value.trim().is_empty()),
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
        /// Optional harness/doctor remediation; TUI falls back to a static hint.
        remediation: Option<String>,
    },
    Retry {
        title: String,
        message: String,
        failed: bool,
    },
}

/// Default doctor-oriented hint when harness omits remediation.
pub const DEFAULT_ERROR_REMEDIATION: &str =
    "Run `impetus doctor` for subsystem probes and remediation hints.";

/// Prefer explicit remediation from the event; else a short static hint by title.
pub fn remediation_hint(title: &str, explicit: Option<&str>) -> String {
    if let Some(hint) = explicit.map(str::trim).filter(|value| !value.is_empty()) {
        return hint.to_owned();
    }
    let lower = title.to_ascii_lowercase();
    if lower.contains("policy") {
        "Check policy mode and pending approvals; run `impetus doctor`.".to_owned()
    } else if lower.contains("unknown") {
        "Do not retry non-replayable work; reattach the session and check run status.".to_owned()
    } else if lower.contains("reconnect")
        || lower.contains("backend")
        || lower.contains("ipc")
        || lower.contains("provider")
        || lower.contains("keychain")
    {
        "Ensure `impetusd` is running and the IPC socket is reachable.".to_owned()
    } else if lower.contains("retry") || lower.contains("exhausted") {
        "Inspect the last error; retry only if the work is replayable.".to_owned()
    } else if lower.contains("fail") || lower.contains("error") {
        "Inspect the event details; retry only if the work is replayable.".to_owned()
    } else {
        DEFAULT_ERROR_REMEDIATION.to_owned()
    }
}

/// Connection + run + optional budget figures already present on `AppState`.
pub fn format_status_strip(app: &AppState) -> String {
    let conn = if app.connection.label.is_empty() {
        format!("ipc v{}", app.connection.protocol_version)
    } else {
        format!(
            "{} · ipc v{}",
            app.connection.label, app.connection.protocol_version
        )
    };
    let mut parts = vec![
        conn,
        format!("run {}", app.run_state.label()),
        format!("{} tok", compact_status_number(app.budget.tokens_used)),
        format!("ctx {}%", app.budget.context_used_percent),
        format!("{} turn", app.budget.turns_used),
    ];
    if app.budget.compactions > 0 {
        parts.push(format!("{} compact", app.budget.compactions));
    }
    if app.budget.warning.is_some() {
        parts.push("budget!".to_owned());
    }
    parts.push(app.theme_id.clone());
    parts.join(" · ")
}

fn compact_status_number(value: u64) -> String {
    match value {
        0..=999 => value.to_string(),
        1_000..=999_999 => format!("{}k", value / 1_000),
        _ => format!("{}m", value / 1_000_000),
    }
}

/// Max scroll offset from bottom so the viewport stays filled when possible.
pub fn max_scroll_from_bottom(total_lines: usize, viewport_rows: usize) -> usize {
    total_lines.saturating_sub(viewport_rows.max(1))
}

/// Tick-gated redraw: high-frequency dirty flags coalesce into one paint per tick.
pub fn should_coalesce_redraw(dirty: bool, since_last_draw: Duration, tick_rate: Duration) -> bool {
    dirty && since_last_draw >= tick_rate
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
    Themes {
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
    /// Active color theme id (`THEME_CATALOG` / `IMPETUS_TUI_THEME`).
    pub theme_id: String,
    /// Composer Prompt / Steer / FollowUp selection (sent on submit).
    pub prompt_intent: impetus_client::protocol::UserPromptIntent,
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
    /// Paced assistant stream (arrival ≠ paint). Keyed by active `run_id`.
    pub stream_run_id: Option<Uuid>,
    pub stream_buffer: StreamBuffer,
    /// Last measured timeline viewport height (rows) for resize-safe scroll clamp.
    pub timeline_viewport_rows: usize,
    /// Last measured timeline line count for resize-safe scroll clamp.
    pub timeline_line_count: usize,
    /// Hit regions recorded during the last paint (cleared each frame).
    pub hit_targets: Vec<HitTarget>,
    /// Previous mouse press for double-click detection.
    pub last_pointer: Option<PointerClick>,
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
            theme_id: crate::theme::theme_id_from_env().to_owned(),
            prompt_intent: impetus_client::protocol::UserPromptIntent::Prompt,
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
            stream_run_id: None,
            stream_buffer: StreamBuffer::new(),
            timeline_viewport_rows: 0,
            timeline_line_count: 0,
            hit_targets: Vec::new(),
            last_pointer: None,
        }
    }

    /// Record timeline metrics from the last paint; clamp scroll if needed.
    pub fn note_timeline_metrics(&mut self, viewport_rows: usize, line_count: usize) {
        self.timeline_viewport_rows = viewport_rows;
        self.timeline_line_count = line_count;
        self.clamp_timeline_scroll();
    }

    /// Keep `line_scroll_from_bottom` inside the visible range after resize/scroll.
    pub fn clamp_timeline_scroll(&mut self) {
        if self.timeline_viewport_rows == 0 {
            return;
        }
        let max = max_scroll_from_bottom(self.timeline_line_count, self.timeline_viewport_rows);
        if self.line_scroll_from_bottom > max {
            self.line_scroll_from_bottom = max;
            if max == 0 {
                self.follow_tail = true;
            }
            self.dirty = true;
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

    pub fn theme(&self) -> crate::theme::Theme {
        crate::theme::resolve_theme(&self.theme_id)
    }

    pub fn set_theme_id(&mut self, id: &str) {
        let resolved = crate::theme::theme_meta(id)
            .map(|meta| meta.id)
            .unwrap_or(crate::theme::DEFAULT_THEME_ID);
        self.theme_id = resolved.to_owned();
        self.dirty = true;
    }

    pub fn cycle_theme(&mut self) {
        let next = crate::theme::cycle_theme_id(&self.theme_id);
        self.set_theme_id(next);
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

    /// Drop paced backlog without painting (session switch / local clear).
    pub fn clear_stream(&mut self) {
        self.stream_buffer.clear();
        self.stream_run_id = None;
    }

    /// Flush paced backlog into the open assistant item and clear stream state.
    pub fn flush_stream_to_timeline(&mut self) {
        let Some(run_id) = self.stream_run_id else {
            self.stream_buffer.clear();
            return;
        };
        let pending = self.stream_buffer.flush();
        if !pending.is_empty() {
            append_stream_body(self, &run_id.to_string(), &pending);
        }
        finalize_stream_item(self, &run_id.to_string());
        self.stream_run_id = None;
        self.dirty = true;
    }
}

pub(crate) fn append_stream_body(app: &mut AppState, key: &str, text: &str) {
    if text.is_empty() {
        return;
    }
    if let Some(item) = app
        .timeline
        .iter_mut()
        .rev()
        .find(|item| item.streaming_key.as_deref() == Some(key))
    {
        item.body.push_str(text);
        item.body = bounded(std::mem::take(&mut item.body), MAX_BODY_CHARS);
        if app.follow_tail {
            app.line_scroll_from_bottom = 0;
        }
        app.dirty = true;
    }
}

pub(crate) fn finalize_stream_item(app: &mut AppState, key: &str) {
    if let Some(item) = app
        .timeline
        .iter_mut()
        .rev()
        .find(|item| item.streaming_key.as_deref() == Some(key))
    {
        item.streaming_key = None;
    }
}

/// Ensure an open assistant card exists for `run_id`, then pace `text` into it.
pub(crate) fn ingest_stream_chunk(
    app: &mut AppState,
    run_id: Uuid,
    sequence: u64,
    at: u64,
    chunk_id: u64,
    text: String,
) {
    let key = run_id.to_string();
    if app.stream_run_id != Some(run_id) {
        if app.stream_run_id.is_some() {
            app.flush_stream_to_timeline();
        }
        app.stream_run_id = Some(run_id);
    }

    let has_item = app
        .timeline
        .iter()
        .rev()
        .any(|item| item.streaming_key.as_deref() == Some(key.as_str()));
    if !has_item {
        let mut item = TimelineItem::new(sequence, at, ItemKind::Assistant, "assistant")
            .with_details(format!("run_id: {run_id}\nchunk_id: {chunk_id}"));
        item.streaming_key = Some(key.clone());
        app.push_item(item);
    } else if let Some(item) = app
        .timeline
        .iter_mut()
        .rev()
        .find(|item| item.streaming_key.as_deref() == Some(key.as_str()))
    {
        item.sequence = sequence;
        item.at_unix_ms = at;
        item.details = format!("run_id: {run_id}\nlast_chunk_id: {chunk_id}");
        app.last_sequence = sequence;
    }

    let revealed = app.stream_buffer.push_text(&text);
    append_stream_body(app, &key, &revealed);
    app.dirty = true;
}

/// Drain one paced frame into the open streaming assistant card.
pub(crate) fn drain_stream_frame(app: &mut AppState) {
    let Some(run_id) = app.stream_run_id else {
        return;
    };
    if app.stream_buffer.is_empty() {
        return;
    }
    let revealed = app.stream_buffer.flush_smooth_frame();
    if revealed.is_empty() {
        return;
    }
    append_stream_body(app, &run_id.to_string(), &revealed);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paste_placeholder_matches_architecture_shape() {
        let body = "line1\nline2\nline3";
        let placeholder = format_paste_placeholder(body.len(), paste_line_count(body));
        assert_eq!(placeholder, "[Pasted text · 1 KB · 3 lines]");
        assert!(is_paste_placeholder(&placeholder));
        assert!(!is_paste_placeholder("plain text"));
    }

    #[test]
    fn normalize_paste_collapses_crlf() {
        assert_eq!(normalize_paste("a\r\nb\rc"), "a\nb\nc");
    }

    #[test]
    fn max_paste_upload_matches_eight_mib() {
        assert_eq!(MAX_PASTE_UPLOAD_BYTES, 8 * 1024 * 1024);
    }

    #[test]
    fn session_summary_maps_fork_meta_and_optional_overrides() {
        let id = Uuid::from_u128(0x1111);
        let parent = Uuid::from_u128(0x2222);
        let derived =
            SessionSummary::from_session_info(id, Some(parent), Some(7), None, None, None);
        assert_eq!(derived.label, format!("fork@7 ← {}", short_id(parent)));
        assert_eq!(derived.status, "fork");
        assert!(derived.workspace.is_none());

        let rich = SessionSummary::from_session_info(
            id,
            Some(parent),
            Some(7),
            Some("TUI architecture".to_owned()),
            Some("working".to_owned()),
            Some("~/dev/impetus".to_owned()),
        );
        assert_eq!(rich.label, "TUI architecture");
        assert_eq!(rich.status, "working");
        assert_eq!(rich.workspace.as_deref(), Some("~/dev/impetus"));
    }

    #[test]
    fn status_strip_includes_connection_run_and_budget() {
        let mut app = AppState::new(ConnectionInfo {
            protocol_version: 3,
            capabilities: BTreeSet::new(),
            label: "demo".to_owned(),
        });
        app.run_state = RunState::Working;
        app.budget.tokens_used = 12_000;
        app.budget.context_used_percent = 42;
        app.budget.turns_used = 3;
        let strip = format_status_strip(&app);
        assert!(strip.contains("demo"));
        assert!(strip.contains("ipc v3"));
        assert!(strip.contains("run working"));
        assert!(strip.contains("12k tok"));
        assert!(strip.contains("ctx 42%"));
        assert!(strip.contains("3 turn"));
    }

    #[test]
    fn remediation_hint_prefers_explicit_then_static() {
        assert_eq!(
            remediation_hint("anything", Some("fix the socket path")),
            "fix the socket path"
        );
        assert!(remediation_hint("policy denied", None).contains("policy"));
        assert_eq!(
            remediation_hint("misc notice", None),
            DEFAULT_ERROR_REMEDIATION
        );
    }

    #[test]
    fn scroll_clamp_caps_offset_after_shrink() {
        let mut app = AppState::new(ConnectionInfo::default());
        app.line_scroll_from_bottom = 80;
        app.follow_tail = false;
        app.note_timeline_metrics(10, 25);
        assert_eq!(app.line_scroll_from_bottom, 15);
        assert!(!app.follow_tail);
        app.note_timeline_metrics(30, 25);
        assert_eq!(app.line_scroll_from_bottom, 0);
        assert!(app.follow_tail);
    }

    #[test]
    fn redraw_coalesce_waits_for_tick() {
        assert!(!should_coalesce_redraw(
            true,
            Duration::from_millis(10),
            Duration::from_millis(33)
        ));
        assert!(should_coalesce_redraw(
            true,
            Duration::from_millis(33),
            Duration::from_millis(33)
        ));
        assert!(!should_coalesce_redraw(
            false,
            Duration::from_millis(100),
            Duration::from_millis(33)
        ));
    }
}
