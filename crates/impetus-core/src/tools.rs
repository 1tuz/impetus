//! Read-only workspace tools with bounded output and disk-backed artifacts.
//!
//! These tools are the first safe evidence vertical slice: they never mutate
//! the workspace and every bounded result keeps an `ArtifactRef` so the model
//! context does not grow linearly with output size. Intended effects are
//! normalized into policy actions before any filesystem access.

use crate::{
    ActionOrigin, EffectExecution, EffectSeam, EventPayload, NormalizedEffect, NoticeEvent,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use thiserror::Error;

/// Maximum UTF-8 bytes kept inline in a tool preview. The full bounded,
/// redacted result is stored as an artifact when this limit is exceeded.
pub const MAX_TOOL_PREVIEW_BYTES: usize = 16 * 1024;
/// Maximum number of files scanned by `search` per call.
pub const MAX_SEARCH_FILES: usize = 200;
/// Maximum bytes read from a single file by any tool.
pub const MAX_FILE_READ_BYTES: usize = 2 * 1024 * 1024;
/// Maximum captured result bytes before list/search stops collecting output.
pub const MAX_TOOL_CAPTURE_BYTES: usize = 2 * 1024 * 1024;
/// Largest artifact that memory testing should exercise without a disk write.
pub const ARTIFACT_CHUNK_SIZE: usize = 32 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadOnlyToolKind {
    List,
    Read,
    Search,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadOnlyTool {
    List { target: PathBuf },
    Read { target: PathBuf },
    Search { target: PathBuf, pattern: String },
}

impl ReadOnlyTool {
    pub fn kind(&self) -> ReadOnlyToolKind {
        match self {
            ReadOnlyTool::List { .. } => ReadOnlyToolKind::List,
            ReadOnlyTool::Read { .. } => ReadOnlyToolKind::Read,
            ReadOnlyTool::Search { .. } => ReadOnlyToolKind::Search,
        }
    }
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
    pub artifact: Option<ArtifactRef>,
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

#[derive(Debug, Error)]
pub enum ToolError {
    #[error("io error while running read-only tool: {0}")]
    Io(String),
    #[error("artifact store error: {0}")]
    Artifact(String),
}

/// Read-only tools bound to a canonical workspace root. They normalize every
/// intent into a policy `Action` and only touch the filesystem when policy
/// allows it.
pub struct ReadOnlyTools {
    root: PathBuf,
}

impl ReadOnlyTools {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Resolve `target` to an absolute path guaranteed inside the workspace.
    /// Returns `None` when the target escapes the root or cannot be proven
    /// contained (e.g. a symlink pointing outside).
    fn resolve_in_scope(&self, target: &Path) -> Option<PathBuf> {
        let candidate = if target.is_absolute() {
            target.to_path_buf()
        } else {
            self.root.join(target)
        };
        let root = self.root.canonicalize().ok()?;
        let resolved = candidate.canonicalize().ok()?;
        resolved.starts_with(&root).then_some(resolved)
    }

    fn normalize(&self, tool: &ReadOnlyTool, origin: ActionOrigin) -> NormalizedEffect {
        let target = tool.target_string().unwrap_or_else(|| ".".to_string());
        NormalizedEffect::workspace_read(
            origin,
            match tool.kind() {
                ReadOnlyToolKind::List => "list workspace directory",
                ReadOnlyToolKind::Read => "read workspace file",
                ReadOnlyToolKind::Search => "search content",
            },
            target,
        )
    }

    pub fn run(
        &self,
        tool: ReadOnlyTool,
        origin: ActionOrigin,
        artifacts: &ArtifactStore,
    ) -> Result<ToolOutcome, ToolError> {
        let seam = EffectSeam::workspace_read(&self.root);
        self.run_with_seam(tool, origin, artifacts, &seam)
    }

    /// Execute through the harness-owned effect seam.  IPC callers must use
    /// this entry point so the policy that admitted a client request is the
    /// same policy evaluated immediately before capability execution.
    pub fn run_with_seam(
        &self,
        tool: ReadOnlyTool,
        origin: ActionOrigin,
        artifacts: &ArtifactStore,
        seam: &EffectSeam,
    ) -> Result<ToolOutcome, ToolError> {
        let effect = self.normalize(&tool, origin);
        let outcome = seam.execute(&effect, || self.execute_capability(tool, artifacts))?;
        Ok(match outcome {
            EffectExecution::Executed(outcome) => outcome,
            EffectExecution::Denied { reason } => ToolOutcome::Denied {
                reason,
                target: effect.action.target.unwrap_or_default().into(),
            },
            EffectExecution::NeedsApproval { .. } => ToolOutcome::Denied {
                reason: "read-only tool needs an approval that is not granted in read-only scope"
                    .into(),
                target: effect.action.target.unwrap_or_default().into(),
            },
        })
    }

    fn execute_capability(
        &self,
        tool: ReadOnlyTool,
        artifacts: &ArtifactStore,
    ) -> Result<ToolOutcome, ToolError> {
        let resolved = match tool {
            ReadOnlyTool::List { ref target }
            | ReadOnlyTool::Read { ref target }
            | ReadOnlyTool::Search { ref target, .. } => self.resolve_in_scope(target),
        };
        let Some(resolved) = resolved else {
            return Ok(ToolOutcome::Denied {
                reason: "target is missing or outside the workspace scope".into(),
                target: tool.target_string().unwrap_or_default().into(),
            });
        };

        let relative = resolved
            .strip_prefix(
                self.root
                    .canonicalize()
                    .map_err(|e| ToolError::Io(e.to_string()))?,
            )
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|_| PathBuf::from("."));

        let provenance = ToolProvenance {
            workspace_root: self.root.clone(),
            relative_path: relative,
            in_scope: true,
        };

        let full = match &tool {
            ReadOnlyTool::List { .. } => self.list_dir(&resolved)?,
            ReadOnlyTool::Read { .. } => self.read_file(&resolved)?,
            ReadOnlyTool::Search { pattern, .. } => self.search(&resolved, pattern)?,
        };
        let full = redact_text(&full);

        let byte_count = full.len();
        let line_count = full.lines().count();

        // Apply token-based reduction
        let reducer = crate::OutputReducer::default();
        let reduced = reducer.reduce(&full);

        // Store full output as artifact if truncated (byte or token)
        let truncated = byte_count > MAX_TOOL_PREVIEW_BYTES || reduced.truncated;
        let artifact = if truncated {
            Some(
                artifacts
                    .store(full.as_bytes())
                    .map_err(|e| ToolError::Artifact(e.to_string()))?,
            )
        } else {
            None
        };

        // Use reduced content as preview
        let preview = reduced.content.into_owned();

        Ok(ToolOutcome::Allowed {
            result: ToolResult {
                tool: tool.kind(),
                provenance,
                preview,
                truncated,
                artifact,
                line_count,
                byte_count,
                original_tokens: reduced.original_tokens,
                reduced_tokens: reduced.reduced_tokens,
            },
        })
    }

    fn list_dir(&self, path: &Path) -> Result<String, ToolError> {
        let mut entries: Vec<String> = Vec::new();
        let mut captured_bytes = 0usize;
        let mut read = std::fs::read_dir(path).map_err(|e| ToolError::Io(e.to_string()))?;
        while let Some(entry) = read
            .next()
            .transpose()
            .map_err(|e| ToolError::Io(e.to_string()))?
        {
            let name = entry.file_name().to_string_lossy().into_owned();
            let kind = entry
                .file_type()
                .map_err(|e| ToolError::Io(e.to_string()))?;
            let suffix = if kind.is_dir() { "/" } else { "" };
            let rendered = format!("{name}{suffix}");
            captured_bytes = captured_bytes.saturating_add(rendered.len() + 1);
            if captured_bytes > MAX_TOOL_CAPTURE_BYTES {
                entries.push("... listing truncated at output limit".into());
                break;
            }
            entries.push(rendered);
        }
        entries.sort();
        Ok(entries.join("\n"))
    }

    fn read_file(&self, path: &Path) -> Result<String, ToolError> {
        let source_bytes = std::fs::metadata(path)
            .map_err(|e| ToolError::Io(e.to_string()))?
            .len();
        let mut bytes = Vec::with_capacity(
            usize::try_from(source_bytes)
                .unwrap_or(MAX_FILE_READ_BYTES)
                .min(MAX_FILE_READ_BYTES),
        );
        std::fs::File::open(path)
            .map_err(|e| ToolError::Io(e.to_string()))?
            .take(MAX_FILE_READ_BYTES as u64)
            .read_to_end(&mut bytes)
            .map_err(|e| ToolError::Io(e.to_string()))?;
        let mut output = String::from_utf8_lossy(&bytes).into_owned();
        if source_bytes > MAX_FILE_READ_BYTES as u64 {
            output.push_str("\n... file truncated at read limit");
        }
        Ok(output)
    }

    fn search(&self, root: &Path, pattern: &str) -> Result<String, ToolError> {
        let needle = pattern.to_lowercase();
        let mut matches: Vec<String> = Vec::new();
        let mut captured_bytes = 0usize;
        let mut scanned = 0usize;
        let mut stack = vec![root.to_path_buf()];
        while let Some(dir) = stack.pop() {
            if scanned >= MAX_SEARCH_FILES {
                matches.push(format!("... search stopped after {MAX_SEARCH_FILES} files"));
                break;
            }
            let mut read = match std::fs::read_dir(&dir) {
                Ok(read) => read,
                Err(_) => continue,
            };
            while let Some(entry) = read
                .next()
                .transpose()
                .map_err(|e| ToolError::Io(e.to_string()))?
            {
                let path = entry.path();
                let name = entry.file_name().to_string_lossy().into_owned();
                if name == ".git" || name == "target" || name.starts_with('.') {
                    continue;
                }
                let file_type = match entry.file_type() {
                    Ok(ft) => ft,
                    Err(_) => continue,
                };
                if file_type.is_dir() {
                    stack.push(path);
                } else if file_type.is_file() {
                    if scanned >= MAX_SEARCH_FILES {
                        matches.push(format!("... search stopped after {MAX_SEARCH_FILES} files"));
                        return Ok(matches.join("\n"));
                    }
                    scanned += 1;
                    let file_size = std::fs::metadata(&path).map(|meta| meta.len()).unwrap_or(0);
                    if file_size <= MAX_FILE_READ_BYTES as u64
                        && let Ok(file) = std::fs::File::open(&path)
                    {
                        let mut bytes = Vec::with_capacity(file_size as usize);
                        if file
                            .take(MAX_FILE_READ_BYTES as u64)
                            .read_to_end(&mut bytes)
                            .is_err()
                        {
                            continue;
                        }
                        let text = String::from_utf8_lossy(&bytes);
                        for (line_no, line) in text.lines().enumerate() {
                            if line.to_lowercase().contains(&needle) {
                                let rendered = format!(
                                    "{}:{}: {}",
                                    path.strip_prefix(root).unwrap_or(&path).display(),
                                    line_no + 1,
                                    line
                                );
                                captured_bytes = captured_bytes.saturating_add(rendered.len() + 1);
                                if captured_bytes > MAX_TOOL_CAPTURE_BYTES {
                                    matches.push("... search truncated at output limit".into());
                                    return Ok(matches.join("\n"));
                                }
                                matches.push(rendered);
                            }
                        }
                    }
                }
            }
        }
        if matches.is_empty() {
            matches.push("(no matches)".into());
        }
        Ok(matches.join("\n"))
    }
}

impl ReadOnlyTool {
    fn target_string(&self) -> Option<String> {
        match self {
            ReadOnlyTool::List { target }
            | ReadOnlyTool::Read { target }
            | ReadOnlyTool::Search { target, .. } => Some(target.to_string_lossy().into_owned()),
        }
    }
}

/// Disk-backed artifact store with a content hash, metadata, range-read and
/// deterministic identity. The full content stays on disk; only refs travel
/// through harness events and model context.
pub struct ArtifactStore {
    root: PathBuf,
    index: Mutex<BTreeMap<String, ArtifactMeta>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactRef {
    pub id: String,
    pub byte_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactMeta {
    pub id: String,
    pub byte_count: usize,
    pub created_unix_ms: u64,
}

impl ArtifactStore {
    pub fn open(root: impl Into<PathBuf>) -> Result<Self, ToolError> {
        let root = root.into();
        std::fs::create_dir_all(&root).map_err(|e| ToolError::Artifact(e.to_string()))?;
        Ok(Self {
            root,
            index: Mutex::new(BTreeMap::new()),
        })
    }

    pub fn store(&self, bytes: &[u8]) -> Result<ArtifactRef, ToolError> {
        let id = content_hash(bytes);
        let path = self.root.join(&id);
        if !path.exists() {
            std::fs::write(&path, bytes).map_err(|e| ToolError::Artifact(e.to_string()))?;
        }
        let meta = ArtifactMeta {
            id: id.clone(),
            byte_count: bytes.len(),
            created_unix_ms: now_unix_ms(),
        };
        self.index
            .lock()
            .map_err(|_| ToolError::Artifact("artifact index poisoned".into()))?
            .insert(id.clone(), meta);
        Ok(ArtifactRef {
            id,
            byte_count: bytes.len(),
        })
    }

    pub fn read_range(&self, id: &str, start: usize, len: usize) -> Result<Vec<u8>, ToolError> {
        let path = self.root.join(id);
        let bytes = std::fs::read(&path).map_err(|e| ToolError::Artifact(e.to_string()))?;
        let end = (start + len).min(bytes.len());
        Ok(bytes.get(start..end).unwrap_or(&[]).to_vec())
    }

    pub fn metadata(&self, id: &str) -> Option<ArtifactMeta> {
        self.index.lock().ok()?.get(id).cloned()
    }
}

fn content_hash(bytes: &[u8]) -> String {
    // FNV-1a 64-bit, hex-encoded. Deterministic and dependency-free; used only
    // for content-addressed dedup, not for security.
    let mut hash: u64 = 0xcbf29ce484222325;
    for &byte in bytes {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}

fn now_unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn truncate_utf8(source: &str, max_bytes: usize) -> &str {
    let mut end = source.len().min(max_bytes);
    while !source.is_char_boundary(end) {
        end -= 1;
    }
    &source[..end]
}

pub(crate) fn redact_text(source: &str) -> String {
    let mut output = String::with_capacity(source.len());
    let mut private_key_block = false;
    for segment in source.split_inclusive('\n') {
        let (line, newline) = segment
            .strip_suffix('\n')
            .map_or((segment, ""), |line| (line, "\n"));
        let upper = line.to_ascii_uppercase();
        if upper.contains("-----BEGIN") && upper.contains("PRIVATE KEY-----") {
            private_key_block = true;
            output.push_str("[REDACTED PRIVATE KEY]");
        } else if private_key_block {
            output.push_str("[REDACTED]");
            if upper.contains("-----END") && upper.contains("PRIVATE KEY-----") {
                private_key_block = false;
            }
        } else {
            output.push_str(&redact_line(line));
        }
        output.push_str(newline);
    }
    output
}

fn redact_line(line: &str) -> String {
    let trimmed = line.trim_start();
    let upper = trimmed.to_ascii_uppercase();
    if upper.starts_with("AUTHORIZATION:") || upper.starts_with("BEARER ") {
        return format!("{}[REDACTED]", &line[..line.len() - trimmed.len()]);
    }
    for separator in ['=', ':'] {
        let Some((key, _value)) = line.split_once(separator) else {
            continue;
        };
        let key = key.trim().to_ascii_uppercase();
        if [
            "TOKEN",
            "SECRET",
            "PASSWORD",
            "PASS_PHRASE",
            "PASSPHRASE",
            "API_KEY",
            "PRIVATE_KEY",
            "ACCESS_KEY",
            "CREDENTIAL",
        ]
        .iter()
        .any(|marker| key.contains(marker))
        {
            return format!(
                "{}{separator}[REDACTED]",
                &line[..line.find(separator).unwrap_or(0)]
            );
        }
    }
    line.to_owned()
}

/// Record the durable tool lifecycle for a read-only tool outcome. A denied
/// outcome is recorded as a typed policy denial so the supervisor can keep a
/// safe plan instead of failing the whole run.
pub fn record_tool_outcome(
    runtime: &crate::AgentRuntime,
    outcome: &ToolOutcome,
) -> Result<(), crate::RuntimeError> {
    match outcome {
        ToolOutcome::Allowed { result } => {
            let name = match result.tool {
                ReadOnlyToolKind::List => "list",
                ReadOnlyToolKind::Read => "read",
                ReadOnlyToolKind::Search => "search",
            };
            runtime.record_tool_started(name)?;
            let summary = if result.truncated {
                format!(
                    "{} lines/{} bytes, truncated, artifact {}",
                    result.line_count,
                    result.byte_count,
                    result
                        .artifact
                        .as_ref()
                        .map(|a| a.id.as_str())
                        .unwrap_or("none")
                )
            } else {
                format!("{} lines/{} bytes", result.line_count, result.byte_count)
            };
            runtime.record_tool_finished(name, &summary)?;
        }
        ToolOutcome::Denied { reason, .. } => {
            runtime.record_event(EventPayload::Notice(NoticeEvent::PolicyDenied {
                reason: reason.clone(),
            }))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn temp_root() -> (PathBuf, ArtifactStore) {
        let root = std::env::temp_dir().join(format!("agentic-tools-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&root).expect("create temp workspace");
        let artifacts = ArtifactStore::open(root.join("artifacts")).expect("open artifact store");
        (root, artifacts)
    }

    #[test]
    fn read_in_scope_returns_allowed_result() {
        let (root, artifacts) = temp_root();
        std::fs::write(root.join("note.txt"), "hello workspace").expect("write note");
        let tools = ReadOnlyTools::new(&root);
        let outcome = tools
            .run(
                ReadOnlyTool::Read {
                    target: "note.txt".into(),
                },
                ActionOrigin::User,
                &artifacts,
            )
            .expect("run read");
        match outcome {
            ToolOutcome::Allowed { result } => {
                assert_eq!(result.preview, "hello workspace");
                assert!(!result.truncated);
                assert!(result.artifact.is_none());
                assert!(result.provenance.in_scope);
            }
            ToolOutcome::Denied { .. } => panic!("in-scope read must be allowed"),
        }
    }

    #[test]
    fn read_outside_workspace_is_denied() {
        let (root, artifacts) = temp_root();
        let tools = ReadOnlyTools::new(&root);
        let outcome = tools
            .run(
                ReadOnlyTool::Read {
                    target: "/etc/hosts".into(),
                },
                ActionOrigin::User,
                &artifacts,
            )
            .expect("run read");
        assert!(matches!(outcome, ToolOutcome::Denied { .. }));
    }

    #[test]
    fn large_read_is_truncated_with_artifact_reference() {
        let (root, artifacts) = temp_root();
        let big = "line content\n".repeat(3000); // ~39KB, exceeds 16KB preview
        std::fs::write(root.join("big.txt"), &big).expect("write big file");
        let tools = ReadOnlyTools::new(&root);
        let outcome = tools
            .run(
                ReadOnlyTool::Read {
                    target: "big.txt".into(),
                },
                ActionOrigin::User,
                &artifacts,
            )
            .expect("run read");
        let ToolOutcome::Allowed { result } = outcome else {
            panic!("large read should be allowed");
        };
        assert!(result.truncated);
        assert!(result.preview.len() <= MAX_TOOL_PREVIEW_BYTES);
        let artifact = result.artifact.as_ref().expect("artifact ref present");
        let full = artifacts
            .read_range(&artifact.id, 0, artifact.byte_count)
            .expect("read artifact range");
        assert_eq!(full.len(), big.len());
    }

    #[test]
    fn search_finds_pattern_without_mutating_workspace() {
        let (root, artifacts) = temp_root();
        std::fs::write(root.join("a.rs"), "fn main() {}\n").expect("write a");
        std::fs::write(root.join("b.rs"), "struct Bar;\n").expect("write b");
        let tools = ReadOnlyTools::new(&root);
        let outcome = tools
            .run(
                ReadOnlyTool::Search {
                    target: ".".into(),
                    pattern: "fn main".into(),
                },
                ActionOrigin::User,
                &artifacts,
            )
            .expect("run search");
        let ToolOutcome::Allowed { result } = outcome else {
            panic!("search should be allowed");
        };
        assert!(result.preview.contains("fn main"));
        assert!(!result.preview.contains("struct Bar"));
    }

    #[test]
    fn artifact_store_is_content_addressed_and_range_readable() {
        let (_root, artifacts) = temp_root();
        let bytes = b"the quick brown fox".to_vec();
        let first = artifacts.store(&bytes).expect("store");
        let second = artifacts.store(&bytes).expect("store again");
        assert_eq!(first.id, second.id, "identical content shares an id");
        let slice = artifacts.read_range(&first.id, 4, 5).expect("range read");
        assert_eq!(slice, b"quick");
        assert_eq!(
            artifacts.metadata(&first.id).unwrap().byte_count,
            bytes.len()
        );
    }

    #[test]
    fn client_visible_tool_output_redacts_secret_values() {
        let (root, artifacts) = temp_root();
        std::fs::write(
            root.join("settings.txt"),
            "API_TOKEN=raw-secret\nname=visible\nAuthorization: Bearer hidden\n-----BEGIN PRIVATE KEY-----\nprivate-material\n-----END PRIVATE KEY-----",
        )
        .expect("write settings");
        let outcome = ReadOnlyTools::new(&root)
            .run(
                ReadOnlyTool::Read {
                    target: "settings.txt".into(),
                },
                ActionOrigin::User,
                &artifacts,
            )
            .expect("run redacted read");
        let ToolOutcome::Allowed { result } = outcome else {
            panic!("redacted read should be allowed");
        };
        assert!(!result.preview.contains("raw-secret"));
        assert!(!result.preview.contains("Bearer hidden"));
        assert!(!result.preview.contains("private-material"));
        assert!(result.preview.contains("name=visible"));
    }

    #[test]
    fn normalization_preserves_explicit_origin() {
        let (root, _artifacts) = temp_root();
        let tools = ReadOnlyTools::new(root);
        let action = tools.normalize(
            &ReadOnlyTool::Read {
                target: "README.md".into(),
            },
            ActionOrigin::User,
        );
        assert_eq!(action.origin, ActionOrigin::User);
    }
}
