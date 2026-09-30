//! Offline batch CLI admission (`impetus batch`) — #439/#441/#445 / parent #397.
//!
//! Default provider: mock. Optional local live adapter: `--provider fs` or
//! `$IMPETUS_BATCH_PROVIDER=fs` ([`FsBatchProvider`] filesystem queue — **not** a
//! paid network API). When daemon sock is live and negotiates `offline_batch`,
//! mock submit uses typed IPC so Harness registers plan+executor on
//! `OfflineBatchRegistry`. Sock down / capability absent / `--provider fs` →
//! offline EventStore journal + plan sidecar (manual collect).

use anyhow::{Context, Result, bail};
use clap::Subcommand;
use impetus_client::protocol::OfflineBatchItemSpec;
use impetus_client::{HarnessClient, UnixSocketTransport};
use impetus_core::{
    BatchItemSpec, BatchPlan, DurableOfflineBatchExecutor, Event, EventPayload, EventStore,
    FrozenBatchConfig, FsBatchProvider, MockBatchProvider, OfflineBatchJournal,
    OfflineBatchProviderKind, SessionEvent, SqliteEventStore, admit_batch_for_kind, hash_bytes,
    load_batch_plan, offline_batch_fs_root, persist_batch_plan,
};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use uuid::Uuid;

fn resolve_provider_kind(cli: Option<&str>) -> Result<OfflineBatchProviderKind> {
    let raw = cli
        .map(str::to_string)
        .or_else(|| std::env::var("IMPETUS_BATCH_PROVIDER").ok())
        .unwrap_or_else(|| "mock".into());
    OfflineBatchProviderKind::parse(&raw).with_context(|| {
        format!("unknown batch provider {raw:?}; expected mock or fs (local filesystem)")
    })
}

#[derive(Subcommand)]
pub enum BatchAction {
    /// Admit an offline batch (mock default; `--provider fs` = local FS adapter)
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
        /// Provider: `mock` (default) or `fs` (local filesystem; not paid API).
        /// Also `$IMPETUS_BATCH_PROVIDER` when flag omitted.
        #[arg(long)]
        provider: Option<String>,
        /// Override FS job root (default: `$data_dir/offline_batch_fs`)
        #[arg(long)]
        fs_root: Option<PathBuf>,
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
    /// Collect Ready results into workspace (mock rehydrate or FS disk state)
    Collect {
        #[arg(long)]
        session_id: Uuid,
        batch_id: Uuid,
        #[arg(long)]
        workspace: Option<PathBuf>,
        #[arg(long)]
        data_dir: Option<PathBuf>,
        /// Provider used at submit (`mock` or `fs`); also `$IMPETUS_BATCH_PROVIDER`
        #[arg(long)]
        provider: Option<String>,
        #[arg(long)]
        fs_root: Option<PathBuf>,
        #[arg(long)]
        json: bool,
    },
}

pub async fn run(action: BatchAction) -> Result<()> {
    match action {
        BatchAction::Submit {
            session_id,
            items,
            workspace,
            data_dir,
            model,
            provider,
            fs_root,
            json,
        } => {
            submit(
                session_id,
                items,
                workspace,
                data_dir,
                model,
                provider.as_deref(),
                fs_root,
                json,
            )
            .await
        }
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
            provider,
            fs_root,
            json,
        } => collect(
            session_id,
            batch_id,
            workspace,
            data_dir,
            provider.as_deref(),
            fs_root,
            json,
        ),
    }
}

async fn submit(
    session_id: Option<Uuid>,
    items: Vec<String>,
    workspace: Option<PathBuf>,
    data_dir: Option<PathBuf>,
    model: String,
    provider: Option<&str>,
    fs_root: Option<PathBuf>,
    json: bool,
) -> Result<()> {
    let kind = resolve_provider_kind(provider)?;
    let data_root = resolve_data_root(data_dir)?;
    let workspace_root = resolve_workspace(workspace)?;
    let plan = parse_plan(&items)?;

    // FS admit stays local so job files land under this data_root. Mock may IPC.
    if kind == OfflineBatchProviderKind::Mock
        && let Some(admitted) =
            try_admit_via_daemon(session_id, &workspace_root, &plan, &model, &data_root).await
    {
        let (batch_id, session_id, config_digest, item_count) = admitted?;
        return print_submit(
            batch_id,
            session_id,
            &config_digest,
            item_count,
            &data_root,
            kind.as_label(),
            "daemon_ipc_register",
            json,
        );
    }

    let store = open_or_create_store(&data_root)?;
    let session_id = ensure_session(store.as_ref(), session_id)?;
    let config = FrozenBatchConfig::freeze(kind.as_label(), &model, vec!["temp=0".into()], &plan);
    let batch_id = admit_batch_for_kind(
        store,
        session_id,
        workspace_root,
        plan.clone(),
        None, // offline CLI: journal only; daemon IPC path registers above
        kind,
        Some(&data_root),
        fs_root.as_deref(),
        model,
        vec!["temp=0".into()],
    )
    .with_context(|| format!("admit offline batch ({})", kind.as_label()))?;
    persist_batch_plan(&data_root, batch_id, &plan).context("persist batch plan sidecar")?;
    print_submit(
        batch_id,
        session_id,
        &config.digest,
        plan.items.len(),
        &data_root,
        kind.as_label(),
        "offline_journal",
        json,
    )
}

/// Best-effort daemon admit when sock exists and `offline_batch` is negotiated.
///
/// Returns `None` when sock absent / connect fails / capability missing (caller
/// falls back to offline journal). `Some(Err)` = sock live but admit failed.
async fn try_admit_via_daemon(
    session_id: Option<Uuid>,
    workspace_root: &Path,
    plan: &BatchPlan,
    model: &str,
    data_root: &Path,
) -> Option<Result<(Uuid, Uuid, String, usize)>> {
    let socket_path = crate::daemon::discover_socket_path();
    if !Path::new(&socket_path).exists() {
        return None;
    }
    let client = match UnixSocketTransport::connect(&socket_path).await {
        Ok(client) => client,
        Err(_) => return None,
    };
    if !client
        .negotiated_capabilities()
        .iter()
        .any(|cap| cap == "offline_batch")
    {
        return None;
    }

    Some(admit_via_client(client, session_id, workspace_root, plan, model, data_root).await)
}

async fn admit_via_client(
    client: UnixSocketTransport,
    session_id: Option<Uuid>,
    workspace_root: &Path,
    plan: &BatchPlan,
    model: &str,
    data_root: &Path,
) -> Result<(Uuid, Uuid, String, usize)> {
    let session_id = match session_id {
        Some(id) => {
            client
                .resume_session(id)
                .await
                .with_context(|| format!("attach session {id} for offline batch IPC"))?;
            id
        }
        None => client
            .create_session(workspace_root.to_path_buf())
            .await
            .context("create session for offline batch IPC")?,
    };

    let wire_items: Vec<OfflineBatchItemSpec> = plan
        .items
        .iter()
        .map(|item| OfflineBatchItemSpec {
            item_id: item.item_id.clone(),
            input_hash: item.input_hash.clone(),
            output_relpath: item.output_relpath.clone(),
        })
        .collect();
    let item_count = wire_items.len();
    let (batch_id, config_digest, _) = client
        .admit_offline_batch(session_id, workspace_root.to_path_buf(), wire_items, model)
        .await
        .context("daemon AdmitOfflineBatch")?;
    // Local sidecar for offline collect fallback (daemon also persists under data root).
    persist_batch_plan(data_root, batch_id, plan).context("persist batch plan sidecar")?;
    Ok((batch_id, session_id, config_digest, item_count))
}

fn print_submit(
    batch_id: Uuid,
    session_id: Uuid,
    config_digest: &str,
    item_count: usize,
    data_root: &Path,
    provider: &str,
    admission: &str,
    json: bool,
) -> Result<()> {
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "batch_id": batch_id,
                "session_id": session_id,
                "config_digest": config_digest,
                "provider": provider,
                "item_count": item_count,
                "admission": admission,
            }))?
        );
    } else {
        println!("Offline batch admitted ({provider} provider, {admission})");
        println!("  batch_id:      {batch_id}");
        println!("  session_id:    {session_id}");
        println!("  config_digest: {config_digest}");
        println!("  items:         {item_count}");
        println!(
            "  plan_sidecar:  {}",
            data_root
                .join("offline_batch_plans")
                .join(format!("{batch_id}.json"))
                .display()
        );
        if provider == OfflineBatchProviderKind::Fs.as_label() {
            println!(
                "  fs_jobs:       {}",
                offline_batch_fs_root(data_root).join("jobs").display()
            );
        }
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
    provider: Option<&str>,
    fs_root: Option<PathBuf>,
    json: bool,
) -> Result<()> {
    let kind = resolve_provider_kind(provider)?;
    let data_root = resolve_data_root(data_dir)?;
    let workspace_root = resolve_workspace(workspace)?;
    let plan = load_batch_plan(&data_root, batch_id).context("load batch plan sidecar")?;
    let store = open_store(&data_root)?;
    let journal = OfflineBatchJournal::new(store, session_id);
    let batch_provider: Arc<dyn impetus_core::BatchProvider> = match kind {
        OfflineBatchProviderKind::Mock => {
            Arc::new(MockBatchProvider::rehydrate_ready(batch_id, &plan))
        }
        OfflineBatchProviderKind::Fs => {
            let root = fs_root.unwrap_or_else(|| offline_batch_fs_root(&data_root));
            Arc::new(FsBatchProvider::new(root))
        }
    };
    let exec = DurableOfflineBatchExecutor::new(journal, batch_provider, workspace_root);
    let outcome = exec
        .collect_durable_with_plan(batch_id, &plan)
        .with_context(|| format!("collect offline batch ({})", kind.as_label()))?;

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "batch_id": outcome.batch_id,
                "state": outcome.state,
                "provider": kind.as_label(),
                "item_outcomes": outcome.item_outcomes.iter().map(|(id, o)| {
                    serde_json::json!({ "item_id": id, "outcome": format!("{o:?}") })
                }).collect::<Vec<_>>(),
            }))?
        );
    } else {
        println!(
            "Offline batch collect {batch_id} ({}) → {:?}",
            kind.as_label(),
            outcome.state
        );
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

    #[tokio::test]
    async fn submit_status_collect_roundtrip_mock_only() {
        let data = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let session = Uuid::new_v4();
        let items = vec!["item-a|seed-a|out/a.txt".into()];
        // No sock → offline journal path.
        submit(
            Some(session),
            items,
            Some(workspace.path().to_path_buf()),
            Some(data.path().to_path_buf()),
            "mock-model".into(),
            Some("mock"),
            None,
            false,
        )
        .await
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
            Some("mock"),
            None,
            false,
        )
        .expect("collect");

        let out = workspace.path().join("out/a.txt");
        assert!(out.is_file());
        assert_eq!(std::fs::read_to_string(&out).unwrap(), "result-for-item-a");
    }

    #[tokio::test]
    async fn submit_status_collect_roundtrip_fs_provider() {
        let data = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let session = Uuid::new_v4();
        let items = vec!["item-a|seed-a|out/a.txt".into()];
        submit(
            Some(session),
            items,
            Some(workspace.path().to_path_buf()),
            Some(data.path().to_path_buf()),
            "fs-model".into(),
            Some("fs"),
            None,
            false,
        )
        .await
        .expect("submit fs");

        let store = open_store(data.path()).unwrap();
        let journal = OfflineBatchJournal::new(store, session);
        let records = journal.load_records().unwrap();
        let batch_id = *records.keys().next().unwrap();
        assert!(
            records[&batch_id]
                .provider_job_id
                .as_deref()
                .unwrap_or("")
                .starts_with("fs-job-")
        );

        collect(
            session,
            batch_id,
            Some(workspace.path().to_path_buf()),
            Some(data.path().to_path_buf()),
            Some("fs"),
            None,
            false,
        )
        .expect("collect fs");

        let out = workspace.path().join("out/a.txt");
        assert!(out.is_file());
        assert_eq!(
            std::fs::read_to_string(&out).unwrap(),
            "fs-result-for-item-a"
        );
    }
}
