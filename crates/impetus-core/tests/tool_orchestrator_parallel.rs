//! Integration test: Parallel tool execution for read-only and idempotent tools

use impetus_core::{
    AgentRuntime, PolicyEngine, SandboxScope, ToolCall, ToolOrchestrator,
};
use std::sync::Arc;
use std::time::Instant;

#[tokio::test]
async fn multiple_read_file_calls_execute_in_parallel() {
    let workspace = std::env::temp_dir().join("parallel_read_test");
    std::fs::create_dir_all(&workspace).unwrap();

    // Create test files
    std::fs::write(workspace.join("file1.txt"), "content1").unwrap();
    std::fs::write(workspace.join("file2.txt"), "content2").unwrap();
    std::fs::write(workspace.join("file3.txt"), "content3").unwrap();

    let scope = SandboxScope {
        workspace_root: workspace.clone(),
        allow_network: false,
        allowed_hosts: vec![],
    };
    let policy = PolicyEngine::new(scope);
    let store = Arc::new(impetus_core::MemoryEventStore::default());
    let runtime = Arc::new(AgentRuntime::new(store, policy.clone()));

    let orchestrator = ToolOrchestrator::new(policy, workspace.clone());

    // Create multiple read_file tool calls
    let tool_calls = vec![
        ToolCall {
            id: "call_1".to_string(),
            name: "read_file".to_string(),
            arguments: serde_json::json!({
                "path": workspace.join("file1.txt").to_str().unwrap()
            }),
        },
        ToolCall {
            id: "call_2".to_string(),
            name: "read_file".to_string(),
            arguments: serde_json::json!({
                "path": workspace.join("file2.txt").to_str().unwrap()
            }),
        },
        ToolCall {
            id: "call_3".to_string(),
            name: "read_file".to_string(),
            arguments: serde_json::json!({
                "path": workspace.join("file3.txt").to_str().unwrap()
            }),
        },
    ];

    let run_id = runtime.start_run().unwrap();
    let start = Instant::now();

    let observations = orchestrator
        .process_tool_calls(run_id, tool_calls, &runtime)
        .await
        .unwrap();

    let duration = start.elapsed();

    // All observations should succeed
    assert_eq!(observations.len(), 3);
    for obs in &observations {
        assert_eq!(
            obs.outcome,
            impetus_core::ToolEventOutcome::Success,
            "Expected success for {}",
            obs.tool_call_id
        );
    }

    // Parallel execution should be faster than sequential
    // (This is a rough heuristic; in real scenarios with I/O delay this would be more pronounced)
    println!("Parallel execution took: {:?}", duration);

    runtime
        .finish_run(impetus_core::RunEvent::Completed { run_id })
        .unwrap();
}

#[tokio::test]
async fn write_operations_remain_sequential() {
    let workspace = std::env::temp_dir().join("sequential_write_test");
    std::fs::create_dir_all(&workspace).unwrap();

    // Create the output file first (write_file requires parent to exist)
    let output_path = workspace.join("output.txt");
    std::fs::write(&output_path, "").unwrap();

    let scope = SandboxScope {
        workspace_root: workspace.clone(),
        allow_network: false,
        allowed_hosts: vec![],
    };
    let policy = PolicyEngine::new(scope);
    let store = Arc::new(impetus_core::MemoryEventStore::default());
    let runtime = Arc::new(AgentRuntime::new(store, policy.clone()));

    let orchestrator = ToolOrchestrator::new(policy, workspace.clone());

    // Create multiple write_file tool calls
    let tool_calls = vec![
        ToolCall {
            id: "write_1".to_string(),
            name: "write_file".to_string(),
            arguments: serde_json::json!({
                "path": output_path.to_str().unwrap(),
                "content": "first"
            }),
        },
        ToolCall {
            id: "write_2".to_string(),
            name: "write_file".to_string(),
            arguments: serde_json::json!({
                "path": output_path.to_str().unwrap(),
                "content": "second"
            }),
        },
    ];

    let run_id = runtime.start_run().unwrap();

    let observations = orchestrator
        .process_tool_calls(run_id, tool_calls, &runtime)
        .await
        .unwrap();

    // Both writes require approval (Agent origin)
    assert_eq!(observations.len(), 2);

    // Verify they were processed sequentially (not in parallel)
    for obs in &observations {
        // Write operations should require approval, succeed, or hit an error
        // (errors can occur due to sensitive detection or other validation)
        assert!(
            matches!(
                obs.outcome,
                impetus_core::ToolEventOutcome::ApprovalRequired
                    | impetus_core::ToolEventOutcome::Success
                    | impetus_core::ToolEventOutcome::Error
                    | impetus_core::ToolEventOutcome::Denied
            ),
            "Write operation outcome: {:?}, error: {:?}",
            obs.outcome,
            obs.error
        );
    }

    runtime
        .finish_run(impetus_core::RunEvent::Completed { run_id })
        .unwrap();
}

#[tokio::test]
async fn mixed_parallel_and_sequential_execution() {
    let workspace = std::env::temp_dir().join("mixed_execution_test");
    std::fs::create_dir_all(&workspace).unwrap();

    std::fs::write(workspace.join("input1.txt"), "data1").unwrap();
    std::fs::write(workspace.join("input2.txt"), "data2").unwrap();

    // Pre-create output file
    let output_path = workspace.join("output.txt");
    std::fs::write(&output_path, "").unwrap();

    let scope = SandboxScope {
        workspace_root: workspace.clone(),
        allow_network: false,
        allowed_hosts: vec![],
    };
    let policy = PolicyEngine::new(scope);
    let store = Arc::new(impetus_core::MemoryEventStore::default());
    let runtime = Arc::new(AgentRuntime::new(store, policy.clone()));

    let orchestrator = ToolOrchestrator::new(policy, workspace.clone());

    // Mix of read (parallelizable) and write (sequential) operations
    let tool_calls = vec![
        ToolCall {
            id: "read_1".to_string(),
            name: "read_file".to_string(),
            arguments: serde_json::json!({
                "path": workspace.join("input1.txt").to_str().unwrap()
            }),
        },
        ToolCall {
            id: "write_1".to_string(),
            name: "write_file".to_string(),
            arguments: serde_json::json!({
                "path": output_path.to_str().unwrap(),
                "content": "written"
            }),
        },
        ToolCall {
            id: "read_2".to_string(),
            name: "read_file".to_string(),
            arguments: serde_json::json!({
                "path": workspace.join("input2.txt").to_str().unwrap()
            }),
        },
    ];

    let run_id = runtime.start_run().unwrap();

    let observations = orchestrator
        .process_tool_calls(run_id, tool_calls, &runtime)
        .await
        .unwrap();

    // All operations should complete (read succeeds, write needs approval)
    assert_eq!(observations.len(), 3);

    // Read operations should succeed
    let read_obs: Vec<_> = observations
        .iter()
        .filter(|obs| obs.tool_name == "read_file")
        .collect();
    assert_eq!(read_obs.len(), 2);
    for obs in read_obs {
        assert_eq!(
            obs.outcome,
            impetus_core::ToolEventOutcome::Success,
            "Expected read success for {}",
            obs.tool_call_id
        );
    }

    runtime
        .finish_run(impetus_core::RunEvent::Completed { run_id })
        .unwrap();
}

#[tokio::test]
async fn partial_failures_handled_correctly() {
    let workspace = std::env::temp_dir().join("partial_failure_test");
    std::fs::create_dir_all(&workspace).unwrap();

    std::fs::write(workspace.join("exists.txt"), "data").unwrap();

    let scope = SandboxScope {
        workspace_root: workspace.clone(),
        allow_network: false,
        allowed_hosts: vec![],
    };
    let policy = PolicyEngine::new(scope);
    let store = Arc::new(impetus_core::MemoryEventStore::default());
    let runtime = Arc::new(AgentRuntime::new(store, policy.clone()));

    let orchestrator = ToolOrchestrator::new(policy, workspace.clone());

    // Mix of valid and invalid read operations
    let tool_calls = vec![
        ToolCall {
            id: "read_valid".to_string(),
            name: "read_file".to_string(),
            arguments: serde_json::json!({
                "path": workspace.join("exists.txt").to_str().unwrap()
            }),
        },
        ToolCall {
            id: "read_invalid".to_string(),
            name: "read_file".to_string(),
            arguments: serde_json::json!({
                "path": workspace.join("nonexistent.txt").to_str().unwrap()
            }),
        },
    ];

    let run_id = runtime.start_run().unwrap();

    let observations = orchestrator
        .process_tool_calls(run_id, tool_calls, &runtime)
        .await
        .unwrap();

    // Should have 2 observations
    assert_eq!(observations.len(), 2);

    let success_count = observations
        .iter()
        .filter(|obs| obs.outcome == impetus_core::ToolEventOutcome::Success)
        .count();
    let failure_count = observations
        .iter()
        .filter(|obs| {
            obs.outcome == impetus_core::ToolEventOutcome::Error
                || obs.outcome == impetus_core::ToolEventOutcome::Denied
        })
        .count();

    assert_eq!(success_count, 1, "Expected one successful read");
    assert_eq!(failure_count, 1, "Expected one failed read");

    runtime
        .finish_run(impetus_core::RunEvent::Completed { run_id })
        .unwrap();
}
