use impetus_core::module::{ModuleDescriptor, ModuleKind, ModulePermissions};
use impetus_core::{
    Action, ActionKind, ActionOrigin, AgentLoop, AgentRuntime, CanonicalModuleKind,
    CanonicalModuleSpec, ContextOptimizer, ContextOptimizerConfig, ContextRunState, EventPayload,
    ExtensionSource, MemoryEventStore, MockProvider, MockProviderItem, PolicyDecision,
    PolicyEngine, ProviderMessage, SandboxScope, ToolCall, ToolOutcomeStatus,
};
use std::collections::HashMap;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

fn text(messages: &[ProviderMessage]) -> String {
    messages
        .iter()
        .map(ProviderMessage::content)
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn stable_prefix_and_order_are_deterministic() {
    let workspace = tempfile::tempdir().expect("workspace");
    std::fs::write(workspace.path().join("AGENTS.md"), "stable project rules")
        .expect("project rules");
    let messages = vec![
        ProviderMessage::user("inspect src/lib.rs"),
        ProviderMessage::assistant("older result"),
        ProviderMessage::user("fix src/lib.rs"),
    ];
    let state = ContextRunState::default();
    let mut optimizer = ContextOptimizer::with_config(
        workspace.path(),
        workspace.path().join("artifacts"),
        ContextOptimizerConfig::default(),
    )
    .expect("optimizer");

    let first = optimizer
        .optimize(&messages, "fix src/lib.rs", &state, Some(4_096))
        .expect("first context");
    let second = optimizer
        .optimize(&messages, "fix src/lib.rs", &state, Some(4_096))
        .expect("second context");

    assert_eq!(first.stable_prefix_hash, second.stable_prefix_hash);
    assert_eq!(first.messages, second.messages);
    assert_eq!(first.messages[0].role(), "system");
    assert!(
        first.messages[0]
            .content()
            .contains("Impetus context protocol")
    );
    let combined = text(&first.messages);
    assert!(combined.find("stable project rules").unwrap() < combined.find("list_files").unwrap());
}

#[test]
fn overflow_drops_cold_then_warm_and_keeps_current_task() {
    let workspace = tempfile::tempdir().expect("workspace");
    let config = ContextOptimizerConfig {
        max_prompt_tokens: 420,
        reserved_response_tokens: 64,
        max_hot_messages: 3,
        max_warm_messages: 3,
        ..ContextOptimizerConfig::default()
    };
    let mut optimizer =
        ContextOptimizer::with_config(workspace.path(), workspace.path().join("artifacts"), config)
            .expect("optimizer");
    let mut messages = Vec::new();
    for index in 0..24 {
        messages.push(ProviderMessage::assistant(format!(
            "old log {index}: {}",
            "cold-output ".repeat(80)
        )));
    }
    messages.push(ProviderMessage::user("current task must survive"));

    let optimized = optimizer
        .optimize(
            &messages,
            "current task must survive",
            &ContextRunState::default(),
            Some(420),
        )
        .expect("optimized context");

    assert!(optimized.telemetry.selected_tokens <= optimized.telemetry.available_tokens);
    assert!(optimized.telemetry.dropped_cold_tokens > 0);
    assert!(optimized.telemetry.dropped_warm_tokens > 0);
    assert!(text(&optimized.messages).contains("current task must survive"));
}

#[test]
fn tool_family_and_instruction_are_lazy_loaded() {
    let workspace = tempfile::tempdir().expect("workspace");
    let skill = workspace.path().join(".impetus/skills/rust-review");
    std::fs::create_dir_all(&skill).expect("skill directory");
    std::fs::write(
        skill.join("SKILL.md"),
        "---\nid: rust-review\nscope: workspace\n---\nUNIQUE_RUST_REVIEW_BODY",
    )
    .expect("skill");
    let messages = vec![ProviderMessage::user("summarize status")];
    let mut optimizer = ContextOptimizer::with_config(
        workspace.path(),
        workspace.path().join("artifacts"),
        ContextOptimizerConfig::default(),
    )
    .expect("optimizer");
    let initial = optimizer
        .optimize(
            &messages,
            "summarize status",
            &ContextRunState::default(),
            None,
        )
        .expect("initial context");
    assert!(!text(&initial.messages).contains("UNIQUE_RUST_REVIEW_BODY"));
    assert!(!text(&initial.messages).contains("web_fetch"));

    let mut state = ContextRunState::default();
    state.request_instruction("rust-review");
    state.request_tool_family("web");
    let loaded = optimizer
        .optimize(&messages, "summarize status", &state, None)
        .expect("loaded context");
    let loaded_text = text(&loaded.messages);
    assert!(loaded_text.contains("UNIQUE_RUST_REVIEW_BODY"));
    assert!(loaded_text.contains("web_fetch"));
    assert!(loaded_text.contains("web_search"));
}

#[test]
fn module_and_mcp_descriptions_are_catalogued_then_lazy_loaded() {
    let workspace = tempfile::tempdir().expect("workspace");
    let mut optimizer = ContextOptimizer::with_config(
        workspace.path(),
        workspace.path().join("artifacts"),
        ContextOptimizerConfig::default(),
    )
    .expect("optimizer");
    optimizer.register_module(ModuleDescriptor {
        id: "rust-index".into(),
        name: "Rust Index".into(),
        version: "1".into(),
        kind: ModuleKind::ToolProvider,
        provides: vec!["symbols".into()],
        requires: vec![],
        capabilities: vec!["repo.symbols".into()],
        permissions: ModulePermissions::default(),
    });
    optimizer.register_extension(CanonicalModuleSpec {
        id: "docs-mcp".into(),
        name: "Documentation MCP".into(),
        version: "1".into(),
        source: ExtensionSource::Mcp,
        kind: CanonicalModuleKind::McpServer,
        capabilities: vec!["docs.search".into()],
        metadata: HashMap::new(),
    });
    let messages = vec![ProviderMessage::user("summarize status")];
    let initial = optimizer
        .optimize(
            &messages,
            "summarize status",
            &ContextRunState::default(),
            None,
        )
        .expect("initial");
    let initial_text = text(&initial.messages);
    assert!(initial_text.contains("rust-index"));
    assert!(initial_text.contains("docs-mcp"));
    assert!(!initial_text.contains("repo.symbols"));
    assert!(!initial_text.contains("docs.search"));

    let mut state = ContextRunState::default();
    for id in ["rust-index", "docs-mcp"] {
        let observation = optimizer
            .handle_control_call(
                &ToolCall {
                    id: format!("load-{id}"),
                    name: "load_component".into(),
                    arguments: serde_json::json!({ "id": id }),
                },
                &messages,
                "summarize status",
                &mut state,
            )
            .expect("context control call");
        assert_eq!(observation.outcome, ToolOutcomeStatus::Success);
        assert!(observation.preview.contains("\"activated\":false"));
    }
    let loaded = optimizer
        .optimize(&messages, "summarize status", &state, None)
        .expect("loaded");
    let loaded_text = text(&loaded.messages);
    assert!(loaded_text.contains("repo.symbols"));
    assert!(loaded_text.contains("docs.search"));
}

#[test]
fn artifact_recovery_is_session_referenced_chunked_and_bounded() {
    let workspace = tempfile::tempdir().expect("workspace");
    let artifact_root = workspace.path().join("artifacts");
    let store = impetus_core::DurableArtifactStore::open(&artifact_root).expect("artifact store");
    let body = format!("BEGIN\n{}\nEND", "artifact-line\n".repeat(10_000));
    let artifact = store.store(body.as_bytes()).expect("store artifact");
    let messages = vec![
        ProviderMessage::user("inspect the referenced artifact"),
        ProviderMessage::tool(
            serde_json::json!({
                "preview": "bounded preview",
                "artifact": artifact,
            })
            .to_string(),
        ),
    ];
    let config = ContextOptimizerConfig {
        max_artifact_chunk_bytes: 2_048,
        max_artifact_summary_tokens: 128,
        ..ContextOptimizerConfig::default()
    };
    let mut optimizer =
        ContextOptimizer::with_config(workspace.path(), &artifact_root, config).expect("optimizer");
    let mut state = ContextRunState::default();
    state.request_artifact(artifact.id.clone(), 0, usize::MAX);

    let optimized = optimizer
        .optimize(
            &messages,
            "inspect the referenced artifact",
            &state,
            Some(4_096),
        )
        .expect("artifact context");
    let combined = text(&optimized.messages);
    assert!(combined.contains(&artifact.id));
    assert!(combined.contains("BEGIN"));
    assert!(!combined.contains("END"));
    assert!(optimized.telemetry.restored_artifact_tokens <= 128);

    let forged_messages = vec![
        ProviderMessage::user("inspect artifact"),
        ProviderMessage::tool(r#"{"artifact":"../../outside"}"#),
    ];
    let mut forged_state = ContextRunState::default();
    let forged = optimizer
        .handle_control_call(
            &ToolCall {
                id: "forged-artifact".into(),
                name: "read_artifact".into(),
                arguments: serde_json::json!({
                    "artifact_id": "../../outside",
                    "start": 0,
                    "length": 100,
                }),
            },
            &forged_messages,
            "inspect artifact",
            &mut forged_state,
        )
        .expect("context control call");
    assert_eq!(forged.outcome, ToolOutcomeStatus::Denied);

    let mut foreign = ContextRunState::default();
    foreign.request_artifact("not-referenced-by-session", 0, 100);
    let foreign_context = optimizer
        .optimize(&messages, "inspect", &foreign, Some(4_096))
        .expect("foreign context");
    assert!(!text(&foreign_context.messages).contains("not-referenced-by-session"));
}

#[test]
fn synthetic_long_session_reports_measurable_savings_and_delta() {
    let workspace = tempfile::tempdir().expect("workspace");
    let mut optimizer = ContextOptimizer::with_config(
        workspace.path(),
        workspace.path().join("artifacts"),
        ContextOptimizerConfig {
            max_prompt_tokens: 2_000,
            reserved_response_tokens: 256,
            ..ContextOptimizerConfig::default()
        },
    )
    .expect("optimizer");
    let mut messages = (0..120)
        .map(|index| {
            ProviderMessage::assistant(format!(
                "historical turn {index}: {}",
                "repeated context ".repeat(100)
            ))
        })
        .collect::<Vec<_>>();
    messages.push(ProviderMessage::user("fix current parser"));
    messages.push(ProviderMessage::assistant(
        "current parser evidence that must be delivered once",
    ));
    let mut state = ContextRunState::default();
    let first = optimizer
        .optimize(&messages, "fix current parser", &state, None)
        .expect("first context");
    assert!(first.telemetry.saved_tokens() > first.telemetry.selected_tokens);
    state.commit(&first);
    messages.push(ProviderMessage::tool("new parser observation"));
    let delta = optimizer
        .optimize(&messages, "fix current parser", &state, None)
        .expect("delta context");
    assert!(text(&delta.messages).contains("new parser observation"));
    assert!(delta.telemetry.delta_omitted_tokens > 0);
}

#[test]
fn lazy_context_selection_does_not_expand_policy_authority() {
    let workspace = tempfile::tempdir().expect("workspace");
    let policy = PolicyEngine::new(SandboxScope::local_workspace(workspace.path()));
    let write = Action {
        origin: ActionOrigin::Agent,
        kind: ActionKind::WriteFile,
        summary: "write file".into(),
        target: Some("result.txt".into()),
    };
    let before = policy.evaluate(&write);
    assert!(matches!(before, PolicyDecision::NeedsApproval { .. }));
    let mut optimizer = ContextOptimizer::with_config(
        workspace.path(),
        workspace.path().join("artifacts"),
        ContextOptimizerConfig::default(),
    )
    .expect("optimizer");
    let mut state = ContextRunState::default();
    state.request_tool_family("mutation");
    let _ = optimizer
        .optimize(
            &[ProviderMessage::user("write result")],
            "write result",
            &state,
            None,
        )
        .expect("context");
    assert_eq!(policy.evaluate(&write), before);
}

#[tokio::test]
async fn agent_loop_optimizes_every_model_call_and_accepts_lazy_tool_discovery() {
    let workspace = tempfile::tempdir().expect("workspace");
    let skill = workspace.path().join(".impetus/skills/manual-guidance");
    std::fs::create_dir_all(&skill).expect("skill directory");
    std::fs::write(
        skill.join("SKILL.md"),
        "---\nid: manual-guidance\nscope: workspace\n---\nUNIQUE_MANUAL_GUIDANCE_BODY",
    )
    .expect("skill");
    let runtime = Arc::new(
        AgentRuntime::create_with_workspace(
            Arc::new(MemoryEventStore::default()),
            PolicyEngine::new(SandboxScope::local_workspace(workspace.path())),
            workspace.path().to_path_buf(),
        )
        .expect("runtime"),
    );
    let run_id = runtime
        .submit_intent_and_start_run("research current release")
        .expect("run");
    let provider = Arc::new(MockProvider::scripted(
        "scripted",
        "test",
        [
            vec![MockProviderItem::Chunk {
                chunk_id: 1,
                text: concat!(
                    "<tool_use><tool_name>discover_tools</tool_name>",
                    "<parameters>{\"family\":\"web\"}</parameters></tool_use>",
                    "<tool_use><tool_name>load_instruction</tool_name>",
                    "<parameters>{\"id\":\"manual-guidance\"}</parameters></tool_use>"
                )
                .into(),
            }],
            vec![MockProviderItem::Chunk {
                chunk_id: 2,
                text: "final answer".into(),
            }],
        ],
    ));
    AgentLoop::new(runtime.clone())
        .execute(
            run_id,
            provider.clone(),
            vec![ProviderMessage::user("research current release")],
            CancellationToken::new(),
        )
        .await
        .expect("agent loop");

    let received = provider.received_messages();
    assert_eq!(received.len(), 2);
    assert!(received.iter().all(|turn| {
        turn.first()
            .is_some_and(|message| message.content().contains("Impetus context protocol"))
    }));
    assert!(text(&received[1]).contains("web_fetch"));
    assert!(!text(&received[0]).contains("UNIQUE_MANUAL_GUIDANCE_BODY"));
    assert!(text(&received[1]).contains("UNIQUE_MANUAL_GUIDANCE_BODY"));
    assert!(runtime.events().expect("events").iter().any(|event| {
        matches!(
            &event.payload,
            EventPayload::Notice(impetus_core::NoticeEvent::Runtime { message })
                if message.starts_with("context_optimizer ")
        )
    }));
}
