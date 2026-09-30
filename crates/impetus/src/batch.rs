//! Offline batch CLI admission (`impetus batch`) — #439 / parent #397.
//!
//! Mock provider only — no live network. Journals to EventStore; plan sidecar
//! under `$IMPETUS_DATA_DIR/offline_batch_plans/`. When Harness holds
//! `OfflineBatchRegistry` (daemon), in-process admit registers for poll tick;
//! this CLI path is offline journal + collect (Partial vs live IPC register).

use anyhow::{Context, Result, bail};
use clap::Subcommand;
use impetus_core::{
    BatchItemSpec, BatchPlan, DurableOfflineBatchExecutor, Event, EventPayload, EventStore,
    FrozenBatchConfig, MockBatchProvider, OfflineBatchJournal, SessionEvent, SqliteEventStore,
    admit_mock_batch, hash_bytes, load_batch_plan, persist_batch_plan,
};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use uuid::Uuid;

#[derive(Subcommand)]
pub enum BatchAction {
    /// Admit a mock offline batch (journal + plan sidecar; no live network)
    Submit {
        /// Session that owns the batch journal events (created if missing; omit to mint new)
        #[arg(long)]
        session_id: Option<Uuid>,
        /// Batch item as `item_id|input_hash_or_seed|output_relpath` (repeatable).
        /// If input looks like `sha256:…` it is used as-is; else hashed as seed bytes.
        #[arg(long = "item", required = true)]
        items: Vec<String>,
        /// Workspace root for collect writes (default: cwd)
        #[arg(long)]
        workspace: Option<PathBuf>,
        /// Daemon data root (default: $IMPETUS_DATA_DIR)
        #[arg(long)]
        data_dir: Option<PathBuf>,
        /// Model label frozen into config digest (default: mock-model)
        #[arg(long, default_value = "mock-model")]
        model: String,
        /// Emit JSON
        #[arg(long)]
        json: bool,
    },
    /// Show journal lifecycle for a batch id
    Status {
        #[arg(long)]
        session_id: Uuid,
        batch_id: Uuid,
        #[arg(long)]
        data_dir: Option<PathBuf>,
        #[arg(long)]
        json: bool,
    },
    /// Collect Ready mock results into workspace (rehydrates mock provider)
    Collect {
        #[arg(long)]
        session_id: Uuid,
        batch_id: Uuid,
        #[arg(long)]
        workspace: Option<PathBuf>,
        #[arg(long)]
        data_dir: Option<PathBuf>,
        #[arg(long)]
        json: bool,
    },
}

pub fn run(action: BatchAction) -> Result<()> {
    match action {
        BatchAction::Submit {
            session_id,
            items,
            workspace,
            data_dir,
            model,
            json,
        } => submit(session_id, items, workspace, data_dir, model, json),
        BatchAction::Status {
            session_id,
            batch_id,
            data_dir,
            json,
        } => status(session_id, batch_id, data_dir, json),
        BatchAction::Collect {
            session_id,
            batch_id,
            workspace,
            data_dir,
            json,
        } => collect(session_id, batch_id, workspace, data_dir, json),
    }
}

fn submit(
    session_id: Option<Uuid>,
    items: Vec<String>,
    workspace: Option<PathBuf>,
    data_dir: Option<PathBuf>,
    model: String,
    json: bool,
) -> Result<()> {
    let data_root = resolve_data_root(data_dir)?;
    let workspace_root = resolve_workspace(workspace)?;
    let plan = parse_plan(&items)?;
    let store = open_or_create_store(&data_root)?;
    let session_id = ensure_session(store.as_ref(), session_id)?;
    let config = FrozenBatchConfig::freeze("mock", &model, vec!["temp=0".into()], &plan);
    let batch_id = admit_mock_batch(
        store,
        session_id,
        workspace_root,
        plan.clone(),
        None, // offline CLI: journal only; daemon Harness registers when wired in-process
        "mock",
        model,
        vec!["temp=0".into()],
    )
    .context("admit mock offline batch")?;
    persist_batch_plan(&data_root, batch_id, &plan).context("persist batch plan sidecar")?;

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "batch_id": batch_id,
                "session_id": session_id,
                "config_digest": config.digest,
                "provider": "mock",
                "item_count": plan.items.len(),
                "admission": "offline_journal",
            }))?
        );
    } else {
        println!("Offline batch admitted (mock provider, offline journal)");
        println!("  batch_id:      {batch_id}");
        println!("  session_id:    {session_id}");
        println!("  config_digest: {}", config.digest);
        println!("  items:         {}", plan.items.len());
        println!(
            "  plan_sidecar:  {}",
            data_root
                .join("offline_batch_plans")
                .join(format!("{batch_id}.json"))
                .display()
        );
    }
    Ok(())
}

fn status(session_id: Uuid, batch_id: Uuid, data_dir: Option<PathBuf>, json: bool) -> Result<()> {
    let data_root = resolve_data_root(data_dir)?;
    let store = open_store(&data_root)?;
    let journal = OfflineBatchJournal::new(store, session_id);
    let record = journal
        .record_for(batch_id)
        .context("load batch journal")?
        .with_context(|| format!("batch {batch_id} not found in session {session_id}"))?;

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "batch_id": record.batch_id,
                "state": record.state,
                "config_digest": record.config_digest,
                "provider_job_id": record.provider_job_id,
                "delivered_items": record.delivered_items.iter().collect::<Vec<_>>(),
            }))?
        );
    } else {
        println!("Offline batch {batch_id}");
        println!("  state:           {:?}", record.state);
        println!("  config_digest:   {}", record.config_digest);
        println!(
            "  provider_job_id: {}",
            record.provider_job_id.as_deref().unwrap_or("-")
        );
        if !record.delivered_items.is_empty() {
            let mut items: Vec<_> = record.delivered_items.iter().collect();
            items.sort();
            println!("  delivered:       {}", items.len());
            for id in items {
                println!("    - {id}");
            }
        }
    }
    Ok(())
}

fn collect(
    session_id: Uuid,
    batch_id: Uuid,
    workspace: Option<PathBuf>,
    data_dir: Option<PathBuf>,
    json: bool,
) -> Result<()> {
    let data_root = resolve_data_root(data_dir)?;
    let workspace_root = resolve_workspace(workspace)?;
    let plan = load_batch_plan(&data_root, batch_id).context("load batch plan sidecar")?;
    let store = open_store(&data_root)?;
    let journal = OfflineBatchJournal::new(store, session_id);
    let provider = Arc::new(MockBatchProvider::rehydrate_ready(batch_id, &plan));
    let exec = DurableOfflineBatchExecutor::new(journal, provider, workspace_root);
    let outcome = exec
        .collect_durable_with_plan(batch_id, &plan)
        .context("collect mock offline batch")?;

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "batch_id": outcome.batch_id,
                "state": outcome.state,
                "item_outcomes": outcome.item_outcomes.iter().map(|(id, o)| {
                    serde_json::json!({ "item_id": id, "outcome": format!("{o:?}") })
                }).collect::<Vec<_>>(),
            }))?
        );
    } else {
        println!("Offline batch collect {batch_id} → {:?}", outcome.state);
        for (id, item_outcome) in &outcome.item_outcomes {
            println!("  [{id}] {item_outcome:?}");
        }
    }
    Ok(())
}

fn parse_plan(items: &[String]) -> Result<BatchPlan> {
    let mut parsed = Vec::with_capacity(items.len());
    for raw in items {
        let parts: Vec<&str> = raw.splitn(3, '|').collect();
        if parts.len() != 3 {
            bail!("invalid --item {raw:?}; expected item_id|input_hash_or_seed|output_relpath");
        }
        let item_id = parts[0].to_string();
        if item_id.is_empty() {
            bail!("--item item_id must be non-empty");
        }
        let input = parts[1];
        let input_hash = if input.starts_with("sha256:") {
            input.to_string()
        } else {
            hash_bytes(input.as_bytes())
        };
        let output_relpath = parts[2].to_string();
        if output_relpath.is_empty() || Path::new(&output_relpath).is_absolute() {
            bail!("--item output_relpath must be non-empty relative path");
        }
        parsed.push(BatchItemSpec {
            item_id,
            input_hash,
            output_relpath,
        });
    }
    Ok(BatchPlan { items: parsed })
}

fn resolve_data_root(data_dir: Option<PathBuf>) -> Result<PathBuf> {
    Ok(data_dir
        .or_else(|| std::env::var_os("IMPETUS_DATA_DIR").map(PathBuf::from))
        .unwrap_or_else(crate::daemon::default_data_root))
}

fn resolve_workspace(workspace: Option<PathBuf>) -> Result<PathBuf> {
    match workspace {
        Some(path) => Ok(path),
        None => Ok(std::env::current_dir()?.canonicalize()?),
    }
}

fn open_or_create_store(data_root: &Path) -> Result<Arc<SqliteEventStore>> {
    std::fs::create_dir_all(data_root)
        .with_context(|| format!("create data dir {}", data_root.display()))?;
    let path = data_root.join("events.sqlite3");
    SqliteEventStore::open(&path).with_context(|| format!("open EventStore {}", path.display()))
}

fn open_store(data_root: &Path) -> Result<Arc<SqliteEventStore>> {
    let path = data_root.join("events.sqlite3");
    if !path.exists() {
        bail!("EventStore not found at {}", path.display());
    }
    SqliteEventStore::open(&path).with_context(|| format!("open EventStore {}", path.display()))
}

/// Mint a session or seed a caller-provided id into Sqlite sessions table.
fn ensure_session(store: &dyn EventStore, session_id: Option<Uuid>) -> Result<Uuid> {
    match session_id {
        None => store
            .create_session()
            .context("create session for offline batch"),
        Some(id) => {
            let existing = store
                .list(id)
                .with_context(|| format!("inspect session {id}"))?;
            if !existing.is_empty() {
                return Ok(id);
            }
            let event = Event::new(id, 1, EventPayload::Session(SessionEvent::Created));
            store
                .append(&event)
                .with_context(|| format!("seed session {id}"))?;
            Ok(id)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use impetus_core::BatchLifecycleState;

    #[test]
    fn parse_plan_hashes_seed_and_keeps_sha() {
        let plan = parse_plan(&[
            "a|seed-a|out/a.txt".into(),
            "b|sha256:deadbeef|out/b.txt".into(),
        ])
        .unwrap();
        assert_eq!(plan.items.len(), 2);
        assert!(plan.items[0].input_hash.starts_with("sha256:"));
        assert_eq!(plan.items[1].input_hash, "sha256:deadbeef");
    }

    #[test]
    fn submit_status_collect_roundtrip_mock_only() {
        let data = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let session = Uuid::new_v4();
        let items = vec!["item-a|seed-a|out/a.txt".into()];
        submit(
            Some(session),
            items,
            Some(workspace.path().to_path_buf()),
            Some(data.path().to_path_buf()),
            "mock-model".into(),
            false,
        )
        .expect("submit");

        let store = open_store(data.path()).unwrap();
        let journal = OfflineBatchJournal::new(store, session);
        let records = journal.load_records().unwrap();
        assert_eq!(records.len(), 1);
        let batch_id = *records.keys().next().unwrap();
        assert_eq!(records[&batch_id].state, BatchLifecycleState::Submitted);

        collect(
            session,
            batch_id,
            Some(workspace.path().to_path_buf()),
            Some(data.path().to_path_buf()),
            false,
        )
        .expect("collect");

        let out = workspace.path().join("out/a.txt");
        assert!(out.is_file());
        assert_eq!(std::fs::read_to_string(&out).unwrap(), "result-for-item-a");
    }
}
