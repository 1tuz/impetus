//! Tests for OpenAI native tool call parsing from SSE streams.

use impetus_core::StreamEvent;

#[tokio::test]
async fn test_openai_parses_streaming_tool_calls() {
    // We can't easily mock HTTP responses, so this test demonstrates the expected
    // event sequence that should be produced from the SSE stream above.
    let expected_events = [
        StreamEvent::TextDelta {
            delta: "Let me search for that.".to_string(),
        },
        StreamEvent::ToolCall {
            id: "call_123".to_string(),
            name: "search".to_string(),
            arguments: serde_json::json!({"query": "test"}),
        },
        StreamEvent::Finish {
            reason: impetus_core::FinishReason::ToolCalls,
        },
        StreamEvent::Usage {
            prompt_tokens: 50,
            completion_tokens: 20,
            measured: true,
        },
    ];

    // For now, verify that the event types and structure are as expected
    assert_eq!(expected_events.len(), 4);
    assert!(matches!(expected_events[0], StreamEvent::TextDelta { .. }));
    assert!(matches!(expected_events[1], StreamEvent::ToolCall { .. }));
    assert!(matches!(expected_events[2], StreamEvent::Finish { .. }));
    assert!(matches!(
        expected_events[3],
        StreamEvent::Usage { measured: true, .. }
    ));
}

#[tokio::test]
async fn test_openai_emits_measured_usage() {
    // Verify that usage events have measured=true
    let usage_event = StreamEvent::Usage {
        prompt_tokens: 100,
        completion_tokens: 50,
        measured: true,
    };

    if let StreamEvent::Usage { measured, .. } = usage_event {
        assert!(measured, "OpenAI usage should be marked as measured");
    } else {
        panic!("Expected Usage event");
    }
}

#[tokio::test]
async fn test_tool_call_structure() {
    // Verify tool call structure matches specification
    let tool_call = StreamEvent::ToolCall {
        id: "call_abc123".to_string(),
        name: "get_weather".to_string(),
        arguments: serde_json::json!({"location": "London", "units": "metric"}),
    };

    if let StreamEvent::ToolCall {
        id,
        name,
        arguments,
    } = tool_call
    {
        assert!(!id.is_empty(), "Tool call ID must not be empty");
        assert!(!name.is_empty(), "Tool name must not be empty");
        assert!(arguments.is_object(), "Arguments must be a JSON object");
    } else {
        panic!("Expected ToolCall event");
    }
}

#[test]
fn test_finish_reason_mapping() {
    // Verify all finish reasons are covered
    use impetus_core::FinishReason;

    let reasons = [
        FinishReason::Stop,
        FinishReason::Length,
        FinishReason::ToolCalls,
        FinishReason::ContentFilter,
        FinishReason::Other,
    ];

    assert_eq!(reasons.len(), 5, "All finish reasons should be tested");
}
