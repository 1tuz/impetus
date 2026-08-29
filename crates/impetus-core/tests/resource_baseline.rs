//! Resource baseline measurements for v0.2 gate.
//!
//! Измеряет RSS, queue size, artifact bytes, restart/cancel latency,
//! context/token accounting для headless harness без нагрузки.

use impetus_core::{
    Action, ActionKind, ActionOrigin, AgentRuntime, EventStore, PolicyEngine, SandboxScope,
    storage::MemoryEventStore,
};
use std::sync::Arc;
use std::time::Instant;

#[test]
fn baseline_idle_session_memory() {
    let store = Arc::new(MemoryEventStore::default());
    let policy = PolicyEngine::new(SandboxScope::local_workspace("."));
    let runtime = AgentRuntime::new(store.clone(), policy);

    let session_id = runtime.session_id();
    let events = store.list(session_id).expect("list events");

    // Idle session: 1 run event, 0 approvals, 0 artifacts
    assert_eq!(events.len(), 1, "idle session has exactly 1 run event");

    // RSS измеряется вручную: cargo test --test resource_baseline -- --nocapture
    // и ps/top во время выполнения. Здесь только структурный baseline.
    println!("Idle session: {} events, 0 artifacts", events.len());
}

#[test]
fn baseline_queue_size() {
    let store = Arc::new(MemoryEventStore::default());
    let policy = PolicyEngine::new(SandboxScope::local_workspace("."));
    let runtime = AgentRuntime::new(store.clone(), policy);

    let session_id = runtime.session_id();
    let events_before = store.list(session_id).expect("list");

    // Добавляем 10 действий через request_action
    for i in 0..10 {
        let action = Action {
            origin: ActionOrigin::Agent,
            kind: ActionKind::ReadFile,
            summary: format!("read file {}", i),
            target: Some(format!("/tmp/file_{}.txt", i)),
        };
        runtime.request_action(action).ok();
    }

    let events_after = store.list(session_id).expect("list after");
    let added = events_after.len() - events_before.len();

    // Действия создают approval request события
    assert!(
        added >= 10,
        "queue grew by at least 10 events (got {})",
        added
    );

    println!(
        "Queue baseline: {} → {} events (+{})",
        events_before.len(),
        events_after.len(),
        added
    );
}

#[test]
fn baseline_artifact_bytes() {
    use impetus_core::DurableArtifactStore;

    let artifacts = DurableArtifactStore::open("target/test-artifacts").expect("open");

    let small = b"Hello, world!";
    let medium = vec![b'x'; 1024]; // 1 KB
    let large = vec![b'y'; 1024 * 1024]; // 1 MB

    let id_small = artifacts.store(small).expect("store small");
    let id_medium = artifacts.store(&medium).expect("store medium");
    let id_large = artifacts.store(&large).expect("store large");

    // Проверяем размер через metadata
    let meta_small = artifacts.metadata(&id_small.id).expect("metadata small").expect("artifact not found");
    let meta_medium = artifacts.metadata(&id_medium.id).expect("metadata medium").expect("artifact not found");
    let meta_large = artifacts.metadata(&id_large.id).expect("metadata large").expect("artifact not found");

    assert_eq!(meta_small.byte_count, small.len());
    assert_eq!(meta_medium.byte_count, medium.len());
    assert_eq!(meta_large.byte_count, large.len());

    println!(
        "Artifact baseline: small={} B, medium={} KB, large={} MB",
        small.len(),
        medium.len() / 1024,
        large.len() / (1024 * 1024)
    );
}

#[test]
fn baseline_restart_latency() {
    let store = Arc::new(MemoryEventStore::default());
    let policy = PolicyEngine::new(SandboxScope::local_workspace("."));

    // Создаём session
    let start_create = Instant::now();
    let runtime = AgentRuntime::new(store.clone(), policy.clone());
    let create_elapsed = start_create.elapsed();
    let session_id = runtime.session_id();

    // Добавляем несколько событий
    for i in 0..5 {
        let action = Action {
            origin: ActionOrigin::Agent,
            kind: ActionKind::ReadFile,
            summary: format!("read {}", i),
            target: Some(format!("/tmp/{}.txt", i)),
        };
        runtime.request_action(action).ok();
    }

    drop(runtime);

    // Attach к существующей session
    let start_attach = Instant::now();
    let _restored = AgentRuntime::attach(store.clone(), policy, session_id).expect("attach");
    let attach_elapsed = start_attach.elapsed();

    println!(
        "Restart latency: create={:?}, attach={:?}",
        create_elapsed, attach_elapsed
    );

    // Baseline: attach должен быть < 100ms для in-memory store
    assert!(
        attach_elapsed.as_millis() < 100,
        "attach latency too high: {:?}",
        attach_elapsed
    );
}

#[test]
fn baseline_cancel_latency() {
    let store = Arc::new(MemoryEventStore::default());
    let policy = PolicyEngine::new(SandboxScope::local_workspace("."));
    let runtime = AgentRuntime::new(store.clone(), policy);

    // Start run
    runtime
        .submit_intent_and_start_run("test intent")
        .expect("start");

    // Cancel
    let start = Instant::now();
    runtime.cancel().expect("cancel");
    let elapsed = start.elapsed();

    println!("Cancel latency: {:?}", elapsed);

    // Baseline: cancel должен быть < 50ms для idle run
    assert!(
        elapsed.as_millis() < 50,
        "cancel latency too high: {:?}",
        elapsed
    );
}

#[test]
fn baseline_context_token_accounting() {
    // Для context/token accounting нужен provider adapter.
    // Здесь только структурный placeholder — реальный baseline
    // добавится после интеграции streaming adapter из шага 1.

    println!("Context/token baseline: deferred until provider integration");
}
