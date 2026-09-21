//! OpenAI Responses API (`/v1/responses`) wire helpers.
//!
//! Subset: text deltas, function-call assembly via [`ToolCallAssembler`],
//! usage + finish on `response.completed`. No network I/O here.

use crate::{FinishReason, ProviderError, ProviderMessage, StreamEvent, ToolCallAssembler};
use serde::Deserialize;

/// Control flow after applying one Responses SSE `data:` JSON payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResponsesStreamAction {
    Continue,
    /// Stream finished successfully (`response.completed`).
    Completed,
    /// Provider reported failure (`response.failed`).
    Failed,
}

/// Responses API `tools` array from [`crate::builtin_tool_schemas`].
///
/// Flat function shape (not Chat Completions nested `function` object).
pub fn openai_responses_tools_payload() -> serde_json::Value {
    let tools: Vec<serde_json::Value> = crate::builtin_tool_schemas()
        .iter()
        .map(|schema| {
            serde_json::json!({
                "type": "function",
                "name": schema.name,
                "description": schema.description,
                "parameters": schema.parameters.clone(),
            })
        })
        .collect();
    serde_json::Value::Array(tools)
}

/// Map harness messages to Responses `input` (role/content items).
pub fn build_responses_input(messages: &[ProviderMessage]) -> serde_json::Value {
    let items: Vec<serde_json::Value> = messages
        .iter()
        .map(|msg| {
            let role = match msg.role() {
                "system" | "user" | "assistant" => msg.role(),
                _ => "user",
            };
            serde_json::json!({
                "role": role,
                "content": msg.content(),
            })
        })
        .collect();
    serde_json::Value::Array(items)
}

/// Apply one Responses SSE JSON object to assembler / StreamEvent sink.
pub fn apply_responses_sse_data(
    data: &str,
    tool_calls: &mut ToolCallAssembler,
    saw_function_call: &mut bool,
    on_event: &mut dyn FnMut(StreamEvent) -> Result<(), ProviderError>,
) -> Result<ResponsesStreamAction, ProviderError> {
    let parsed: ResponsesSseEvent =
        serde_json::from_str(data).map_err(|_| ProviderError::MalformedStream)?;

    match parsed.event_type.as_str() {
        "response.output_text.delta" => {
            if let Some(delta) = parsed.delta.filter(|d| !d.is_empty()) {
                on_event(StreamEvent::TextDelta { delta })?;
            }
            Ok(ResponsesStreamAction::Continue)
        }
        "response.output_item.added" => {
            if let Some(item) = &parsed.item
                && item.type_field == "function_call"
            {
                *saw_function_call = true;
                let index = parsed.output_index.unwrap_or(0);
                let id = item
                    .call_id
                    .clone()
                    .or_else(|| item.id.clone())
                    .unwrap_or_default();
                let name = item.name.clone().unwrap_or_default();
                tool_calls.set_id(index, id);
                tool_calls.set_name(index, name);
                if let Some(args) = item.arguments.as_deref().filter(|a| !a.is_empty()) {
                    tool_calls.push_arguments_fragment(index, args);
                }
            }
            Ok(ResponsesStreamAction::Continue)
        }
        "response.function_call_arguments.delta" => {
            let index = parsed.output_index.unwrap_or(0);
            if let Some(delta) = parsed.delta.as_deref().filter(|d| !d.is_empty()) {
                tool_calls.push_arguments_fragment(index, delta);
            }
            Ok(ResponsesStreamAction::Continue)
        }
        "response.completed" => {
            tool_calls.emit_into(on_event)?;
            if let Some(usage) = parsed.response.as_ref().and_then(|r| r.usage.as_ref()) {
                on_event(StreamEvent::Usage {
                    prompt_tokens: usage.input_tokens.unwrap_or(0),
                    completion_tokens: usage.output_tokens.unwrap_or(0),
                    measured: true,
                })?;
            }
            let reason = if *saw_function_call {
                FinishReason::ToolCalls
            } else {
                FinishReason::Stop
            };
            on_event(StreamEvent::Finish { reason })?;
            Ok(ResponsesStreamAction::Completed)
        }
        "response.failed" => Ok(ResponsesStreamAction::Failed),
        _ => Ok(ResponsesStreamAction::Continue),
    }
}

#[derive(Deserialize)]
struct ResponsesSseEvent {
    #[serde(rename = "type")]
    event_type: String,
    #[serde(default)]
    delta: Option<String>,
    #[serde(default)]
    output_index: Option<usize>,
    #[serde(default)]
    item: Option<ResponsesOutputItem>,
    #[serde(default)]
    response: Option<ResponsesBody>,
}

#[derive(Deserialize)]
struct ResponsesOutputItem {
    #[serde(rename = "type")]
    type_field: String,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    call_id: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    arguments: Option<String>,
}

#[derive(Deserialize)]
struct ResponsesBody {
    #[serde(default)]
    usage: Option<ResponsesUsage>,
}

#[derive(Deserialize)]
struct ResponsesUsage {
    #[serde(default)]
    input_tokens: Option<u64>,
    #[serde(default)]
    output_tokens: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn responses_tools_payload_is_flat_function_shape() {
        let tools = openai_responses_tools_payload();
        let arr = tools.as_array().expect("array");
        assert!(!arr.is_empty());
        assert_eq!(arr[0]["type"], "function");
        assert!(arr[0]["name"].is_string());
        assert!(arr[0]["parameters"].is_object());
        assert!(arr[0].get("function").is_none());
    }

    #[test]
    fn build_responses_input_maps_roles() {
        let input = build_responses_input(&[
            ProviderMessage::system("sys"),
            ProviderMessage::user("hi"),
            ProviderMessage::assistant("yo"),
        ]);
        let arr = input.as_array().unwrap();
        assert_eq!(arr[0]["role"], "system");
        assert_eq!(arr[1]["role"], "user");
        assert_eq!(arr[2]["content"], "yo");
    }

    #[test]
    fn fixture_text_delta_emits_stream_event() {
        let json = r#"{"type":"response.output_text.delta","item_id":"msg_1","output_index":0,"content_index":0,"delta":"Hi"}"#;
        let mut assembler = ToolCallAssembler::new();
        let mut saw = false;
        let mut events = Vec::new();
        let action = apply_responses_sse_data(json, &mut assembler, &mut saw, &mut |ev| {
            events.push(ev);
            Ok(())
        })
        .unwrap();
        assert_eq!(action, ResponsesStreamAction::Continue);
        assert_eq!(events.len(), 1);
        match &events[0] {
            StreamEvent::TextDelta { delta } => assert_eq!(delta, "Hi"),
            other => panic!("expected TextDelta, got {other:?}"),
        }
    }

    #[test]
    fn fixture_function_call_chunks_assemble_tool_call() {
        let added = r#"{"type":"response.output_item.added","output_index":0,"item":{"id":"fc_1","type":"function_call","status":"in_progress","call_id":"call_abc","name":"get_weather","arguments":""}}"#;
        let delta1 = r#"{"type":"response.function_call_arguments.delta","item_id":"fc_1","output_index":0,"delta":"{\"city\":"}"#;
        let delta2 = r#"{"type":"response.function_call_arguments.delta","item_id":"fc_1","output_index":0,"delta":"\"NYC\"}"}"#;
        let completed = r#"{"type":"response.completed","response":{"id":"resp_1","status":"completed","usage":{"input_tokens":10,"output_tokens":5}}}"#;

        let mut assembler = ToolCallAssembler::new();
        let mut saw = false;
        let mut events = Vec::new();
        let mut on_event = |ev: StreamEvent| {
            events.push(ev);
            Ok(())
        };

        assert_eq!(
            apply_responses_sse_data(added, &mut assembler, &mut saw, &mut on_event).unwrap(),
            ResponsesStreamAction::Continue
        );
        assert!(saw);
        assert_eq!(
            apply_responses_sse_data(delta1, &mut assembler, &mut saw, &mut on_event).unwrap(),
            ResponsesStreamAction::Continue
        );
        assert_eq!(
            apply_responses_sse_data(delta2, &mut assembler, &mut saw, &mut on_event).unwrap(),
            ResponsesStreamAction::Continue
        );
        assert_eq!(
            apply_responses_sse_data(completed, &mut assembler, &mut saw, &mut on_event).unwrap(),
            ResponsesStreamAction::Completed
        );

        assert!(
            events
                .iter()
                .any(|e| matches!(e, StreamEvent::ToolCall { name, .. } if name == "get_weather"))
        );
        assert!(events.iter().any(|e| matches!(
            e,
            StreamEvent::Finish {
                reason: FinishReason::ToolCalls
            }
        )));
        assert!(events.iter().any(|e| matches!(
            e,
            StreamEvent::Usage {
                prompt_tokens: 10,
                completion_tokens: 5,
                measured: true
            }
        )));
    }

    #[test]
    fn fixture_completed_without_tools_finishes_stop() {
        let completed = r#"{"type":"response.completed","response":{"status":"completed","usage":{"input_tokens":1,"output_tokens":2}}}"#;
        let mut assembler = ToolCallAssembler::new();
        let mut saw = false;
        let mut events = Vec::new();
        assert_eq!(
            apply_responses_sse_data(completed, &mut assembler, &mut saw, &mut |ev| {
                events.push(ev);
                Ok(())
            })
            .unwrap(),
            ResponsesStreamAction::Completed
        );
        assert!(events.iter().any(|e| matches!(
            e,
            StreamEvent::Finish {
                reason: FinishReason::Stop
            }
        )));
    }

    #[test]
    fn fixture_failed_returns_failed_action() {
        let failed = r#"{"type":"response.failed","response":{"status":"failed"}}"#;
        let mut assembler = ToolCallAssembler::new();
        let mut saw = false;
        assert_eq!(
            apply_responses_sse_data(failed, &mut assembler, &mut saw, &mut |_| Ok(())).unwrap(),
            ResponsesStreamAction::Failed
        );
    }
}
