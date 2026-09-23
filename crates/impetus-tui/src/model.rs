use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::time::{Duration, Instant};
use uuid::Uuid;

use impetus_client::protocol::WorkspaceDirEntry;

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

/// Compact composer placeholder for filesystem attach (no raw bytes).
pub fn format_attach_placeholder(
    file_name: &str,
    bytes: usize,
    content_type: Option<&str>,
) -> String {
    let kb = bytes.saturating_add(1023) / 1024;
    let mime = content_type.unwrap_or("application/octet-stream");
    format!("[Attached · {file_name} · {kb} KB · {mime}]")
}

pub fn is_attach_placeholder(text: &str) -> bool {
    let trimmed = text.trim();
    trimmed.starts_with("[Attached · ") && trimmed.ends_with(']')
}

/// Timeline / inspector label for a durable artifact ref (id + size; optional MIME).
pub fn format_artifact_ref_label(id: &str, bytes: usize, content_type: Option<&str>) -> String {
    let kb = bytes.saturating_add(1023) / 1024;
    match content_type {
        Some(mime) if !mime.is_empty() => format!("artifact {id} · {kb} KB · {mime}"),
        _ => format!("artifact {id} · {kb} KB"),
    }
}

/// Best-effort MIME from file extension (upload label only; store may refine).
pub fn guess_content_type(path: &std::path::Path) -> Option<String> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    let mime = match ext.as_str() {
        "txt" | "md" | "rst" | "log" => "text/plain",
        "rs" | "toml" | "json" | "jsonl" | "yaml" | "yml" => "text/plain",
        "html" | "htm" => "text/html",
        "css" => "text/css",
        "js" | "mjs" | "ts" | "tsx" | "jsx" => "text/javascript",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "pdf" => "application/pdf",
        "zip" => "application/zip",
        "gz" | "tgz" => "application/gzip",
        _ => return None,
    };
    Some(mime.to_owned())
}

/// Pending durable artifact after filesystem attach (composer holds label only).
#[derive(Clone, Debug)]
pub struct PendingArtifact {
    pub artifact: impetus_client::protocol::DurableArtifactRef,
    pub path: String,
    pub file_name: String,
    pub content_type: Option<String>,
    pub label: String,
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

pub use impetus_client::protocol::ExecutionMode;

pub const EXECUTION_MODE_ALL: [ExecutionMode; 5] = [
    ExecutionMode::Ask,
    ExecutionMode::Plan,
    ExecutionMode::AcceptEdits,
    ExecutionMode::Auto,
    ExecutionMode::Bypass,
];

pub fn execution_mode_description(mode: ExecutionMode) -> &'static str {
    match mode {
        ExecutionMode::Plan => "Research and plan only; daemon denies mutating tools.",
        ExecutionMode::Ask => "Proceed normally; show every daemon-requested approval.",
        ExecutionMode::AcceptEdits => "Scoped grant for file edits; requires daemon capability.",
        ExecutionMode::Auto => "Policy-allowed autonomy; risky paths still approval-gated.",
        ExecutionMode::Bypass => "Broad grant; requires approval_scope_full_auto capability.",
    }
}

pub fn execution_mode_is_available(mode: ExecutionMode, capabilities: &BTreeSet<String>) -> bool {
    match mode.required_ipc_capability() {
        None => true,
        Some(cap) => capabilities.contains(cap),
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
    /// Compact expandable tree for child runs / tool groups.
    Activity,
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
    /// Structured hunks from harness when available; prefer over [`Self::diff_preview`].
    pub diff_observation: Option<impetus_client::protocol::DiffObservation>,
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
    ExecutionModeChanged {
        mode: ExecutionMode,
    },
    UserInput {
        text: String,
        /// Durable ArtifactRef from Intent (large paste / filesystem attach).
        artifact: Option<impetus_client::protocol::DurableArtifactRef>,
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
    ReasoningSummary {
        run_id: Uuid,
        text: String,
    },
    ChildStarted {
        child_id: String,
        role: String,
        parent_id: String,
    },
    ChildStatus {
        child_id: String,
        status: String,
        current_action: Option<String>,
    },
    ChildFinished {
        child_id: String,
        status: String,
        summary: Option<String>,
        error: Option<String>,
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
    /// Typed activity rows folded into the tools Activity tree.
    ActivityStep {
        label: String,
        detail: Option<String>,
        is_error: bool,
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

/// One row in the Review changed-files list (daemon-backed).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReviewFileRow {
    pub path: String,
    pub kind_label: String,
    pub status_code: String,
    pub insertions: Option<usize>,
    pub deletions: Option<usize>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FilesFocus {
    #[default]
    Tree,
    Preview,
    Search,
}

/// Soft cap for visible directory entries per listing (pagination polish).
pub const FILES_DIR_PAGE_LIMIT: usize = 500;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FilesTreeRow {
    pub path: String,
    pub name: String,
    pub depth: usize,
    pub is_dir: bool,
    pub expanded: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FilesSearchHitRow {
    pub path: String,
    pub line: u32,
    pub text: String,
}

/// Workspace Files overlay state (all entries come from harness IPC).
#[derive(Clone, Debug)]
pub struct FilesOverlayState {
    pub focus: FilesFocus,
    pub selected: usize,
    pub expanded: BTreeSet<String>,
    pub children: BTreeMap<String, Vec<WorkspaceDirEntry>>,
    pub loading_dirs: BTreeSet<String>,
    pub error: Option<String>,
    pub preview_path: Option<String>,
    pub preview_text: Option<String>,
    pub preview_error: Option<String>,
    pub preview_loading: bool,
    pub preview_scroll: usize,
    /// Daemon search query (typed when focus == Search).
    pub search_query: String,
    pub search_hits: Vec<FilesSearchHitRow>,
    pub search_truncated: bool,
    pub search_loading: bool,
    /// When set, tree shows search hits instead of directory walk.
    pub search_active: bool,
    /// Client-side name filter over current tree rows (type-to-filter).
    pub filter: String,
}

impl Default for FilesOverlayState {
    fn default() -> Self {
        Self::new()
    }
}

impl FilesOverlayState {
    pub fn new() -> Self {
        Self {
            focus: FilesFocus::Tree,
            selected: 0,
            expanded: BTreeSet::new(),
            children: BTreeMap::new(),
            loading_dirs: BTreeSet::new(),
            error: None,
            preview_path: None,
            preview_text: None,
            preview_error: None,
            preview_loading: false,
            preview_scroll: 0,
            search_query: String::new(),
            search_hits: Vec::new(),
            search_truncated: false,
            search_loading: false,
            search_active: false,
            filter: String::new(),
        }
    }

    /// Normalize directory cache key (`.` = workspace root).
    pub fn dir_key(path: &str) -> String {
        let trimmed = path.trim().trim_end_matches('/');
        if trimmed.is_empty() || trimmed == "." {
            ".".to_owned()
        } else {
            trimmed.to_owned()
        }
    }

    pub fn visible_rows(&self) -> Vec<FilesTreeRow> {
        if self.search_active {
            return self
                .search_hits
                .iter()
                .map(|hit| FilesTreeRow {
                    path: hit.path.clone(),
                    name: format!("{}:{} {}", hit.path, hit.line, truncate_hit(&hit.text, 48)),
                    depth: 0,
                    is_dir: false,
                    expanded: false,
                })
                .collect();
        }
        let mut rows = Vec::new();
        self.append_children(".", 0, &mut rows);
        if self.filter.trim().is_empty() {
            return rows;
        }
        let needle = self.filter.to_ascii_lowercase();
        rows.into_iter()
            .filter(|row| row.name.to_ascii_lowercase().contains(&needle))
            .collect()
    }

    fn append_children(&self, dir: &str, depth: usize, out: &mut Vec<FilesTreeRow>) {
        let key = Self::dir_key(dir);
        let Some(entries) = self.children.get(&key) else {
            return;
        };
        for entry in entries {
            let expanded = entry.is_dir && self.expanded.contains(&entry.path);
            out.push(FilesTreeRow {
                path: entry.path.clone(),
                name: entry.name.clone(),
                depth,
                is_dir: entry.is_dir,
                expanded,
            });
            if expanded {
                self.append_children(&entry.path, depth + 1, out);
            }
        }
    }

    pub fn selected_row(&self) -> Option<FilesTreeRow> {
        let rows = self.visible_rows();
        rows.get(self.selected).cloned()
    }

    pub fn clamp_selected(&mut self) {
        let len = self.visible_rows().len();
        if len == 0 {
            self.selected = 0;
        } else {
            self.selected = self.selected.min(len - 1);
        }
    }

    pub fn apply_listing(&mut self, path: &str, mut entries: Vec<WorkspaceDirEntry>) {
        let key = Self::dir_key(path);
        self.loading_dirs.remove(&key);
        if entries.len() > FILES_DIR_PAGE_LIMIT {
            entries.truncate(FILES_DIR_PAGE_LIMIT);
            self.error = Some(format!(
                "directory truncated to {FILES_DIR_PAGE_LIMIT} entries (pagination cap)"
            ));
        } else {
            self.error = None;
        }
        self.children.insert(key, entries);
        self.clamp_selected();
    }

    pub fn apply_search(&mut self, hits: Vec<FilesSearchHitRow>, truncated: bool) {
        self.search_loading = false;
        self.search_hits = hits;
        self.search_truncated = truncated;
        self.search_active = true;
        self.selected = 0;
        self.focus = FilesFocus::Tree;
        self.error = if truncated {
            Some("search truncated (daemon hit cap)".to_owned())
        } else {
            None
        };
    }

    pub fn clear_search(&mut self) {
        self.search_query.clear();
        self.search_hits.clear();
        self.search_truncated = false;
        self.search_loading = false;
        self.search_active = false;
        self.filter.clear();
        self.focus = FilesFocus::Tree;
        self.clamp_selected();
    }

    pub fn begin_refresh(&mut self) {
        self.children.clear();
        self.expanded.clear();
        self.loading_dirs.clear();
        self.error = None;
        self.preview_path = None;
        self.preview_text = None;
        self.preview_error = None;
        self.preview_loading = false;
        self.preview_scroll = 0;
        self.selected = 0;
        self.search_query.clear();
        self.search_hits.clear();
        self.search_truncated = false;
        self.search_loading = false;
        self.search_active = false;
        self.filter.clear();
        self.focus = FilesFocus::Tree;
        self.loading_dirs.insert(".".to_owned());
    }
}

fn truncate_hit(text: &str, max_chars: usize) -> String {
    let flat: String = text
        .chars()
        .map(|c| if c.is_whitespace() { ' ' } else { c })
        .collect();
    if flat.chars().count() <= max_chars {
        flat
    } else {
        let end = flat
            .char_indices()
            .nth(max_chars.saturating_sub(1))
            .map(|(i, _)| i)
            .unwrap_or(flat.len());
        format!("{}…", &flat[..end])
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ReviewFocus {
    #[default]
    Files,
    Diff,
}

/// Daemon-backed Review overlay (GitStatus / ListChangedFiles / GetFileDiff).
#[derive(Clone, Debug)]
pub struct ReviewOverlayState {
    pub focus: ReviewFocus,
    pub selected: usize,
    pub files: Vec<ReviewFileRow>,
    pub branch_label: String,
    pub dirty: bool,
    pub loading: bool,
    pub error: Option<String>,
    pub diff_path: Option<String>,
    pub diff_patch: Option<String>,
    pub diff_observation: Option<impetus_client::protocol::DiffObservation>,
    pub diff_loading: bool,
    pub diff_error: Option<String>,
    pub diff_scroll: usize,
    pub hunk_line_idxs: Vec<usize>,
    pub selected_hunk: usize,
}

impl Default for ReviewOverlayState {
    fn default() -> Self {
        Self::new()
    }
}

impl ReviewOverlayState {
    pub fn new() -> Self {
        Self {
            focus: ReviewFocus::Files,
            selected: 0,
            files: Vec::new(),
            branch_label: String::new(),
            dirty: false,
            loading: true,
            error: None,
            diff_path: None,
            diff_patch: None,
            diff_observation: None,
            diff_loading: false,
            diff_error: None,
            diff_scroll: 0,
            hunk_line_idxs: Vec::new(),
            selected_hunk: 0,
        }
    }

    pub fn clamp_selected(&mut self) {
        if self.files.is_empty() {
            self.selected = 0;
        } else {
            self.selected = self.selected.min(self.files.len() - 1);
        }
    }

    pub fn selected_path(&self) -> Option<&str> {
        self.files.get(self.selected).map(|row| row.path.as_str())
    }

    pub fn apply_snapshot(&mut self, branch_label: String, dirty: bool, files: Vec<ReviewFileRow>) {
        self.branch_label = branch_label;
        self.dirty = dirty;
        self.files = files;
        self.loading = false;
        self.error = None;
        self.clamp_selected();
    }

    pub fn set_diff(
        &mut self,
        path: String,
        patch: String,
        hunk_idxs: Vec<usize>,
        observation: Option<impetus_client::protocol::DiffObservation>,
    ) {
        self.diff_path = Some(path);
        self.diff_patch = Some(patch);
        self.diff_observation = observation;
        self.hunk_line_idxs = hunk_idxs;
        self.selected_hunk = 0;
        self.diff_scroll = 0;
        self.diff_loading = false;
        self.diff_error = None;
    }

    pub fn jump_hunk(&mut self, delta: isize) {
        if self.hunk_line_idxs.is_empty() {
            return;
        }
        let len = self.hunk_line_idxs.len() as isize;
        let next = (self.selected_hunk as isize + delta).rem_euclid(len) as usize;
        self.selected_hunk = next;
        self.diff_scroll = self.hunk_line_idxs[next];
        self.focus = ReviewFocus::Diff;
    }
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
    /// Path prompt for filesystem → durable artifact attach.
    AttachPath {
        path: String,
    },
    Diagnostics {
        text: String,
    },
    Message {
        title: String,
        body: String,
        error: bool,
    },
    Files {
        state: FilesOverlayState,
    },
    /// Git branch picker (list/filter/switch/create via harness IPC).
    Branches {
        selected: usize,
        query: String,
        branches: Vec<impetus_client::protocol::GitBranchInfo>,
    },
    /// Changed-files + file diff via harness Git IPC.
    Review {
        state: ReviewOverlayState,
    },
    /// Minimal path/name prompt (workspace root or checkpoint name).
    TextPrompt {
        kind: TextPromptKind,
        title: String,
        value: String,
    },
    /// Durable session checkpoints (list + Enter restore).
    Checkpoints {
        selected: usize,
        checkpoints: Vec<impetus_client::protocol::CheckpointInfo>,
    },
    /// Provider → Model → Reasoning → options from daemon catalog (#337).
    ModelPicker {
        state: crate::catalog::ModelPickerState,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextPromptKind {
    WorkspaceRoot,
    CheckpointName,
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
    /// Uploaded filesystem artifact waiting for prompt submit (label in composer).
    pub pending_artifact: Option<PendingArtifact>,
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
    /// Current git branch from daemon (header / post-switch refresh).
    pub current_branch: Option<String>,
    /// Daemon `ListProviders` catalog (no hard-coded vendors).
    pub provider_catalog: Vec<impetus_client::protocol::ModelProviderStatus>,
    /// Active session model from `Get`/`SetSessionModel` (daemon SoT).
    pub session_model: Option<impetus_client::protocol::SessionModelSelection>,
    /// Local options draft from catalog (`service_tier` / `provider_options`).
    /// Passthrough via SetSessionModel waits on #328.
    pub session_model_options: Option<serde_json::Value>,
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
            pending_artifact: None,
            toast: None,
            status_message: "ready".to_owned(),
            subscription_generation: 0,
            stream_run_id: None,
            stream_buffer: StreamBuffer::new(),
            timeline_viewport_rows: 0,
            timeline_line_count: 0,
            hit_targets: Vec::new(),
            last_pointer: None,
            current_branch: None,
            provider_catalog: Vec::new(),
            session_model: None,
            session_model_options: None,
        }
    }

    pub fn session_model_label(&self) -> String {
        crate::catalog::session_model_label(
            self.session_model.as_ref(),
            self.session_model_options.as_ref(),
        )
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
            .and_then(|id| {
                self.sessions
                    .iter()
                    .find(|session| session.id == id)
                    .map(|session| session.label.clone())
            })
            .or_else(|| self.active_session.map(short_id))
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
    fn attach_placeholder_shows_name_size_mime_not_bytes() {
        let label = format_attach_placeholder("notes.txt", 2048, Some("text/plain"));
        assert_eq!(label, "[Attached · notes.txt · 2 KB · text/plain]");
        assert!(is_attach_placeholder(&label));
        assert!(!is_attach_placeholder("[Pasted text · 1 KB · 1 lines]"));
        assert_eq!(
            guess_content_type(std::path::Path::new("/tmp/x.rs")).as_deref(),
            Some("text/plain")
        );
        assert!(guess_content_type(std::path::Path::new("/tmp/x.bin")).is_none());
        assert_eq!(
            format_artifact_ref_label("abc", 1500, Some("text/plain")),
            "artifact abc · 2 KB · text/plain"
        );
        assert_eq!(
            format_artifact_ref_label("abc", 100, None),
            "artifact abc · 1 KB"
        );
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

    #[test]
    fn files_overlay_expand_collapse_visible_rows() {
        use impetus_client::protocol::WorkspaceDirEntry;

        let mut state = FilesOverlayState::new();
        state.apply_listing(
            ".",
            vec![
                WorkspaceDirEntry {
                    name: "README.md".into(),
                    path: "README.md".into(),
                    is_dir: false,
                    is_symlink: false,
                    is_file: true,
                },
                WorkspaceDirEntry {
                    name: "src".into(),
                    path: "src".into(),
                    is_dir: true,
                    is_symlink: false,
                    is_file: false,
                },
            ],
        );
        assert_eq!(state.visible_rows().len(), 2);

        state.expanded.insert("src".into());
        state.apply_listing(
            "src",
            vec![WorkspaceDirEntry {
                name: "main.rs".into(),
                path: "src/main.rs".into(),
                is_dir: false,
                is_symlink: false,
                is_file: true,
            }],
        );
        let rows = state.visible_rows();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[2].path, "src/main.rs");
        assert_eq!(rows[2].depth, 1);

        state.expanded.remove("src");
        assert_eq!(state.visible_rows().len(), 2);
    }
}
