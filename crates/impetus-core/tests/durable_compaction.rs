//! Durable compaction in the agent loop: events + structural state.

use impetus_core::{
    AgentLoop, AgentRuntime, BudgetConfig, BudgetEvent, CompactionPolicy, DurableArtifactStore,
    EventPayload, EventStore, MemoryEventStore, MockProvider, MockProviderItem, PolicyEngine,
    ProviderMessage, SandboxScope, SqliteEventStore,
};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn agent_loop_emits_durable_compaction_events_with_structural_state() {
    let workspace = tempfile::tempdir().expect("workspace");
    let artifact_root = tempfile::tempdir().expect("artifacts");
    let artifacts = DurableArtifactStore::open(artifact_root.path()).expect("open artifact store");

    let scope = SandboxScope {
        workspace_root: workspace.path().to_path_buf(),
        allow_network: true,
        allowed_hosts: vec!["example.com".into()],
        allow_web_outbound: false,
        allow_private_network: false,
    };
    let store = Arc::new(MemoryEventStore::default());
    let mut runtime = AgentRuntime::create_with_workspace(
        store.clone(),
        PolicyEngine::new(scope.clone()),
        workspace.path().to_path_buf(),
    )
    .expect("runtime");

    runtime
        .set_budget(BudgetConfig {
            context_limit: Some(1_000),
            compaction: CompactionPolicy {
                threshold_percent: 50,
                compaction_model: None,
                min_turns_before_compaction: 1,
            },
            ..Default::default()
        })
        .expect("budget");

    // Push past compaction threshold without going through the loop.
    runtime.record_turn(600).expect("seed usage");
    assert!(runtime.compaction_needed().is_some());

    let messages = vec![
        ProviderMessage::system("policy lives outside summary"),
        ProviderMessage::user("early turn alpha"),
        ProviderMessage::assistant("reply alpha"),
        ProviderMessage::user("early turn beta"),
        ProviderMessage::assistant("reply beta"),
        ProviderMessage::user("latest question"),
    ];

    let compacted = runtime
        .run_durable_compaction_with_store(messages, Some(&artifacts))
        .expect("compact");

    assert!(
        compacted
            .iter()
            .any(|m| m.content().contains("[context compaction summary]"))
    );
    assert_eq!(
        compacted.last().map(|m| m.content()),
        Some("latest question")
    );

    let events = runtime.events().expect("events");
    let started = events.iter().any(|e| {
        matches!(
            &e.payload,
            EventPayload::Budget(BudgetEvent::CompactionStarted { .. })
        )
    });
    assert!(started, "CompactionStarted must be durable");

    let completed = events.iter().find_map(|e| match &e.payload {
        EventPayload::Budget(BudgetEvent::CompactionCompleted {
            structural,
            summary_artifact,
            compaction_count,
            ..
        }) => Some((
            structural.clone(),
            summary_artifact.clone(),
            *compaction_count,
        )),
        _ => None,
    });
    let (structural, summary_artifact, compaction_count) =
        completed.expect("CompactionCompleted event");
    assert_eq!(compaction_count, 1);
    assert!(summary_artifact.is_some(), "summary stored as artifact");

    let structural = structural.expect("structural snapshot");
    assert_eq!(
        structural.workspace_root,
        runtime.workspace_root().expect("cwd")
    );
    assert!(structural.allow_network);
    assert_eq!(structural.allowed_hosts, vec!["example.com".to_string()]);
    assert!(!structural.allow_web_outbound);
    // Policy capability is typed on the event — not derived from summary prose.
    assert_eq!(
        structural.allow_network,
        runtime.policy().scope().allow_network
    );
    assert!(runtime.compaction_needed().is_none());

    // Agent loop still finishes after compaction gate.
    let mock = Arc::new(MockProvider::new(
        "mock",
        "mock-model",
        vec![MockProviderItem::Chunk {
            chunk_id: 1,
            text: "done".into(),
        }],
    ));
    let runtime = Arc::new(runtime);
    let run_id = runtime.start_run().expect("run");
    AgentLoop::new(runtime.clone())
        .execute(
            run_id,
            mock,
            compacted,
            CancellationToken::new(),
            None,
            impetus_core::StreamOptions::default(),
        )
        .await
        .expect("loop after compaction");
}

#[test]
fn structural_state_survives_sqlite_restart() {
    let workspace = tempfile::tempdir().expect("workspace");
    let db_dir = tempfile::tempdir().expect("db");
    let db_path = db_dir.path().join("events.db");
    let artifact_root = tempfile::tempdir().expect("artifacts");
    let artifacts = DurableArtifactStore::open(artifact_root.path()).expect("artifacts");

    let scope = SandboxScope::local_workspace(workspace.path()).with_network(true);
    let store = SqliteEventStore::open(&db_path).expect("sqlite");
    let mut runtime = AgentRuntime::create_with_workspace(
        store.clone(),
        PolicyEngine::new(scope),
        workspace.path().to_path_buf(),
    )
    .expect("runtime");
    runtime
        .set_budget(BudgetConfig {
            context_limit: Some(500),
            compaction: CompactionPolicy {
                threshold_percent: 40,
                compaction_model: None,
                min_turns_before_compaction: 1,
            },
            ..Default::default()
        })
        .expect("budget");
    runtime.record_turn(400).expect("seed");

    let session_id = runtime.session_id();
    let expected_cwd = runtime.workspace_root().expect("cwd");
    let messages = vec![
        ProviderMessage::user("one"),
        ProviderMessage::assistant("two"),
        ProviderMessage::user("three"),
        ProviderMessage::assistant("four"),
        ProviderMessage::user("five"),
    ];
    runtime
        .run_durable_compaction_with_store(messages, Some(&artifacts))
        .expect("compact");

    drop(runtime);
    drop(store);

    let reopened = SqliteEventStore::open(&db_path).expect("reopen");
    let events = reopened.list(session_id).expect("list");
    let structural = events.iter().find_map(|e| match &e.payload {
        EventPayload::Budget(BudgetEvent::CompactionCompleted { structural, .. }) => {
            structural.clone()
        }
        _ => None,
    });
    let structural = structural.expect("structural after restart");
    assert_eq!(structural.workspace_root, expected_cwd);
    assert!(structural.allow_network);
    assert_eq!(structural.compaction_count, 1);

    // Attach restores workspace from Session events — not from summary text.
    let attached = AgentRuntime::attach(
        reopened,
        PolicyEngine::new(SandboxScope::local_workspace(".")),
        session_id,
    )
    .expect("attach");
    assert_eq!(attached.workspace_root().expect("cwd"), expected_cwd);
}

#[test]
fn worktree_identity_survives_compaction_and_attach() {
    use impetus_core::{WorktreeLifecycleState, WorktreeManager};
    use std::process::Command;

    let workspace = tempfile::tempdir().expect("workspace");
    let repo = tempfile::tempdir().expect("repo");
    let store_dir = tempfile::tempdir().expect("wt store");
    let artifact_root = tempfile::tempdir().expect("artifacts");
    let artifacts = DurableArtifactStore::open(artifact_root.path()).expect("artifacts");

    // Seed a real git repo for WorktreeManager.
    let repo_path = repo.path();
    assert!(
        Command::new("git")
            .args(["init", "-b", "main"])
            .current_dir(repo_path)
            .status()
            .expect("git init")
            .success()
    );
    assert!(
        Command::new("git")
            .args(["config", "user.email", "test@example.com"])
            .current_dir(repo_path)
            .status()
            .expect("email")
            .success()
    );
    assert!(
        Command::new("git")
            .args(["config", "user.name", "Test"])
            .current_dir(repo_path)
            .status()
            .expect("name")
            .success()
    );
    std::fs::write(repo_path.join("README"), b"seed").expect("readme");
    assert!(
        Command::new("git")
            .args(["add", "README"])
            .current_dir(repo_path)
            .status()
            .expect("add")
            .success()
    );
    assert!(
        Command::new("git")
            .args(["commit", "-m", "seed"])
            .current_dir(repo_path)
            .status()
            .expect("commit")
            .success()
    );

    let scope = SandboxScope::local_workspace(workspace.path());
    let event_store = Arc::new(MemoryEventStore::default());
    let mut runtime = AgentRuntime::create_with_workspace(
        event_store.clone(),
        PolicyEngine::new(scope),
        workspace.path().to_path_buf(),
    )
    .expect("runtime");
    let session_id = runtime.session_id();

    let manager = WorktreeManager::open(
        store_dir.path().join("worktrees.db"),
        store_dir.path().join("worktrees"),
    )
    .expect("manager");
    let binding = manager.create(session_id, repo_path).expect("create wt");
    let worktree_id = binding.worktree_id.clone();
    runtime.set_worktree_id(&worktree_id);

    runtime
        .set_budget(BudgetConfig {
            context_limit: Some(500),
            compaction: CompactionPolicy {
                threshold_percent: 40,
                compaction_model: None,
                min_turns_before_compaction: 1,
            },
            ..Default::default()
        })
        .expect("budget");
    runtime.record_turn(400).expect("seed");

    let messages = vec![
        ProviderMessage::user("one"),
        ProviderMessage::assistant("two"),
        ProviderMessage::user("three"),
        ProviderMessage::assistant("four"),
        ProviderMessage::user("five"),
    ];
    runtime
        .run_durable_compaction_with_store(messages, Some(&artifacts))
        .expect("compact");

    let events = runtime.events().expect("events");
    let structural = events
        .iter()
        .find_map(|e| match &e.payload {
            EventPayload::Budget(BudgetEvent::CompactionCompleted { structural, .. }) => {
                structural.clone()
            }
            _ => None,
        })
        .expect("CompactionCompleted structural");
    assert_eq!(
        structural.worktree_id.as_deref(),
        Some(worktree_id.as_str())
    );

    let resolved = manager
        .resolve_after_compaction(session_id, &structural)
        .expect("resolve after compaction");
    assert_eq!(resolved.worktree_id, worktree_id);
    assert_eq!(resolved.state, WorktreeLifecycleState::Active);

    // Attach restores worktree_id from CompactionCompleted structural snapshot.
    let attached = AgentRuntime::attach(
        event_store,
        PolicyEngine::new(SandboxScope::local_workspace(".")),
        session_id,
    )
    .expect("attach");
    assert_eq!(attached.worktree_id(), Some(worktree_id.as_str()));

    let resumed = manager.resume(session_id).expect("resume");
    assert_eq!(resumed.worktree_id, worktree_id);
}
