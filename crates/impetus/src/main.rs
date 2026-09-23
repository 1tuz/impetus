use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use impetus_client::HarnessClient;
use uuid::Uuid;

mod daemon;
mod doctor;
mod extension;
mod skills;
mod tui;

#[derive(Parser)]
#[command(name = "impetus")]
#[command(version)]
#[command(about = "Impetus - terminal-first agent harness")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum ComponentsAction {
    /// List the static built-in tool catalog
    List,
    /// Show status for a catalog entry (static; not live IPC health)
    Status {
        /// Component ID to inspect (optional, shows all if omitted)
        component_id: Option<String>,
    },
}

#[derive(Subcommand)]
enum SkillsAction {
    /// List all discovered skills
    List,
    /// Import and display a skill from path
    Import {
        /// Path to skill directory or SKILL.md file
        path: String,
    },
    /// Show details of a named skill
    Show {
        /// Skill name (directory name under ~/.agents/skills/)
        name: String,
    },
}

#[derive(Subcommand)]
enum ExtensionAction {
    /// Dry-run InstallPlan (no filesystem writes)
    Plan {
        /// Extension kind: skill or mcp
        #[arg(value_enum)]
        kind: extension::ExtensionKind,
        /// Path to SKILL.md / skill dir, or MCP config JSON
        path: String,
        /// Target project root (workspace layout; wins over --data-dir)
        #[arg(long)]
        root: Option<String>,
        /// Daemon data root (canonical SoT; default: $IMPETUS_DATA_DIR)
        #[arg(long)]
        data_dir: Option<String>,
        /// Emit JSON instead of human text
        #[arg(long)]
        json: bool,
    },
    /// Plan then apply (ownership + install state)
    Install {
        /// Extension kind: skill or mcp
        #[arg(value_enum)]
        kind: extension::ExtensionKind,
        /// Path to SKILL.md / skill dir, or MCP config JSON
        path: String,
        /// Target project root (workspace layout; wins over --data-dir)
        #[arg(long)]
        root: Option<String>,
        /// Daemon data root (canonical SoT; default: $IMPETUS_DATA_DIR)
        #[arg(long)]
        data_dir: Option<String>,
        /// Emit JSON instead of human text
        #[arg(long)]
        json: bool,
    },
    /// Uninstall by installation_id (ownership proof)
    Remove {
        /// Installation ID from a prior `extension install`
        installation_id: String,
        /// Target project root (workspace layout; wins over --data-dir)
        #[arg(long)]
        root: Option<String>,
        /// Daemon data root (canonical SoT; default: $IMPETUS_DATA_DIR)
        #[arg(long)]
        data_dir: Option<String>,
        /// Emit JSON instead of human text
        #[arg(long)]
        json: bool,
    },
    /// Enable a disabled or unloaded install
    Enable {
        /// Installation ID from a prior `extension install`
        installation_id: String,
        /// Target project root (workspace layout; wins over --data-dir)
        #[arg(long)]
        root: Option<String>,
        /// Daemon data root (canonical SoT; default: $IMPETUS_DATA_DIR)
        #[arg(long)]
        data_dir: Option<String>,
        /// Emit JSON instead of human text
        #[arg(long)]
        json: bool,
    },
    /// Disable install (sideline files; not loaded on restart)
    Disable {
        /// Installation ID from a prior `extension install`
        installation_id: String,
        /// Target project root (workspace layout; wins over --data-dir)
        #[arg(long)]
        root: Option<String>,
        /// Daemon data root (canonical SoT; default: $IMPETUS_DATA_DIR)
        #[arg(long)]
        data_dir: Option<String>,
        /// Emit JSON instead of human text
        #[arg(long)]
        json: bool,
    },
    /// Unload install (sideline files; drop from runtime reload set)
    Unload {
        /// Installation ID from a prior `extension install`
        installation_id: String,
        /// Target project root (workspace layout; wins over --data-dir)
        #[arg(long)]
        root: Option<String>,
        /// Daemon data root (canonical SoT; default: $IMPETUS_DATA_DIR)
        #[arg(long)]
        data_dir: Option<String>,
        /// Emit JSON instead of human text
        #[arg(long)]
        json: bool,
    },
    /// List install states / effective daemon inventory
    List {
        /// Target project root (workspace layout; wins over --data-dir)
        #[arg(long)]
        root: Option<String>,
        /// Daemon data root (canonical SoT; default: $IMPETUS_DATA_DIR)
        #[arg(long)]
        data_dir: Option<String>,
        /// Emit JSON instead of human text
        #[arg(long)]
        json: bool,
    },
    /// Migrate workspace `.impetus/` Enabled installs into daemon SoT
    Migrate {
        /// Legacy project root with `.impetus/install_state.db` (default: cwd)
        #[arg(long)]
        from: Option<String>,
        /// Daemon data root (required unless $IMPETUS_DATA_DIR)
        #[arg(long)]
        data_dir: Option<String>,
        /// Emit JSON instead of human text
        #[arg(long)]
        json: bool,
    },
    /// Report install-state + ownership health (read-only)
    Doctor {
        /// Optional installation ID; omit to check all under --root
        installation_id: Option<String>,
        /// Target project root (workspace layout; wins over --data-dir)
        #[arg(long)]
        root: Option<String>,
        /// Daemon data root (canonical SoT; default: $IMPETUS_DATA_DIR)
        #[arg(long)]
        data_dir: Option<String>,
        /// Emit JSON instead of human text
        #[arg(long)]
        json: bool,
    },
    /// Restore owned paths from recorded source (digest mismatch needs --force)
    Repair {
        /// Installation ID from a prior `extension install`
        installation_id: String,
        /// Overwrite paths whose digest no longer matches ownership (user edits)
        #[arg(long)]
        force: bool,
        /// Target project root (workspace layout; wins over --data-dir)
        #[arg(long)]
        root: Option<String>,
        /// Daemon data root (canonical SoT; default: $IMPETUS_DATA_DIR)
        #[arg(long)]
        data_dir: Option<String>,
        /// Emit JSON instead of human text
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
enum Commands {
    /// Run diagnostics and health checks
    Doctor {
        /// Output in JSON format
        #[arg(long)]
        json: bool,
        /// Perform live network probes (web search backends, internet access)
        #[arg(long)]
        probe_network: bool,
    },
    /// Show the static built-in tool catalog (not a live module registry)
    Components {
        #[command(subcommand)]
        action: ComponentsAction,
    },
    /// Manage Agent Skills extensions
    Skills {
        #[command(subcommand)]
        action: SkillsAction,
    },
    /// Extension lifecycle (plan / install / remove / enable / disable / unload / list / doctor / repair)
    Extension {
        #[command(subcommand)]
        action: ExtensionAction,
    },
    /// Launch interactive TUI (MVP UI)
    Ui,
    /// Create a new session
    Create,
    /// Stream events from a session
    Stream {
        /// Session ID to stream from
        session_id: Uuid,
    },
    /// Cancel a running session
    Cancel {
        /// Session ID to cancel
        session_id: Uuid,
    },
    /// Approve or reject a pending agent action
    Approve {
        /// Session that owns the approval
        session_id: Uuid,
        /// Pending approval ID
        approval_id: Uuid,
        /// Reject the pending action instead of approving it
        #[arg(long)]
        reject: bool,
    },
    /// Send a prompt to a session
    Prompt {
        /// Session ID to prompt
        session_id: Uuid,
        /// The prompt text
        text: String,
    },
    /// Show transient resolved instruction context for a session
    Context {
        /// Session ID to inspect
        session_id: Uuid,
    },
    /// List all sessions
    List,
}

async fn show_components(action: ComponentsAction) -> Result<()> {
    // Static catalog only — no live IPC query of the daemon module registry.
    const BUILTIN: &[(&str, &str)] = &[
        ("bash", "Shell command execution"),
        ("read", "File reading"),
        ("write", "File writing"),
        ("edit", "File editing"),
        ("search", "Repository search"),
    ];

    match action {
        ComponentsAction::List => {
            println!("Built-in tool catalog (static; not a live registry via IPC):\n");
            for (id, description) in BUILTIN {
                println!("  • {id} - {description}");
            }
            println!("\nNote: This command does not query impetusd or loaded modules.");
            println!("Use `impetus doctor` for runtime/subsystem diagnostics.");
        }
        ComponentsAction::Status { component_id } => {
            if let Some(id) = component_id {
                if let Some((_, description)) = BUILTIN.iter().find(|(name, _)| *name == id) {
                    println!("Component: {id}");
                    println!("Description: {description}");
                    println!("Source: static built-in catalog (impetus-core tool names)");
                    println!("Live health: not queried (no IPC)");
                } else {
                    println!("Component '{id}' not in the static built-in catalog");
                    println!("\nUse 'impetus components list' to see the catalog");
                }
            } else {
                println!("Built-in tool catalog summary (static; not live IPC):\n");
                println!(
                    "Catalog entries: {} ({})",
                    BUILTIN.len(),
                    BUILTIN
                        .iter()
                        .map(|(id, _)| *id)
                        .collect::<Vec<_>>()
                        .join(", ")
                );
                println!("Loaded modules / registry: not queried by this command");
                println!("\nUse `impetus doctor` for runtime/subsystem diagnostics.");
            }
        }
    }
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    let socket_path = daemon::discover_socket_path();

    match cli.command {
        Commands::Doctor {
            json,
            probe_network,
        } => {
            // Soft-start so first `doctor` after install does not require manual daemon.
            let _ = daemon::ensure_daemon_running(&socket_path).await;
            doctor::run_diagnostics(&socket_path, json, probe_network).await?;
            return Ok(());
        }
        Commands::Components { action } => {
            // Offline static catalog — does not talk to the daemon.
            show_components(action).await?;
            return Ok(());
        }
        Commands::Skills { ref action } => {
            match action {
                SkillsAction::List => {
                    skills::list_skills().await?;
                }
                SkillsAction::Import { path } => {
                    skills::import_skill(std::path::Path::new(path)).await?;
                }
                SkillsAction::Show { name } => {
                    skills::show_skill(name).await?;
                }
            }
            return Ok(());
        }
        Commands::Extension { ref action } => {
            match action {
                ExtensionAction::Plan {
                    kind,
                    path,
                    root,
                    data_dir,
                    json,
                } => {
                    let root_path = root.as_ref().map(std::path::PathBuf::from);
                    let data_path = data_dir.as_ref().map(std::path::PathBuf::from);
                    extension::plan(
                        *kind,
                        std::path::Path::new(path),
                        root_path.as_deref(),
                        data_path.as_deref(),
                        *json,
                    )
                    .await?;
                }
                ExtensionAction::Install {
                    kind,
                    path,
                    root,
                    data_dir,
                    json,
                } => {
                    let root_path = root.as_ref().map(std::path::PathBuf::from);
                    let data_path = data_dir.as_ref().map(std::path::PathBuf::from);
                    extension::install(
                        *kind,
                        std::path::Path::new(path),
                        root_path.as_deref(),
                        data_path.as_deref(),
                        *json,
                    )
                    .await?;
                }
                ExtensionAction::Remove {
                    installation_id,
                    root,
                    data_dir,
                    json,
                } => {
                    let root_path = root.as_ref().map(std::path::PathBuf::from);
                    let data_path = data_dir.as_ref().map(std::path::PathBuf::from);
                    extension::remove(
                        installation_id,
                        root_path.as_deref(),
                        data_path.as_deref(),
                        *json,
                    )?;
                }
                ExtensionAction::Enable {
                    installation_id,
                    root,
                    data_dir,
                    json,
                } => {
                    let root_path = root.as_ref().map(std::path::PathBuf::from);
                    let data_path = data_dir.as_ref().map(std::path::PathBuf::from);
                    extension::enable(
                        installation_id,
                        root_path.as_deref(),
                        data_path.as_deref(),
                        *json,
                    )?;
                }
                ExtensionAction::Disable {
                    installation_id,
                    root,
                    data_dir,
                    json,
                } => {
                    let root_path = root.as_ref().map(std::path::PathBuf::from);
                    let data_path = data_dir.as_ref().map(std::path::PathBuf::from);
                    extension::disable(
                        installation_id,
                        root_path.as_deref(),
                        data_path.as_deref(),
                        *json,
                    )?;
                }
                ExtensionAction::Unload {
                    installation_id,
                    root,
                    data_dir,
                    json,
                } => {
                    let root_path = root.as_ref().map(std::path::PathBuf::from);
                    let data_path = data_dir.as_ref().map(std::path::PathBuf::from);
                    extension::unload(
                        installation_id,
                        root_path.as_deref(),
                        data_path.as_deref(),
                        *json,
                    )?;
                }
                ExtensionAction::List {
                    root,
                    data_dir,
                    json,
                } => {
                    let root_path = root.as_ref().map(std::path::PathBuf::from);
                    let data_path = data_dir.as_ref().map(std::path::PathBuf::from);
                    extension::list(root_path.as_deref(), data_path.as_deref(), *json)?;
                }
                ExtensionAction::Migrate {
                    from,
                    data_dir,
                    json,
                } => {
                    let from_path = from.as_ref().map(std::path::PathBuf::from);
                    let data_path = data_dir.as_ref().map(std::path::PathBuf::from);
                    extension::migrate(from_path.as_deref(), data_path.as_deref(), *json)?;
                }
                ExtensionAction::Doctor {
                    installation_id,
                    root,
                    data_dir,
                    json,
                } => {
                    let root_path = root.as_ref().map(std::path::PathBuf::from);
                    let data_path = data_dir.as_ref().map(std::path::PathBuf::from);
                    extension::doctor(
                        installation_id.as_deref(),
                        root_path.as_deref(),
                        data_path.as_deref(),
                        *json,
                    )?;
                }
                ExtensionAction::Repair {
                    installation_id,
                    force,
                    root,
                    data_dir,
                    json,
                } => {
                    let root_path = root.as_ref().map(std::path::PathBuf::from);
                    let data_path = data_dir.as_ref().map(std::path::PathBuf::from);
                    extension::repair(
                        installation_id,
                        root_path.as_deref(),
                        data_path.as_deref(),
                        *force,
                        *json,
                    )?;
                }
            }
            return Ok(());
        }
        Commands::Ui => {
            daemon::ensure_daemon_running(&socket_path).await?;
            tui::run(&socket_path).await?;
            return Ok(());
        }
        _ => {}
    }

    // Auto-spawn daemon if not running
    daemon::ensure_daemon_running(&socket_path).await?;

    let client = impetus_client::UnixSocketTransport::connect(&socket_path)
        .await
        .context("Failed to connect to impetusd after ensuring it's running")?;

    match cli.command {
        Commands::Doctor { .. } => unreachable!("handled above"),
        Commands::Components { .. } => unreachable!("handled above"),
        Commands::Skills { .. } => unreachable!("handled above"),
        Commands::Extension { .. } => unreachable!("handled above"),
        Commands::Ui => unreachable!("handled above"),
        Commands::Create => {
            let workspace_root = std::env::current_dir()?.canonicalize()?;
            let response = client
                .request(impetus_core::IpcRequest::CreateSession { workspace_root })
                .await?;
            match response {
                impetus_core::IpcResponse::Session { session_id, status } => {
                    println!("Created session: {session_id}");
                    println!("Status: {status:?}");
                }
                impetus_core::IpcResponse::Error { message, .. } => {
                    bail!("Error creating session: {message}");
                }
                other => bail!("Unexpected response: {other:?}"),
            }
        }
        Commands::Stream { session_id } => {
            let response = client
                .request(impetus_core::IpcRequest::Stream {
                    session_id,
                    after_sequence: 0,
                })
                .await?;
            match response {
                impetus_core::IpcResponse::Events { events, .. } => {
                    println!("Events from session {session_id}:");
                    for event in events {
                        println!("  [{}] {:?}", event.sequence, event.payload);
                    }
                }
                impetus_core::IpcResponse::Error { message, .. } => {
                    bail!("Error streaming: {message}");
                }
                other => bail!("Unexpected response: {other:?}"),
            }
        }
        Commands::Cancel { session_id } => {
            let response = client
                .request(impetus_core::IpcRequest::Cancel { session_id })
                .await?;
            match response {
                impetus_core::IpcResponse::Status { status, .. } => {
                    println!("Session {session_id} cancelled, status: {status:?}");
                }
                impetus_core::IpcResponse::Error { message, .. } => {
                    bail!("Error cancelling: {message}");
                }
                other => bail!("Unexpected response: {other:?}"),
            }
        }
        Commands::Approve {
            session_id,
            approval_id,
            reject,
        } => {
            let response = client
                .request(impetus_core::IpcRequest::ResolveApproval {
                    session_id,
                    approval_id,
                    accepted: !reject,
                })
                .await?;
            match response {
                impetus_core::IpcResponse::ApprovalResolved { .. } => {
                    println!("Approval {approval_id} resolved for session {session_id}");
                }
                impetus_core::IpcResponse::Error { message, .. } => {
                    bail!("Error resolving approval: {message}");
                }
                other => bail!("Unexpected response: {other:?}"),
            }
        }
        Commands::Prompt { session_id, text } => {
            let response = client
                .request(impetus_core::IpcRequest::Prompt {
                    session_id,
                    text,
                    artifact: None,
                    intent: Default::default(),
                })
                .await?;
            match response {
                impetus_core::IpcResponse::Status { status, .. } => {
                    println!("Prompt sent to {session_id}, status: {status:?}");
                }
                impetus_core::IpcResponse::Error { message, .. } => {
                    bail!("Error sending prompt: {message}");
                }
                other => bail!("Unexpected response: {other:?}"),
            }
        }
        Commands::Context { session_id } => {
            let context = client.get_context(session_id).await?;
            for reference in context.references {
                println!(
                    "{:?}: {}",
                    reference.kind,
                    reference.relative_path.display()
                );
            }
            println!("Estimated tokens: {}", context.estimated_tokens.total());
        }
        Commands::List => {
            let response = client
                .request(impetus_core::IpcRequest::ListSessions)
                .await?;
            match response {
                impetus_core::IpcResponse::Sessions { sessions } => {
                    if sessions.is_empty() {
                        println!("No sessions found.");
                    } else {
                        println!("Sessions:");
                        for session in sessions {
                            match (session.parent_session_id, session.fork_sequence) {
                                (Some(parent), Some(seq)) => {
                                    println!(
                                        "  {} (parent={parent}, fork_sequence={seq})",
                                        session.id
                                    );
                                }
                                _ => println!("  {}", session.id),
                            }
                        }
                    }
                }
                impetus_core::IpcResponse::Error { message, .. } => {
                    bail!("Error listing sessions: {message}");
                }
                other => bail!("Unexpected response: {other:?}"),
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_context_session_id() {
        let id = Uuid::new_v4();
        let cli = Cli::try_parse_from(["impetus", "context", &id.to_string()]).unwrap();
        assert!(matches!(cli.command, Commands::Context { session_id } if session_id == id));
    }

    #[test]
    fn parses_rejected_approval() {
        let session_id = Uuid::new_v4();
        let approval_id = Uuid::new_v4();
        let cli = Cli::try_parse_from([
            "impetus",
            "approve",
            &session_id.to_string(),
            &approval_id.to_string(),
            "--reject",
        ])
        .unwrap();
        assert!(matches!(
            cli.command,
            Commands::Approve { reject: true, .. }
        ));
    }

    #[test]
    fn parses_extension_plan_skill() {
        let cli = Cli::try_parse_from([
            "impetus",
            "extension",
            "plan",
            "skill",
            "./demo/SKILL.md",
            "--json",
        ])
        .unwrap();
        assert!(matches!(
            cli.command,
            Commands::Extension {
                action: ExtensionAction::Plan {
                    kind: extension::ExtensionKind::Skill,
                    json: true,
                    ..
                }
            }
        ));
    }

    #[test]
    fn parses_extension_install_mcp_with_root() {
        let cli = Cli::try_parse_from([
            "impetus",
            "extension",
            "install",
            "mcp",
            "./servers/demo.json",
            "--root",
            "/tmp/project",
        ])
        .unwrap();
        match cli.command {
            Commands::Extension {
                action:
                    ExtensionAction::Install {
                        kind: extension::ExtensionKind::Mcp,
                        json: false,
                        root: Some(root),
                        ..
                    },
            } => assert_eq!(root, "/tmp/project"),
            _ => panic!("unexpected command variant"),
        }
    }

    #[test]
    fn parses_extension_remove_with_root() {
        let cli = Cli::try_parse_from([
            "impetus",
            "extension",
            "remove",
            "11111111-2222-3333-4444-555555555555",
            "--root",
            "/tmp/project",
            "--json",
        ])
        .unwrap();
        match cli.command {
            Commands::Extension {
                action:
                    ExtensionAction::Remove {
                        installation_id,
                        json: true,
                        root: Some(root),
                        ..
                    },
            } => {
                assert_eq!(installation_id, "11111111-2222-3333-4444-555555555555");
                assert_eq!(root, "/tmp/project");
            }
            _ => panic!("unexpected command variant"),
        }
    }

    #[test]
    fn parses_extension_migrate() {
        let cli = Cli::try_parse_from([
            "impetus",
            "extension",
            "migrate",
            "--from",
            "/tmp/project",
            "--data-dir",
            "/tmp/data",
            "--json",
        ])
        .unwrap();
        match cli.command {
            Commands::Extension {
                action:
                    ExtensionAction::Migrate {
                        from: Some(from),
                        data_dir: Some(data),
                        json: true,
                    },
            } => {
                assert_eq!(from, "/tmp/project");
                assert_eq!(data, "/tmp/data");
            }
            _ => panic!("unexpected command variant"),
        }
    }

    #[test]
    fn parses_extension_enable_disable_unload_list() {
        let enable = Cli::try_parse_from([
            "impetus",
            "extension",
            "enable",
            "11111111-2222-3333-4444-555555555555",
            "--json",
        ])
        .unwrap();
        assert!(matches!(
            enable.command,
            Commands::Extension {
                action: ExtensionAction::Enable { json: true, .. }
            }
        ));

        let disable = Cli::try_parse_from([
            "impetus",
            "extension",
            "disable",
            "11111111-2222-3333-4444-555555555555",
            "--root",
            "/tmp/project",
        ])
        .unwrap();
        assert!(matches!(
            disable.command,
            Commands::Extension {
                action: ExtensionAction::Disable { root: Some(_), .. }
            }
        ));

        let unload = Cli::try_parse_from([
            "impetus",
            "extension",
            "unload",
            "11111111-2222-3333-4444-555555555555",
        ])
        .unwrap();
        assert!(matches!(
            unload.command,
            Commands::Extension {
                action: ExtensionAction::Unload { .. }
            }
        ));

        let list = Cli::try_parse_from(["impetus", "extension", "list", "--json"]).unwrap();
        assert!(matches!(
            list.command,
            Commands::Extension {
                action: ExtensionAction::List { json: true, .. }
            }
        ));
    }

    #[test]
    fn parses_extension_doctor_all_and_one() {
        let all = Cli::try_parse_from(["impetus", "extension", "doctor", "--json"]).unwrap();
        assert!(matches!(
            all.command,
            Commands::Extension {
                action: ExtensionAction::Doctor {
                    installation_id: None,
                    json: true,
                    ..
                }
            }
        ));

        let one = Cli::try_parse_from([
            "impetus",
            "extension",
            "doctor",
            "11111111-2222-3333-4444-555555555555",
            "--root",
            "/tmp/project",
        ])
        .unwrap();
        match one.command {
            Commands::Extension {
                action:
                    ExtensionAction::Doctor {
                        installation_id: Some(id),
                        json: false,
                        root: Some(root),
                        ..
                    },
            } => {
                assert_eq!(id, "11111111-2222-3333-4444-555555555555");
                assert_eq!(root, "/tmp/project");
            }
            _ => panic!("unexpected command variant"),
        }
    }

    #[test]
    fn parses_extension_repair_with_force() {
        let cli = Cli::try_parse_from([
            "impetus",
            "extension",
            "repair",
            "11111111-2222-3333-4444-555555555555",
            "--force",
            "--root",
            "/tmp/project",
            "--json",
        ])
        .unwrap();
        match cli.command {
            Commands::Extension {
                action:
                    ExtensionAction::Repair {
                        installation_id,
                        force: true,
                        json: true,
                        root: Some(root),
                        ..
                    },
            } => {
                assert_eq!(installation_id, "11111111-2222-3333-4444-555555555555");
                assert_eq!(root, "/tmp/project");
            }
            _ => panic!("unexpected command variant"),
        }
    }
}
