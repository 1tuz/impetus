//! Structured blocks protocol for Zap terminal.
//!
//! This module renders typed events as structured blocks with metadata for
//! syntax highlighting, approval buttons, and attachment references.

use orbit_core::{ApprovalRequest, Event, EventPayload};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Block types for Zap structured rendering
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Block {
    Diff {
        approval_id: Uuid,
        language: String,
        affected_files: Vec<String>,
        old_text: String,
        new_text: String,
        estimated_lines: usize,
    },
    Approval {
        approval_id: Uuid,
        summary: String,
        action: String,
        affected_files: Vec<String>,
        estimated_scope: String,
    },
    Output {
        text: String,
        is_final: bool,
    },
    Attachment {
        attachment_id: Uuid,
        name: String,
        size_bytes: usize,
        content_type: String,
    },
    Status {
        state: String,
        detail: Option<String>,
    },
    Error {
        message: String,
    },
}

/// Render event as structured block
pub fn render_event_block(event: &Event) -> Option<Block> {
    match &event.payload {
        EventPayload::Approval(approval_event) => match approval_event {
            orbit_core::ApprovalEvent::Requested { request } => {
                Some(render_approval_block(request))
            }
            orbit_core::ApprovalEvent::Resolved { .. } => None,
        },
        EventPayload::Agent(agent_event) => match agent_event {
            orbit_core::AgentEvent::Chunk { text, .. } => Some(Block::Output {
                text: text.clone(),
                is_final: false,
            }),
            orbit_core::AgentEvent::Final { text, .. } => Some(Block::Output {
                text: text.clone(),
                is_final: true,
            }),
        },
        EventPayload::Backend(backend_event) => {
            use orbit_core::BackendEvent;
            match backend_event {
                BackendEvent::ProviderHealthy { profile } => Some(Block::Status {
                    state: "healthy".to_string(),
                    detail: Some(profile.clone()),
                }),
                BackendEvent::ProviderDegraded { profile, reason } => Some(Block::Status {
                    state: "degraded".to_string(),
                    detail: Some(format!("{}: {}", profile, reason)),
                }),
                BackendEvent::ProviderUnavailable { profile, reason } => Some(Block::Error {
                    message: format!("Provider {} unavailable: {}", profile, reason),
                }),
                BackendEvent::KeychainUnavailable { reason } => Some(Block::Error {
                    message: format!("Keychain unavailable: {}", reason),
                }),
                BackendEvent::TokenExpiryWarning {
                    profile,
                    expires_in_seconds,
                } => Some(Block::Status {
                    state: "warning".to_string(),
                    detail: Some(format!(
                        "{} token expires in {}s",
                        profile, expires_in_seconds
                    )),
                }),
                _ => None,
            }
        }
        _ => None,
    }
}

fn render_approval_block(request: &ApprovalRequest) -> Block {
    let affected_files = extract_affected_files(request);
    let estimated_scope = estimate_scope(&affected_files);

    Block::Approval {
        approval_id: request.id,
        summary: request.action.summary.clone(),
        action: format!("{:?}", request.action.kind),
        affected_files,
        estimated_scope,
    }
}

fn extract_affected_files(_request: &ApprovalRequest) -> Vec<String> {
    // ponytail: parse approval payload → affected files later
    vec![]
}

fn estimate_scope(files: &[String]) -> String {
    match files.len() {
        0 => "unknown".to_string(),
        1 => "single file".to_string(),
        n if n <= 5 => format!("{} files", n),
        n => format!("{} files (large change)", n),
    }
}

/// Serialize block as JSON for Zap structured protocol
#[allow(dead_code)]
pub fn serialize_block(block: &Block) -> String {
    serde_json::to_string(block).unwrap_or_else(|_| "{}".to_string())
}

/// Render block with visual separators (fallback for non-structured terminals)
pub fn render_block_text(block: &Block) -> String {
    match block {
        Block::Approval {
            summary,
            affected_files,
            estimated_scope,
            ..
        } => {
            let files_str = if affected_files.is_empty() {
                "unknown".to_string()
            } else {
                affected_files.join(", ")
            };
            format!(
                "┌─ Approval Required ─────────────────────\n\
                 │ {}\n\
                 │ Scope: {}\n\
                 │ Files: {}\n\
                 └─────────────────────────────────────────",
                summary, estimated_scope, files_str
            )
        }
        Block::Output { text, is_final } => {
            let marker = if *is_final { "[Final]" } else { "" };
            let lines = text
                .lines()
                .map(|l| format!("│ {}", l))
                .collect::<Vec<_>>()
                .join("\n");
            format!(
                "┌─ Agent {} ───────────────────────────\n\
                 {}\n\
                 └─────────────────────────────────────────",
                marker, lines
            )
        }
        Block::Status { state, detail } => {
            let detail_str = detail.as_deref().unwrap_or("");
            format!("● {} {}", state, detail_str)
        }
        Block::Error { message } => {
            format!("✗ Error: {}", message)
        }
        Block::Diff { language, .. } => {
            format!("┌─ Diff ({}) ───────────────────────", language)
        }
        Block::Attachment {
            name, size_bytes, ..
        } => {
            format!("📎 {} ({} bytes)", name, size_bytes)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn block_serialization_roundtrip() {
        let block = Block::Status {
            state: "running".to_string(),
            detail: Some("task in progress".to_string()),
        };
        let json = serialize_block(&block);
        let parsed: Block = serde_json::from_str(&json).unwrap();
        assert!(matches!(parsed, Block::Status { .. }));
    }

    #[test]
    fn estimate_scope_categories() {
        assert_eq!(estimate_scope(&[]), "unknown");
        assert_eq!(estimate_scope(&["file.rs".to_string()]), "single file");
        assert_eq!(
            estimate_scope(&["a.rs".to_string(), "b.rs".to_string()]),
            "2 files"
        );
    }
}
