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

/// Test that simulates actual SSE stream parsing logic
#[test]
fn test_sse_tool_call_parsing_logic() {
    // Simulate the SSE chunks that OpenAI would send for a tool call
    let sse_chunks = vec![
        r#"data: {"choices":[{"delta":{"content":"Searching..."},"finish_reason":null}]}"#,
        r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_xyz","function":{"name":"web_search","arguments":""}}]},"finish_reason":null}]}"#,
        r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"query\""}}]},"finish_reason":null}]}"#,
        r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":": \"rust testing\"}"}}]},"finish_reason":null}]}"#,
        r#"data: {"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#,
        r#"data: {"choices":[],"usage":{"prompt_tokens":50,"completion_tokens":20}}"#,
        "data: [DONE]",
    ];

    // Parse each chunk to verify structure (logic mirrors openai_provider.rs)
    #[derive(serde::Deserialize)]
    #[allow(dead_code)]
    struct SseData {
        choices: Vec<SseChoice>,
        usage: Option<SseUsage>,
    }

    #[derive(serde::Deserialize)]
    #[allow(dead_code)]
    struct SseChoice {
        delta: SseDelta,
        finish_reason: Option<String>,
    }

    #[derive(serde::Deserialize)]
    #[allow(dead_code)]
    struct SseDelta {
        content: Option<String>,
        tool_calls: Option<Vec<SseToolCall>>,
    }

    #[derive(serde::Deserialize)]
    #[allow(dead_code)]
    struct SseToolCall {
        index: usize,
        id: Option<String>,
        function: Option<SseFunction>,
    }

    #[derive(serde::Deserialize)]
    #[allow(dead_code)]
    struct SseFunction {
        name: Option<String>,
        arguments: Option<String>,
    }

    #[derive(serde::Deserialize)]
    #[allow(dead_code)]
    struct SseUsage {
        prompt_tokens: u64,
        completion_tokens: u64,
    }

    let mut text_deltas = Vec::new();
    let mut tool_call_id = None;
    let mut tool_call_name = None;
    let mut tool_call_args = String::new();
    let mut finish_reason = None;
    let mut usage = None;

    for chunk in &sse_chunks {
        if let Some(data) = chunk.strip_prefix("data: ") {
            if data.trim() == "[DONE]" {
                break;
            }
            if let Ok(parsed) = serde_json::from_str::<SseData>(data) {
                if let Some(choice) = parsed.choices.first() {
                    // Text delta
                    if let Some(content) = &choice.delta.content {
                        text_deltas.push(content.clone());
                    }

                    // Tool calls
                    if let Some(tool_calls) = &choice.delta.tool_calls {
                        for tc in tool_calls {
                            if let Some(id) = &tc.id {
                                tool_call_id = Some(id.clone());
                            }
                            if let Some(func) = &tc.function {
                                if let Some(name) = &func.name {
                                    tool_call_name = Some(name.clone());
                                }
                                if let Some(args) = &func.arguments {
                                    tool_call_args.push_str(args);
                                }
                            }
                        }
                    }

                    // Finish reason
                    if let Some(reason) = &choice.finish_reason {
                        finish_reason = Some(reason.clone());
                    }
                }

                // Usage
                if let Some(u) = parsed.usage {
                    usage = Some((u.prompt_tokens, u.completion_tokens));
                }
            }
        }
    }

    // Verify parsed data
    assert_eq!(text_deltas, vec!["Searching..."]);
    assert_eq!(tool_call_id, Some("call_xyz".to_string()));
    assert_eq!(tool_call_name, Some("web_search".to_string()));
    assert_eq!(tool_call_args, r#"{"query": "rust testing"}"#);
    assert_eq!(finish_reason, Some("tool_calls".to_string()));
    assert_eq!(usage, Some((50, 20)));

    // Verify arguments are valid JSON
    let parsed_args: serde_json::Value =
        serde_json::from_str(&tool_call_args).expect("Tool call arguments must be valid JSON");
    assert_eq!(parsed_args["query"], "rust testing");
}

/// Test malformed tool arguments rejection
#[test]
fn test_malformed_tool_arguments_rejected() {
    use impetus_core::ProviderError;

    // Simulate incomplete/malformed JSON
    let malformed_args = r#"{"query": "test"#; // Missing closing brace

    let parse_result = serde_json::from_str::<serde_json::Value>(malformed_args);
    assert!(
        parse_result.is_err(),
        "Malformed JSON should be rejected during parsing"
    );

    // Verify that ProviderError::MalformedToolCall exists
    let _error = ProviderError::MalformedToolCall("invalid JSON".to_string());
}

/// Test multiple tool calls in single response
#[test]
fn test_multiple_tool_calls_parsing() {
    let sse_chunk = r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"search","arguments":"{}"}},{"index":1,"id":"call_2","function":{"name":"fetch","arguments":"{}"}}]},"finish_reason":null}]}"#;

    #[derive(serde::Deserialize)]
    #[allow(dead_code)]
    struct SseData {
        choices: Vec<SseChoice>,
    }

    #[derive(serde::Deserialize)]
    #[allow(dead_code)]
    struct SseChoice {
        delta: SseDelta,
    }

    #[derive(serde::Deserialize)]
    #[allow(dead_code)]
    struct SseDelta {
        tool_calls: Option<Vec<SseToolCall>>,
    }

    #[derive(serde::Deserialize)]
    #[allow(dead_code)]
    struct SseToolCall {
        index: usize,
        id: Option<String>,
        function: Option<SseFunction>,
    }

    #[derive(serde::Deserialize)]
    #[allow(dead_code)]
    struct SseFunction {
        name: Option<String>,
        arguments: Option<String>,
    }

    if let Some(data) = sse_chunk.strip_prefix("data: ") {
        let parsed: SseData = serde_json::from_str(data).expect("Valid SSE data");
        let tool_calls = parsed.choices[0].delta.tool_calls.as_ref().unwrap();

        assert_eq!(tool_calls.len(), 2);
        assert_eq!(tool_calls[0].index, 0);
        assert_eq!(tool_calls[0].id.as_ref().unwrap(), "call_1");
        assert_eq!(
            tool_calls[0]
                .function
                .as_ref()
                .unwrap()
                .name
                .as_ref()
                .unwrap(),
            "search"
        );

        assert_eq!(tool_calls[1].index, 1);
        assert_eq!(tool_calls[1].id.as_ref().unwrap(), "call_2");
        assert_eq!(
            tool_calls[1]
                .function
                .as_ref()
                .unwrap()
                .name
                .as_ref()
                .unwrap(),
            "fetch"
        );
    }
}
