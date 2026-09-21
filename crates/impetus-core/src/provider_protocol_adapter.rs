//! Provider-native protocol adapter boundary and shared tool-call assembler.
//!
//! Target shape from ARCHITECTURE: `ProviderProtocolAdapter` → `StreamEvent`
//! → tool-call assembler → schema validation → policy → execution.
//!
//! Out of scope here: OpenAI Responses API (`/v1/responses`) — separate item.

use crate::{ModelProvider, ProviderError, StreamEvent};
use std::collections::HashMap;

/// Explicit boundary: vendor streaming protocol → typed [`StreamEvent`].
///
/// Implementations own HTTP/SSE framing and vendor-specific deltas.
/// Shared [`ToolCallAssembler`] owns index-keyed tool-call accumulation and
/// `StreamEvent::ToolCall` emission so future protocols (Responses API, etc.)
/// can plug in without duplicating assemblers.
///
/// Credential wrappers (e.g. [`crate::OpenAiNativeAdapter`]) stay outside this
/// trait: they resolve secrets then delegate to a protocol adapter /
/// [`ModelProvider`].
pub trait ProviderProtocolAdapter: ModelProvider {
    /// Stable protocol label (`openai_chat_completions`, `anthropic_messages`, …).
    fn protocol_id(&self) -> &'static str;
}

/// Accumulates streaming tool-call fragments keyed by wire index.
///
/// OpenAI Chat Completions and Anthropic Messages both stream tool calls as
/// partial deltas; this assembler is the shared flush → [`StreamEvent::ToolCall`]
/// path.
#[derive(Debug, Default)]
pub struct ToolCallAssembler {
    slots: HashMap<usize, PendingToolCall>,
}

#[derive(Debug, Default)]
struct PendingToolCall {
    id: Option<String>,
    name: Option<String>,
    arguments: String,
}

impl ToolCallAssembler {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_id(&mut self, index: usize, id: impl Into<String>) {
        self.slots.entry(index).or_default().id = Some(id.into());
    }

    pub fn set_name(&mut self, index: usize, name: impl Into<String>) {
        self.slots.entry(index).or_default().name = Some(name.into());
    }

    pub fn push_arguments_fragment(&mut self, index: usize, fragment: &str) {
        self.slots
            .entry(index)
            .or_default()
            .arguments
            .push_str(fragment);
    }

    /// Apply an OpenAI-style tool-call delta (optional id/name + argument fragment).
    pub fn apply_openai_delta(
        &mut self,
        index: usize,
        id: Option<&str>,
        name: Option<&str>,
        arguments_fragment: Option<&str>,
    ) {
        let slot = self.slots.entry(index).or_default();
        if let Some(id) = id {
            slot.id = Some(id.to_string());
        }
        if let Some(name) = name {
            slot.name = Some(name.to_string());
        }
        if let Some(frag) = arguments_fragment {
            slot.arguments.push_str(frag);
        }
    }

    /// Emit completed tool calls as [`StreamEvent::ToolCall`] and clear slots.
    ///
    /// Empty id/name slots are skipped. Empty argument buffers become `{}`.
    /// Invalid JSON in the argument buffer yields [`ProviderError::MalformedToolCall`].
    pub fn emit_into(
        &mut self,
        on_event: &mut dyn FnMut(StreamEvent) -> Result<(), ProviderError>,
    ) -> Result<(), ProviderError> {
        let mut pending: Vec<_> = self.slots.drain().collect();
        pending.sort_by_key(|(index, _)| *index);
        for (_index, acc) in pending {
            let (Some(id), Some(name)) = (acc.id, acc.name) else {
                continue;
            };
            if id.is_empty() || name.is_empty() {
                continue;
            }
            let arguments = if acc.arguments.is_empty() {
                serde_json::Value::Object(serde_json::Map::new())
            } else {
                serde_json::from_str(&acc.arguments).map_err(|_| {
                    ProviderError::MalformedToolCall(format!(
                        "invalid JSON in tool call arguments for {name}"
                    ))
                })?
            };
            on_event(StreamEvent::ToolCall {
                id,
                name,
                arguments,
            })?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ProviderError;

    #[test]
    fn assembler_default_empty() {
        let mut assembler = ToolCallAssembler::new();
        let mut events = Vec::new();
        assembler
            .emit_into(&mut |ev| {
                events.push(ev);
                Ok(())
            })
            .unwrap();
        assert!(events.is_empty());
    }

    #[test]
    fn assembler_openai_style_fragments_emit_tool_call() {
        let mut assembler = ToolCallAssembler::new();
        assembler.apply_openai_delta(0, Some("call_abc"), Some("get_weather"), Some("{\"city\":"));
        assembler.apply_openai_delta(0, None, None, Some("\"NYC\"}"));

        let mut events = Vec::new();
        assembler
            .emit_into(&mut |ev| {
                events.push(ev);
                Ok(())
            })
            .unwrap();

        assert_eq!(events.len(), 1);
        match &events[0] {
            StreamEvent::ToolCall {
                id,
                name,
                arguments,
            } => {
                assert_eq!(id, "call_abc");
                assert_eq!(name, "get_weather");
                assert_eq!(arguments["city"], "NYC");
            }
            other => panic!("expected ToolCall, got {other:?}"),
        }
    }

    #[test]
    fn assembler_anthropic_style_setters_emit_tool_call() {
        let mut assembler = ToolCallAssembler::new();
        assembler.set_id(1, "toolu_01");
        assembler.set_name(1, "bash");
        assembler.push_arguments_fragment(1, "{\"command\":");
        assembler.push_arguments_fragment(1, "\"ls\"}");

        let mut events = Vec::new();
        assembler
            .emit_into(&mut |ev| {
                events.push(ev);
                Ok(())
            })
            .unwrap();

        assert_eq!(events.len(), 1);
        match &events[0] {
            StreamEvent::ToolCall {
                id,
                name,
                arguments,
            } => {
                assert_eq!(id, "toolu_01");
                assert_eq!(name, "bash");
                assert_eq!(arguments["command"], "ls");
            }
            other => panic!("expected ToolCall, got {other:?}"),
        }
    }

    #[test]
    fn assembler_empty_arguments_become_object() {
        let mut assembler = ToolCallAssembler::new();
        assembler.set_id(0, "call_1");
        assembler.set_name(0, "noop");

        let mut events = Vec::new();
        assembler
            .emit_into(&mut |ev| {
                events.push(ev);
                Ok(())
            })
            .unwrap();

        match &events[0] {
            StreamEvent::ToolCall { arguments, .. } => {
                assert_eq!(arguments, &serde_json::json!({}));
            }
            other => panic!("expected ToolCall, got {other:?}"),
        }
    }

    #[test]
    fn assembler_invalid_json_is_malformed_tool_call() {
        let mut assembler = ToolCallAssembler::new();
        assembler.apply_openai_delta(0, Some("c1"), Some("f"), Some("{bad"));

        let err = assembler
            .emit_into(&mut |_| Ok(()))
            .expect_err("invalid JSON must fail");
        assert!(matches!(err, ProviderError::MalformedToolCall(_)));
    }

    #[test]
    fn assembler_skips_incomplete_slots() {
        let mut assembler = ToolCallAssembler::new();
        assembler.push_arguments_fragment(0, "{}");
        assembler.set_id(1, "only_id");
        assembler.set_name(2, "only_name");

        let mut events = Vec::new();
        assembler
            .emit_into(&mut |ev| {
                events.push(ev);
                Ok(())
            })
            .unwrap();
        assert!(events.is_empty());
    }

    #[test]
    fn assembler_emits_in_index_order() {
        let mut assembler = ToolCallAssembler::new();
        assembler.apply_openai_delta(2, Some("c2"), Some("b"), Some("{}"));
        assembler.apply_openai_delta(0, Some("c0"), Some("a"), Some("{}"));

        let mut ids = Vec::new();
        assembler
            .emit_into(&mut |ev| {
                if let StreamEvent::ToolCall { id, .. } = ev {
                    ids.push(id);
                }
                Ok(())
            })
            .unwrap();
        assert_eq!(ids, vec!["c0".to_string(), "c2".to_string()]);
    }
}
