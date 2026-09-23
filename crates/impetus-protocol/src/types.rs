//! Pure wire DTOs shared by IPC and durable events.
//!
//! No rusqlite, reqwest, Harness, or process/PTY runtime lives here.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::PathBuf;
use thiserror::Error;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Caps / upload limits (wire contracts)
// ---------------------------------------------------------------------------

/// Hard cap for a single assembled upload (8 MiB).
pub const MAX_ARTIFACT_UPLOAD_BYTES: usize = 8 * 1024 * 1024;
/// Max raw chunk size before base64. Keeps one IPC JSON line under 64 KiB.
pub const MAX_ARTIFACT_UPLOAD_CHUNK_BYTES: usize = 24 * 1024;
/// Hard cap for a single text-file read over workspace Files IPC.
pub const MAX_WORKSPACE_FILE_BYTES: usize = 2 * 1024 * 1024;

pub const APPROVAL_DETAIL_SCHEMA_ID: &str = "impetus.approval_detail.v1";
pub const APPROVAL_DETAIL_SCHEMA_VERSION: u16 = 1;

fn default_approval_detail_schema_version() -> u16 {
    APPROVAL_DETAIL_SCHEMA_VERSION
}

// ---------------------------------------------------------------------------
// Session / runtime status
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeStatus {
    Idle,
    AwaitingApproval,
    Running,
    Completed,
    Failed,
    Cancelled,
    InterruptedUnknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionInfo {
    pub id: Uuid,
    pub created_at_unix_ms: u64,
    pub updated_at_unix_ms: u64,
    pub parent_session_id: Option<Uuid>,
    pub fork_sequence: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckpointInfo {
    pub id: Uuid,
    pub session_id: Uuid,
    pub name: String,
    pub sequence: u64,
    pub created_at_unix_ms: u64,
}

// ---------------------------------------------------------------------------
// Execution mode / user intent
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionMode {
    #[default]
    Ask,
    Plan,
    AcceptEdits,
    Auto,
    Bypass,
}

impl ExecutionMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::Ask => "ASK",
            Self::Plan => "PLAN",
            Self::AcceptEdits => "ACCEPT EDITS",
            Self::Auto => "AUTO",
            Self::Bypass => "BYPASS",
        }
    }

    pub fn is_mutating_tool_allowed(self) -> bool {
        !matches!(self, Self::Plan)
    }

    pub fn cycle_next(self) -> Self {
        match self {
            Self::Ask => Self::AcceptEdits,
            Self::AcceptEdits => Self::Plan,
            Self::Plan => Self::Auto,
            Self::Auto | Self::Bypass => Self::Ask,
        }
    }

    pub fn required_ipc_capability(self) -> Option<&'static str> {
        match self {
            Self::AcceptEdits => Some("approval_scope_file_edits"),
            Self::Bypass => Some("approval_scope_full_auto"),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UserPromptIntent {
    #[default]
    Prompt,
    Steer,
    FollowUp,
}

impl UserPromptIntent {
    pub fn label(self) -> &'static str {
        match self {
            Self::Prompt => "prompt",
            Self::Steer => "steer",
            Self::FollowUp => "follow-up",
        }
    }
}

// ---------------------------------------------------------------------------
// Artifacts
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactRef {
    pub id: String,
    pub byte_count: usize,
}

/// Wire/public alias used across IPC + events.
pub type DurableArtifactRef = ArtifactRef;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactMeta {
    pub id: String,
    pub byte_count: usize,
    pub created_unix_ms: u64,
    pub sha256: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_type: Option<String>,
}

pub type DurableArtifactMeta = ArtifactMeta;

// ---------------------------------------------------------------------------
// Policy action fingerprint (approval wire)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ActionOrigin {
    User,
    Agent,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ActionKind {
    ReadFile,
    WriteFile,
    SpawnProcess,
    NetworkConnect,
    SshConnect,
    SftpTransfer,
    TmuxAttach,
    WebSearch,
    WebFetch,
    WebDownload,
    WebBrowser,
    WebSubmit,
    WebUpload,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionSemantics {
    ReadOnly,
    Idempotent,
    Mutating,
    NonReplayable,
}

impl ActionKind {
    pub fn execution_semantics(self) -> ExecutionSemantics {
        match self {
            ActionKind::ReadFile => ExecutionSemantics::ReadOnly,
            ActionKind::WriteFile => ExecutionSemantics::Mutating,
            ActionKind::SpawnProcess => ExecutionSemantics::NonReplayable,
            ActionKind::NetworkConnect => ExecutionSemantics::Idempotent,
            ActionKind::SshConnect => ExecutionSemantics::NonReplayable,
            ActionKind::SftpTransfer => ExecutionSemantics::Mutating,
            ActionKind::TmuxAttach => ExecutionSemantics::NonReplayable,
            ActionKind::WebSearch => ExecutionSemantics::Idempotent,
            ActionKind::WebFetch => ExecutionSemantics::ReadOnly,
            ActionKind::WebDownload => ExecutionSemantics::Mutating,
            ActionKind::WebBrowser => ExecutionSemantics::NonReplayable,
            ActionKind::WebSubmit => ExecutionSemantics::Mutating,
            ActionKind::WebUpload => ExecutionSemantics::Mutating,
        }
    }

    pub fn can_parallelize(self) -> bool {
        matches!(
            self.execution_semantics(),
            ExecutionSemantics::ReadOnly | ExecutionSemantics::Idempotent
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Action {
    pub origin: ActionOrigin,
    pub kind: ActionKind,
    pub summary: String,
    pub target: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(transparent)]
pub struct ActionFingerprint(String);

impl ActionFingerprint {
    pub fn for_action(action: &Action) -> Self {
        Self::for_action_with_version(action, None)
    }

    pub fn for_action_with_version(action: &Action, version: Option<u32>) -> Self {
        let mut payload = serde_json::to_vec(action).expect("action serialization is infallible");
        if let Some(v) = version {
            payload.extend_from_slice(b"\0version:");
            payload.extend_from_slice(v.to_string().as_bytes());
        }
        let digest = Sha256::digest([b"impetus-action-v1\0".as_slice(), &payload].concat());
        Self(digest.iter().map(|byte| format!("{byte:02x}")).collect())
    }
}

impl Action {
    pub fn fingerprint(&self) -> ActionFingerprint {
        ActionFingerprint::for_action(self)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum PolicyDecision {
    Allow,
    NeedsApproval { reason: String },
    Deny { reason: String },
}

pub const POLICY_CONFIG_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PolicyConfigDecision {
    Allow,
    Deny,
    NeedsApproval,
}

impl PolicyConfigDecision {
    pub fn to_decision(self) -> PolicyDecision {
        match self {
            Self::Allow => PolicyDecision::Allow,
            Self::Deny => PolicyDecision::Deny {
                reason: "denied by user policy config".into(),
            },
            Self::NeedsApproval => PolicyDecision::NeedsApproval {
                reason: "requires approval per user policy config".into(),
            },
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct PolicyConfig {
    pub version: u32,
    #[serde(default)]
    pub overrides: BTreeMap<ActionKind, PolicyConfigDecision>,
}

#[derive(Debug, Error)]
pub enum PolicyConfigError {
    #[error("failed to read policy config: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid policy config JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("unsupported policy config version: {0} (expected {POLICY_CONFIG_VERSION})")]
    UnsupportedVersion(u32),
}

impl PolicyConfig {
    pub fn parse(json: &str) -> Result<Self, PolicyConfigError> {
        let config: Self = serde_json::from_str(json)?;
        config.validate()?;
        Ok(config)
    }

    pub fn load_from_path(path: impl AsRef<std::path::Path>) -> Result<Self, PolicyConfigError> {
        let raw = std::fs::read_to_string(path)?;
        Self::parse(&raw)
    }

    fn validate(&self) -> Result<(), PolicyConfigError> {
        if self.version != POLICY_CONFIG_VERSION {
            return Err(PolicyConfigError::UnsupportedVersion(self.version));
        }
        Ok(())
    }

    pub fn override_for(&self, kind: ActionKind) -> Option<PolicyConfigDecision> {
        self.overrides.get(&kind).copied()
    }

    pub fn load_optional(path: impl AsRef<std::path::Path>) -> Result<Self, PolicyConfigError> {
        let path = path.as_ref();
        if !path.exists() {
            return Ok(Self {
                version: POLICY_CONFIG_VERSION,
                overrides: BTreeMap::new(),
            });
        }
        Self::load_from_path(path)
    }
}

pub const POLICY_STORE_VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub enum InstructionKind {
    Soul,
    ProjectRules,
    Convention,
    Guide,
    Skill,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum InstructionScope {
    Global,
    Workspace,
    Path(String),
    Ecosystem(String),
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct InstructionReference {
    pub id: String,
    pub kind: InstructionKind,
    pub scope: InstructionScope,
    pub relative_path: PathBuf,
    pub content_hash: String,
    pub text: String,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct InstructionTokenEstimate {
    pub project_rules: usize,
    pub conventions: usize,
    pub guides: usize,
    pub skills: usize,
}

impl InstructionTokenEstimate {
    pub fn total(&self) -> usize {
        self.project_rules + self.conventions + self.guides + self.skills
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct ResolvedInstructions {
    pub references: Vec<InstructionReference>,
    pub estimated_tokens: InstructionTokenEstimate,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GovernedInstructionRef {
    pub id: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instruction_kind: Option<InstructionKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instruction_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct PolicyStore {
    pub version: u32,
    #[serde(default)]
    pub instructions: Vec<GovernedInstructionRef>,
}

#[derive(Debug, Error)]
pub enum PolicyStoreError {
    #[error("failed to read policy store: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid policy store JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("unsupported policy store version: {0} (expected {POLICY_STORE_VERSION})")]
    UnsupportedVersion(u32),
    #[error("duplicate governed instruction id: {0}")]
    DuplicateId(String),
    #[error("governed instruction id must not be empty")]
    EmptyId,
    #[error("governed instruction label must not be empty")]
    EmptyLabel,
}

impl PolicyStore {
    pub fn parse(json: &str) -> Result<Self, PolicyStoreError> {
        let store: Self = serde_json::from_str(json)?;
        store.validate()?;
        Ok(store)
    }

    pub fn load_from_path(path: impl AsRef<std::path::Path>) -> Result<Self, PolicyStoreError> {
        let raw = std::fs::read_to_string(path)?;
        Self::parse(&raw)
    }

    pub fn load_optional(
        path: impl AsRef<std::path::Path>,
    ) -> Result<Option<Self>, PolicyStoreError> {
        let path = path.as_ref();
        if !path.exists() {
            return Ok(None);
        }
        Self::load_from_path(path).map(Some)
    }

    pub fn export_json(&self) -> Result<String, PolicyStoreError> {
        self.validate()?;
        serde_json::to_string_pretty(self).map_err(PolicyStoreError::from)
    }

    pub fn instructions(&self) -> &[GovernedInstructionRef] {
        &self.instructions
    }

    pub fn governed_ids(&self) -> Vec<&str> {
        self.instructions
            .iter()
            .map(|entry| entry.id.as_str())
            .collect()
    }

    pub fn governed_ids_in(&self, resolved: &ResolvedInstructions) -> Vec<String> {
        let governed: std::collections::BTreeSet<&str> = self
            .instructions
            .iter()
            .filter_map(|entry| entry.instruction_id.as_deref())
            .collect();
        resolved
            .references
            .iter()
            .filter_map(|reference| {
                if governed.contains(reference.id.as_str()) {
                    Some(reference.id.clone())
                } else {
                    None
                }
            })
            .collect()
    }

    /// Validate store version and governed instruction labels (no secrets).
    pub fn validate(&self) -> Result<(), PolicyStoreError> {
        if self.version != POLICY_STORE_VERSION {
            return Err(PolicyStoreError::UnsupportedVersion(self.version));
        }
        let mut seen = std::collections::BTreeSet::new();
        for entry in &self.instructions {
            if entry.id.trim().is_empty() {
                return Err(PolicyStoreError::EmptyId);
            }
            if entry.label.trim().is_empty() {
                return Err(PolicyStoreError::EmptyLabel);
            }
            if !seen.insert(entry.id.clone()) {
                return Err(PolicyStoreError::DuplicateId(entry.id.clone()));
            }
            for text in [&entry.id, &entry.label] {
                if text.contains("sk-") || text.to_ascii_lowercase().contains("password") {
                    return Err(PolicyStoreError::EmptyLabel);
                }
            }
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Approvals / diffs
// ---------------------------------------------------------------------------

pub type ApprovalId = Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ApprovalState {
    Pending,
    Approved,
    Rejected,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalResolver {
    User,
    Agent,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ApprovalResolution {
    pub id: ApprovalId,
    pub action_fingerprint: ActionFingerprint,
    pub intent_revision: u64,
    pub accepted: bool,
    pub resolver: ApprovalResolver,
}

impl ApprovalResolution {
    pub fn user(request: &ApprovalRequest, accepted: bool) -> Self {
        Self {
            id: request.id,
            action_fingerprint: request.action_fingerprint.clone(),
            intent_revision: request.intent_revision,
            accepted,
            resolver: ApprovalResolver::User,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ApprovalRequest {
    pub id: ApprovalId,
    pub action: Action,
    pub action_fingerprint: ActionFingerprint,
    pub capability_version: Option<u32>,
    pub intent_revision: u64,
    pub reason: String,
    pub state: ApprovalState,
}

impl ApprovalRequest {
    pub fn pending(action: Action, reason: String, intent_revision: u64) -> Self {
        Self::pending_with_version(action, reason, intent_revision, None)
    }

    pub fn pending_with_version(
        action: Action,
        reason: String,
        intent_revision: u64,
        capability_version: Option<u32>,
    ) -> Self {
        Self {
            id: Uuid::new_v4(),
            action_fingerprint: ActionFingerprint::for_action_with_version(
                &action,
                capability_version,
            ),
            action,
            capability_version,
            intent_revision,
            reason,
            state: ApprovalState::Pending,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DiffSource {
    Git { commit_range: Option<String> },
    Files { before: PathBuf, after: PathBuf },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DiffHunk {
    pub file: PathBuf,
    pub old_start: usize,
    pub old_lines: usize,
    pub new_start: usize,
    pub new_lines: usize,
    pub preview: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DiffObservation {
    pub source: DiffSource,
    pub files_changed: usize,
    pub insertions: usize,
    pub deletions: usize,
    pub summary: String,
    pub hunks: Vec<DiffHunk>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artifact_ref: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum ScopeEstimate {
    Lines(u32),
    Bytes(u64),
    Operations(u32),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ApprovalDetail {
    #[serde(default = "default_approval_detail_schema_version")]
    pub schema_version: u16,
    pub request: ApprovalRequest,
    pub diff_preview: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diff_observation: Option<DiffObservation>,
    pub affected_files: Vec<String>,
    pub estimated_scope: Option<ScopeEstimate>,
    pub attachment_refs: Vec<Uuid>,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error(
    "approval detail schema version {got} unsupported (expected {APPROVAL_DETAIL_SCHEMA_VERSION})"
)]
pub struct ApprovalDetailSchemaError {
    pub got: u16,
}

impl ApprovalDetail {
    pub fn validate_schema_version(&self) -> Result<(), ApprovalDetailSchemaError> {
        if self.schema_version != APPROVAL_DETAIL_SCHEMA_VERSION {
            return Err(ApprovalDetailSchemaError {
                got: self.schema_version,
            });
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Tools / coding / diagnostics
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadOnlyToolKind {
    List,
    Read,
    Search,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolProvenance {
    pub workspace_root: PathBuf,
    pub relative_path: PathBuf,
    pub in_scope: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolResult {
    pub tool: ReadOnlyToolKind,
    pub provenance: ToolProvenance,
    pub preview: String,
    pub truncated: bool,
    pub artifact: Option<DurableArtifactRef>,
    pub line_count: usize,
    pub byte_count: usize,
    pub original_tokens: usize,
    pub reduced_tokens: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToolOutcome {
    Allowed { result: ToolResult },
    Denied { reason: String, target: PathBuf },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SourcePosition {
    pub line: u32,
    pub character: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SourceRange {
    pub start: SourcePosition,
    pub end: SourcePosition,
}

impl SourceRange {
    pub fn new(start_line: u32, start_character: u32, end_line: u32, end_character: u32) -> Self {
        Self {
            start: SourcePosition {
                line: start_line,
                character: start_character,
            },
            end: SourcePosition {
                line: end_line,
                character: end_character,
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SourceLocation {
    pub path: PathBuf,
    pub range: SourceRange,
}

impl SourceLocation {
    pub fn new(path: impl Into<PathBuf>, range: SourceRange) -> Self {
        Self {
            path: path.into(),
            range,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HoverInfo {
    pub contents: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub range: Option<SourceRange>,
}

/// Diagnostic severity (labels only; coding-tools IPC).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticSeverity {
    Error,
    Warning,
    Information,
    Hint,
}

/// One diagnostic for a path/range (coding-tools IPC; no secrets).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodingDiagnostic {
    pub path: PathBuf,
    pub range: SourceRange,
    pub severity: DiagnosticSeverity,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
}

/// Symbol kind (coarse; not full LSP enum).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SymbolKind {
    File,
    Module,
    Namespace,
    Class,
    Method,
    Function,
    Variable,
    Constant,
    Field,
    Enum,
    Interface,
    Struct,
    TypeParameter,
    Other,
}

/// Document / workspace symbol entry (coding-tools IPC).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocumentSymbol {
    pub name: String,
    pub kind: SymbolKind,
    pub location: SourceLocation,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub container_name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SubsystemHealth {
    pub event_store: SubsystemStatus,
    pub artifact_store: SubsystemStatus,
    pub policy_engine: SubsystemStatus,
    pub provider_registry: SubsystemStatus,
    pub sandbox: SubsystemStatus,
    pub credential_store: SubsystemStatus,
    pub tools_capabilities: SubsystemStatus,
    pub external_agents: SubsystemStatus,
    pub optional_modules: SubsystemStatus,
    pub disk_runtime: SubsystemStatus,
    pub web_research: SubsystemStatus,
    pub output_optimization: SubsystemStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SubsystemStatus {
    pub available: bool,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,
}

impl SubsystemStatus {
    pub fn ok(message: impl Into<String>) -> Self {
        Self {
            available: true,
            message: message.into(),
            details: None,
        }
    }

    pub fn unavailable(message: impl Into<String>) -> Self {
        Self {
            available: false,
            message: message.into(),
            details: None,
        }
    }

    pub fn with_details(mut self, details: serde_json::Value) -> Self {
        self.details = Some(details);
        self
    }
}

// ---------------------------------------------------------------------------
// Child / PTY / MCP / models / workspace / git
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChildResultStatus {
    Completed,
    Failed,
    Cancelled,
}

impl ChildResultStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "completed" => Some(Self::Completed),
            "failed" => Some(Self::Failed),
            "cancelled" => Some(Self::Cancelled),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChildResult {
    pub child_id: String,
    pub parent_id: String,
    pub role_label: String,
    pub status: ChildResultStatus,
    pub summary_label: String,
    pub artifact_ref_labels: Vec<String>,
    pub recorded_unix_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PtySessionState {
    Starting,
    Running { pid: u32 },
    Detached { pid: u32 },
    Exited { exit_code: Option<i32> },
    Failed { reason: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum McpTransport {
    Stdio,
    Http,
    Sse,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpCapabilities {
    pub tools: bool,
    pub resources: bool,
    pub prompts: bool,
    pub sampling: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpServerStatus {
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transport: Option<McpTransport>,
    pub connected: bool,
    pub capabilities: McpCapabilities,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelProviderHealthLabel {
    Unknown,
    Healthy,
    Unavailable { last_error_redacted: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ModelAvailability {
    Available,
    Unavailable,
    #[default]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ModelCapabilityFlags {
    #[serde(default)]
    pub tools: bool,
    #[serde(default)]
    pub reasoning: bool,
    #[serde(default)]
    pub vision: bool,
    #[serde(default)]
    pub context_window: u64,
}

/// Vendor-neutral ACP (or similar agent-backend) capability snapshot for clients.
///
/// No vendor-prefixed fields (`codex_*`, etc.): UI protocol stays agent-agnostic.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct AgentCapabilitySnapshot {
    #[serde(default)]
    pub load_session: bool,
    #[serde(default)]
    pub prompt_image: bool,
    #[serde(default)]
    pub prompt_audio: bool,
    #[serde(default)]
    pub prompt_embedded_context: bool,
    /// Auth method ids advertised by the agent (never secrets).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub auth_method_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_version: Option<String>,
    /// Model select values from agent `session/new` config_options (ACP SoT).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub model_ids: Vec<String>,
    /// ThoughtLevel / reasoning select values from agent config_options (ACP SoT).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub thought_levels: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelProviderStatus {
    pub provider_id: String,
    pub model_id: String,
    pub health: ModelProviderHealthLabel,
    pub is_default: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_display_name: Option<String>,
    #[serde(default)]
    pub availability: ModelAvailability,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reasoning_efforts: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_reasoning_effort: Option<String>,
    #[serde(default)]
    pub capabilities: ModelCapabilityFlags,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub service_tiers: Vec<String>,
    /// Provider-specific non-secret options (never credentials).
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub provider_options: serde_json::Value,
    /// Present for ACP (or similar) backends after initialize/probe.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_capabilities: Option<AgentCapabilitySnapshot>,
}

impl ModelProviderStatus {
    /// Minimal catalog row (ids + health). Does **not** invent capabilities or
    /// reasoning efforts — those come only from provider/discovery metadata.
    pub fn basic(
        provider_id: impl Into<String>,
        model_id: impl Into<String>,
        health: ModelProviderHealthLabel,
        is_default: bool,
    ) -> Self {
        let provider_id = provider_id.into();
        let model_id = model_id.into();
        Self {
            provider_display_name: Some(provider_id.clone()),
            model_display_name: Some(model_id.clone()),
            availability: match &health {
                ModelProviderHealthLabel::Healthy => ModelAvailability::Available,
                ModelProviderHealthLabel::Unavailable { .. } => ModelAvailability::Unavailable,
                ModelProviderHealthLabel::Unknown => ModelAvailability::Unknown,
            },
            reasoning_efforts: Vec::new(),
            default_reasoning_effort: None,
            capabilities: ModelCapabilityFlags::default(),
            service_tiers: Vec::new(),
            provider_options: serde_json::Value::Null,
            agent_capabilities: None,
            provider_id,
            model_id,
            health,
            is_default,
        }
    }

    /// Honest static row when remote discovery is unsupported or failed.
    pub fn unknown_static(
        provider_id: impl Into<String>,
        model_id: impl Into<String>,
        is_default: bool,
    ) -> Self {
        Self::basic(
            provider_id,
            model_id,
            ModelProviderHealthLabel::Unknown,
            is_default,
        )
    }
}

#[cfg(test)]
mod model_provider_status_tests {
    use super::*;

    #[test]
    fn basic_does_not_invent_capabilities_or_reasoning() {
        let status = ModelProviderStatus::basic(
            "mock",
            "mock-model",
            ModelProviderHealthLabel::Healthy,
            true,
        );
        assert!(status.reasoning_efforts.is_empty());
        assert!(status.default_reasoning_effort.is_none());
        assert!(!status.capabilities.tools);
        assert!(!status.capabilities.reasoning);
        assert!(!status.capabilities.vision);
        assert_eq!(status.capabilities.context_window, 0);
        assert_eq!(status.availability, ModelAvailability::Available);
    }

    #[test]
    fn unknown_static_marks_unknown_availability() {
        let status = ModelProviderStatus::unknown_static("p", "m", false);
        assert_eq!(status.health, ModelProviderHealthLabel::Unknown);
        assert_eq!(status.availability, ModelAvailability::Unknown);
        assert!(status.reasoning_efforts.is_empty());
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionModelSelection {
    pub provider_id: String,
    pub model_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
    /// Service tier when the selected model catalog advertises tiers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_tier: Option<String>,
    /// Non-secret adapter request extras (never credentials). Validated on set/load.
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub provider_options: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkspaceDirEntry {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    pub is_symlink: bool,
    pub is_file: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkspaceDirListing {
    pub path: String,
    pub entries: Vec<WorkspaceDirEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkspaceFileMetadata {
    pub path: String,
    pub is_dir: bool,
    pub is_symlink: bool,
    pub is_file: bool,
    pub size: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkspaceFileContent {
    pub path: String,
    pub content: String,
    pub byte_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkspaceSearchHit {
    pub path: String,
    pub line: u32,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkspaceSearchResult {
    pub path: String,
    pub pattern: String,
    pub hits: Vec<WorkspaceSearchHit>,
    pub truncated: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorktreeLifecycleState {
    Active,
    Stopped,
    Stale,
    Closed,
}

impl WorktreeLifecycleState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Stopped => "stopped",
            Self::Stale => "stale",
            Self::Closed => "closed",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitRepositoryState {
    pub repo_root: PathBuf,
    pub worktree_path: PathBuf,
    pub head_sha: Option<String>,
    pub current_branch: Option<String>,
    pub detached: bool,
    pub dirty: bool,
    pub conflict_in_progress: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_lifecycle: Option<WorktreeLifecycleState>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitBranchInfo {
    pub name: String,
    pub current: bool,
    pub upstream: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitCurrentBranch {
    pub name: Option<String>,
    pub detached: bool,
    pub head_sha: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GitChangeKind {
    Modified,
    Added,
    Deleted,
    Renamed,
    Copied,
    Unmerged,
    Untracked,
    Ignored,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitChangedFile {
    pub path: PathBuf,
    pub kind: GitChangeKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status_code: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitStatusSnapshot {
    pub branch: GitCurrentBranch,
    pub dirty: bool,
    pub conflict_in_progress: bool,
    pub files: Vec<GitChangedFile>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitDiffPayload {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<PathBuf>,
    pub patch: String,
    pub truncated: bool,
    pub files_changed: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observation: Option<DiffObservation>,
}

// ---------------------------------------------------------------------------
// Sandbox decision (event helper)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SandboxDecisionState {
    Prepared,
    Denied,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SandboxDecision {
    pub backend: String,
    pub state: SandboxDecisionState,
    pub network_allowed: bool,
    pub writable_root_count: u32,
    pub reason_code: Option<String>,
}

/// Managed worktree binding snapshot for IPC (paths/labels only).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorktreeInfo {
    pub worktree_id: String,
    pub session_id: Uuid,
    pub path: PathBuf,
    pub branch: String,
    pub repo_root: PathBuf,
    pub state: WorktreeLifecycleState,
    /// Present when created via Build role (`create_for_role`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
}

/// Diff summary of a managed worktree branch versus a base ref.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorktreeDiffSummary {
    pub base_ref: String,
    pub branch: String,
    pub files_changed: u64,
    pub insertions: u64,
    pub deletions: u64,
}

/// Pre-merge check of a managed worktree against a base ref.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MergeReadyReport {
    pub merge_ready: bool,
    pub has_conflicts: bool,
    pub dirty: bool,
    pub diff: WorktreeDiffSummary,
}

/// Upsert MCP server config (labels / Keychain env names only — never secret values).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpServerUpsert {
    pub id: String,
    pub name: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    pub transport: McpTransport,
    pub capabilities: McpCapabilities,
    /// Env var names or Keychain labels only.
    #[serde(default)]
    pub env_keys: Vec<String>,
}

/// Visibility boundary for a contextual memory entry (wire).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MemoryEntryScope {
    Project,
    Team,
    User,
}

/// Labels describing where a memory entry came from. Never holds secrets.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryProvenanceInfo {
    pub source: String,
    pub kind: String,
}

/// Contextual memory entry snapshot (labels + redacted content).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryEntryInfo {
    pub id: String,
    pub scope: MemoryEntryScope,
    pub content: String,
    pub provenance: MemoryProvenanceInfo,
}

/// Loaded extension inventory row (daemon ExtensionRuntime; labels only).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtensionStatusInfo {
    pub installation_id: String,
    pub module_id: String,
    pub module_name: String,
    pub version: String,
    /// Source adapter label (`agent_skills`, `mcp`, …).
    pub source: String,
    /// Lifecycle status (`enabled`; Disabled/Unloaded never appear in loaded set).
    pub status: String,
}

/// ExtensionHost package row (labels only; no secrets).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtensionPackageInfo {
    pub id: String,
    pub name: String,
    pub version: String,
    pub extension_api_version: u32,
    /// `global` | `workspace` | `dev`
    pub source: String,
    /// Host phase: loaded | active | disabled | failed | …
    pub phase: String,
    pub capabilities: Vec<String>,
    pub permissions: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    /// False when package failed compat/validation (still listed after partial reload).
    pub compatible: bool,
}

/// Export format for session memory control-plane.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryExportFormat {
    Jsonl,
    Markdown,
}

/// Honest browser provider health for daemon negotiate/health IPC.
///
/// CDP/WebDriver stay Parked — production path reports Absent/Unavailable,
/// never a fake Available.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "status")]
pub enum BrowserHealthStatus {
    Unavailable {
        reason: String,
    },
    Degraded {
        reason: String,
    },
    Misconfigured {
        reason: String,
    },
    Available {
        provider_id: String,
        capabilities: Vec<String>,
    },
}

impl BrowserHealthStatus {
    pub const ABSENT_REASON: &'static str = "no browser provider registered (optional track)";

    pub fn absent() -> Self {
        Self::Unavailable {
            reason: Self::ABSENT_REASON.into(),
        }
    }
}

/// Negotiate result for optional browser track (no session/CDP).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrowserNegotiateInfo {
    pub protocol_version: String,
    pub compatible: bool,
    pub reason: String,
}
