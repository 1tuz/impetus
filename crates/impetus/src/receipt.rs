//! Flight Recorder CLI (`impetus receipt` / `impetus replay`) — #413 / parent #397.
//!
//! Reads `$IMPETUS_DATA_DIR/events.sqlite3` (or `--db`). Observe-only: never
//! re-executes effects via EffectSeam.

use anyhow::{Context, Result, bail};
use clap::Subcommand;
use impetus_core::{EffectReplayMode, SqliteEventStore, export_receipt, replay_session};
use std::path::{Path, PathBuf};
use uuid::Uuid;

#[derive(Subcommand)]
pub enum ReceiptAction {
    /// Export a secret-free session receipt from EventStore
    Export {
        /// Session id
        session_id: Uuid,
        /// Emit JSON (default human text)
        #[arg(long)]
        json: bool,
        /// Override EventStore sqlite path (default: $IMPETUS_DATA_DIR/events.sqlite3)
        #[arg(long)]
        db: Option<PathBuf>,
        /// Daemon data root (wins when --db omitted)
        #[arg(long)]
        data_dir: Option<PathBuf>,
    },
}

pub fn run_receipt(action: ReceiptAction) -> Result<()> {
    match action {
        ReceiptAction::Export {
            session_id,
            json,
            db,
            data_dir,
        } => export(session_id, json, db, data_dir),
    }
}

pub fn run_replay(
    session_id: Uuid,
    json: bool,
    db: Option<PathBuf>,
    data_dir: Option<PathBuf>,
) -> Result<()> {
    let store = open_store(db, data_dir)?;
    let timeline = replay_session(store.as_ref(), session_id, EffectReplayMode::ObserveOnly)
        .with_context(|| format!("replay session {session_id}"))?;

    if json {
        println!("{}", serde_json::to_string_pretty(&timeline)?);
        return Ok(());
    }

    println!("Replay session {session_id} (observe-only, no EffectSeam execute)");
    println!(
        "  events: {}  sequences: {:?}..{:?}",
        timeline.event_count, timeline.first_sequence, timeline.last_sequence
    );
    for entry in &timeline.entries {
        println!(
            "  [{:>4}] {} — {}",
            entry.sequence, entry.kind, entry.detail
        );
    }
    if !timeline.effect_fences.is_empty() {
        println!("\nEffect fences:");
        for fence in &timeline.effect_fences {
            let target = fence.target_label.as_deref().unwrap_or("-");
            println!(
                "  {}  {}  state={}  digest={}  target={}{}",
                fence.effect_id,
                fence.kind,
                fence.state,
                &fence.args_digest[..fence.args_digest.len().min(12)],
                target,
                if fence.blocks_blind_replay {
                    "  [blocks_blind_replay]"
                } else {
                    ""
                }
            );
        }
    }
    Ok(())
}

fn export(
    session_id: Uuid,
    json: bool,
    db: Option<PathBuf>,
    data_dir: Option<PathBuf>,
) -> Result<()> {
    let store = open_store(db, data_dir)?;
    let receipt = export_receipt(store.as_ref(), session_id)
        .with_context(|| format!("export receipt for session {session_id}"))?;

    if json {
        println!("{}", serde_json::to_string_pretty(&receipt)?);
        return Ok(());
    }

    println!("Flight receipt — session {session_id}");
    println!(
        "  events: {}  sequences: {:?}..{:?}  duration_ms: {:?}",
        receipt.event_count, receipt.first_sequence, receipt.last_sequence, receipt.duration_ms
    );
    if let Some(p) = &receipt.provider_profile {
        println!("  provider: {p}");
    }
    if let Some(outcome) = &receipt.run_outcome {
        println!("  outcome: {outcome}");
    }
    if let Some(t) = receipt.tokens_used {
        println!("  tokens_used: {t}");
    }
    if let Some(t) = receipt.turns_used {
        println!("  turns_used: {t}");
    }
    print_list("tool_calls", &receipt.tool_calls);
    print_list("commands", &receipt.commands);
    print_list("files_read", &receipt.files_read);
    print_list("child_runs", &receipt.child_runs);
    print_list("approvals", &receipt.approvals);
    print_list("denials", &receipt.denials);
    print_list("artifacts", &receipt.artifacts);
    if !receipt.effect_fences.is_empty() {
        println!("  effect_fences:");
        for fence in &receipt.effect_fences {
            let target = fence.target_label.as_deref().unwrap_or("-");
            println!(
                "    - {} {} state={} target={}",
                fence.effect_id, fence.kind, fence.state, target
            );
        }
    }
    Ok(())
}

fn print_list(label: &str, items: &[String]) {
    if items.is_empty() {
        return;
    }
    println!("  {label}:");
    for item in items {
        println!("    - {item}");
    }
}

fn open_store(
    db: Option<PathBuf>,
    data_dir: Option<PathBuf>,
) -> Result<std::sync::Arc<SqliteEventStore>> {
    let path = resolve_db_path(db, data_dir)?;
    if !path.exists() {
        bail!("EventStore not found at {}", path.display());
    }
    SqliteEventStore::open(&path).with_context(|| format!("open EventStore {}", path.display()))
}

fn resolve_db_path(db: Option<PathBuf>, data_dir: Option<PathBuf>) -> Result<PathBuf> {
    if let Some(path) = db {
        return Ok(path);
    }
    let root = data_dir
        .or_else(|| std::env::var_os("IMPETUS_DATA_DIR").map(PathBuf::from))
        .unwrap_or_else(crate::daemon::default_data_root);
    Ok(Path::new(&root).join("events.sqlite3"))
}
