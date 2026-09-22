use anyhow::{Context, Result, bail};
use impetus_acp_gateway::AcpProfile;
use impetus_core::{
    CredentialResolver, CredentialStrategy, Harness, IpcErrorCode, IpcRequest, IpcResponse,
    MAX_IPC_LINE_BYTES, NoCredentialResolver, OpenAiProvider, OpenAiRetryBudget, PolicyConfig,
    PolicyEngine, ProviderError, ProviderProfile, SandboxScope, SqliteEventStore,
    build_explore_spawn_bridge_for_harness, load_daemon_hook_prefilter, load_daemon_mcp_runtime,
    load_daemon_policy_store, open_daemon_worktree_manager,
};
use std::collections::BTreeSet;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};

#[cfg(test)]
use impetus_core::RunEvent;

#[tokio::main]
async fn main() -> Result<()> {
    let socket_path = socket_path()?;
    let data_root = data_root()?;
    if socket_path.exists() {
        bail!(
            "refusing to replace existing socket: {}",
            socket_path.display()
        );
    }
    let parent = socket_path
        .parent()
        .context("socket path must have a parent directory")?;
    std::fs::create_dir_all(parent).context("create harness data directory")?;
    std::fs::create_dir_all(&data_root).context("create harness event-store directory")?;
    let store = SqliteEventStore::open(data_root.join("events.sqlite3"))?;
    let harness = Arc::new(configured_harness(
        store,
        &data_root,
        std::env::args_os().skip(1),
    )?);
    spawn_artifact_gc_loop(impetus_core::default_artifact_root());
    let listener = UnixListener::bind(&socket_path).context("bind harness Unix socket")?;
    set_socket_permissions(&socket_path)?;
    loop {
        let (stream, _) = listener.accept().await.context("accept harness client")?;
        let harness = harness.clone();
        tokio::spawn(async move {
            let _ = serve_client(stream, harness).await;
        });
    }
}

/// Startup + interval age-based GC for DurableArtifactStore. Errors log only.
fn spawn_artifact_gc_loop(artifact_root: PathBuf) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(impetus_core::ARTIFACT_GC_INTERVAL);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;
            match impetus_core::run_artifact_gc(&artifact_root, impetus_core::ARTIFACT_GC_RETENTION)
            {
                Ok(removed) if removed > 0 => {
                    eprintln!("impetusd: artifact GC removed {removed} entries");
                }
                Ok(_) => {}
                Err(error) => {
                    eprintln!("impetusd: artifact GC failed (continuing): {error}");
                }
            }
        }
    });
}

/// Direct providers and ACP agents are enabled only by an explicit daemon-start profile file.
/// The file is deserialized into a deny-unknown-fields DTO, so a raw token
/// cannot be silently accepted as configuration.
///
/// Usage:
///   impetusd                                  # mock provider only
///   impetusd --policy-config PATH             # load PolicyConfig JSON at startup
///   impetusd --provider-profile PATH          # OpenAI-compatible direct provider
///   impetusd --acp-profile PATH               # External ACP agent (Codex, Cursor, etc.)
///
/// PolicyConfig resolution order: `--policy-config PATH` → `IMPETUS_POLICY_CONFIG`
/// → `$IMPETUS_DATA_DIR/policy.json` (optional; missing = empty overrides).
fn configured_harness(
    store: Arc<dyn impetus_core::EventStore>,
    data_root: &Path,
    cli_args: impl IntoIterator<Item = OsString>,
) -> Result<Harness> {
    let mut arguments = cli_args.into_iter().peekable();
    let mut explicit_policy_path: Option<PathBuf> = None;

    if arguments
        .peek()
        .and_then(|flag| flag.to_str())
        .is_some_and(|flag| flag == "--policy-config")
    {
        arguments.next();
        let path = arguments.next().context("--policy-config requires PATH")?;
        explicit_policy_path = Some(PathBuf::from(path));
    }

    let policy = startup_policy(explicit_policy_path.as_deref())?;

    let harness = match arguments.next() {
        None => Harness::new(store, policy),
        Some(flag) => {
            let flag_str = flag.to_str().context("invalid UTF-8 in command flag")?;
            match flag_str {
                "--provider-profile" => {
                    let profile_path = arguments
                        .next()
                        .context("--provider-profile requires PATH")?;
                    if arguments.next().is_some() {
                        bail!(
                            "usage: impetusd [--policy-config PATH] [--provider-profile PATH | --acp-profile PATH]"
                        );
                    }
                    let profile_bytes =
                        std::fs::read(profile_path).context("read provider profile")?;
                    let profile: ProviderProfile = serde_json::from_slice(&profile_bytes).context(
                        "provider profile must contain only the documented non-secret fields",
                    )?;
                    let provider = OpenAiProvider::new(profile, OpenAiRetryBudget::default())
                        .map_err(|error| anyhow::anyhow!(error.to_string()))?;
                    Harness::with_openai_provider_and_resolver(
                        store,
                        policy,
                        provider,
                        provider_credential_resolver()?,
                    )
                }
                "--acp-profile" => {
                    let profile_path = arguments.next().context("--acp-profile requires PATH")?;
                    if arguments.next().is_some() {
                        bail!(
                            "usage: impetusd [--policy-config PATH] [--provider-profile PATH | --acp-profile PATH]"
                        );
                    }
                    let profile_bytes = std::fs::read(profile_path).context("read acp profile")?;
                    let profile: AcpProfile = serde_json::from_slice(&profile_bytes).context(
                        "acp profile must contain only the documented non-secret fields",
                    )?;
                    profile.validate().context("invalid acp profile")?;

                    let config = profile
                        .to_agent_config()
                        .context("invalid acp launch config")?;

                    Harness::with_acp_gateway(
                        store,
                        policy,
                        config,
                        profile.auth_method_id,
                        profile.id,
                        profile.display_name,
                    )
                }
                "--policy-config" => bail!("--policy-config must appear before provider/acp flags"),
                _ => bail!(
                    "usage: impetusd [--policy-config PATH] [--provider-profile PATH | --acp-profile PATH]"
                ),
            }
        }
    };

    wire_daemon_runtime(harness, data_root)
}

/// Attach production Explore spawn + WorktreeManager + optional MCP/hook/policy/workflow.
///
/// WorktreeManager: `{data_root}/worktrees.sqlite3` + `{data_root}/worktrees/`.
/// Open failure is fail-closed (daemon start aborts) so Git IPC does not silently
/// resolve cwd to the workspace root without managed-worktree preference.
fn wire_daemon_runtime(harness: Harness, data_root: &Path) -> Result<Harness> {
    let explore = build_explore_spawn_bridge_for_harness(
        data_root,
        harness.provider_registry(),
        harness.default_provider_id(),
        harness.policy(),
        harness.store(),
    )
    .context("wire Explore spawn bridge")?;
    let harness = harness.with_explore_spawn(explore);

    let worktrees =
        open_daemon_worktree_manager(data_root).context("open WorktreeManager under data root")?;
    let harness = harness.with_worktree_manager(worktrees);

    let hook_prefilter =
        load_daemon_hook_prefilter(data_root).context("load hook prefilter catalog")?;
    let harness = harness.with_hook_prefilter(hook_prefilter);

    let harness = match load_daemon_policy_store(data_root).context("load policy store")? {
        Some(store) => harness.with_policy_store(Arc::new(store)),
        None => harness,
    };

    let harness = harness
        .with_provider_steer_rewrite()
        .context("wire provider steer rewrite")?;

    let child_store = Arc::new(
        impetus_core::ChildResultStore::open(data_root.join("child_results.sqlite3"))
            .context("open child result store for workflow")?,
    );
    let workflow = Arc::new(
        impetus_core::WorkflowRuntime::with_parent_events(
            child_store,
            Arc::new(impetus_core::ProcessRoleChildExecutor::new()),
            Arc::new(impetus_core::ReadOnlyExploreExecutor::new(
                impetus_core::default_artifact_root(),
            )),
            Some(harness.store()),
        )
        .context("build workflow runtime")?,
    );
    let harness = harness.with_workflow_runtime(workflow);

    let mcp_runtime = load_daemon_mcp_runtime(data_root).context("load MCP autoload config")?;
    if mcp_runtime.registered_ids().is_empty() {
        Ok(harness)
    } else {
        Ok(harness.with_tool_providers(Arc::new(tokio::sync::Mutex::new(mcp_runtime))))
    }
}

/// Build the daemon PolicyEngine from an optional explicit path or conventional defaults.
fn startup_policy(explicit: Option<&Path>) -> Result<PolicyEngine> {
    let path = if let Some(path) = explicit {
        path.to_path_buf()
    } else if let Some(env_path) = std::env::var_os("IMPETUS_POLICY_CONFIG") {
        PathBuf::from(env_path)
    } else {
        data_root()?.join("policy.json")
    };

    let config = if explicit.is_some() || std::env::var_os("IMPETUS_POLICY_CONFIG").is_some() {
        // Explicit path must exist and parse — refuse start on bad config.
        PolicyConfig::load_from_path(&path)
            .with_context(|| format!("load PolicyConfig from {}", path.display()))?
    } else {
        PolicyConfig::load_optional(&path)
            .with_context(|| format!("load optional PolicyConfig from {}", path.display()))?
    };

    Ok(PolicyEngine::with_config(
        SandboxScope::local_workspace("."),
        config,
    ))
}

fn env_truthy(name: &str) -> bool {
    std::env::var(name)
        .map(|value| matches!(value.as_str(), "1" | "true" | "yes" | "on"))
        .unwrap_or(false)
}

/// CI and scripted runs must not open the Keychain GUI or block on authorization.
fn is_noninteractive_env() -> bool {
    env_truthy("CI") || env_truthy("IMPETUS_NONINTERACTIVE")
}

fn provider_credential_resolver() -> Result<Arc<dyn CredentialResolver>> {
    match std::env::var("IMPETUS_CREDENTIAL_BACKEND").as_deref() {
        Ok("mock") => Ok(Arc::new(NoCredentialResolver)),
        Ok("keychain") | Err(_) => Ok(Arc::new(MacosKeychainResolver)),
        Ok(other) => {
            bail!("unknown IMPETUS_CREDENTIAL_BACKEND `{other}` (expected `mock` or `keychain`)")
        }
    }
}

/// The daemon owns the macOS Keychain lookup. The resolver returns only a
/// transient request credential and intentionally suppresses platform errors,
/// so neither a Keychain detail nor a credential can enter an event or log.
struct MacosKeychainResolver;

impl CredentialResolver for MacosKeychainResolver {
    fn resolve(&self, profile: &ProviderProfile) -> Result<Option<String>, ProviderError> {
        let CredentialStrategy::KeychainReference { service, account } =
            &profile.credential_strategy
        else {
            return Ok(None);
        };
        read_keychain_credential(service, account).map(Some)
    }
}

type KeychainPasswordFetcher = fn(&str, &str) -> Result<Vec<u8>, ()>;

#[cfg(target_os = "macos")]
fn platform_keychain_fetch(service: &str, account: &str) -> Result<Vec<u8>, ()> {
    security_framework::passwords::get_generic_password(service, account).map_err(|_| ())
}

#[cfg(not(target_os = "macos"))]
fn platform_keychain_fetch(_service: &str, _account: &str) -> Result<Vec<u8>, ()> {
    Err(())
}

fn read_keychain_credential(service: &str, account: &str) -> Result<String, ProviderError> {
    read_keychain_credential_with(
        service,
        account,
        platform_keychain_fetch,
        is_noninteractive_env(),
    )
}

fn read_keychain_credential_with(
    service: &str,
    account: &str,
    fetch: KeychainPasswordFetcher,
    noninteractive: bool,
) -> Result<String, ProviderError> {
    if noninteractive {
        return Err(ProviderError::MissingCredential);
    }
    let bytes = fetch(service, account).map_err(|_| ProviderError::MissingCredential)?;
    String::from_utf8(bytes).map_err(|_| ProviderError::MissingCredential)
}

async fn serve_client(stream: UnixStream, harness: Arc<Harness>) -> Result<()> {
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);
    let mut negotiated = None::<BTreeSet<String>>;
    let mut subscription = None;
    let mut notification_receiver: Option<tokio::sync::broadcast::Receiver<(uuid::Uuid, u64)>> =
        None;
    loop {
        tokio::select! {
            result = async {
                match notification_receiver.as_mut() {
                    Some(receiver) => receiver.recv().await.ok(),
                    None => std::future::pending().await,
                }
            }, if subscription.is_some() => {
                let Some((notified_session_id, _notified_sequence)) = result else {
                    continue;
                };
                let (session_id, mut after_sequence) = subscription.expect("checked above");
                if notified_session_id != session_id {
                    continue;
                }
                // Drain until empty: Stream may return a size-capped batch; remaining
                // events must still be pushed without waiting for another append.
                loop {
                    match harness.handle(IpcRequest::Stream {
                        session_id,
                        after_sequence,
                    }) {
                        IpcResponse::Events { events, .. } if !events.is_empty() => {
                            after_sequence = events
                                .last()
                                .map(|last| last.sequence)
                                .expect("non-empty events");
                            subscription = Some((session_id, after_sequence));
                            write_response(
                                &mut writer,
                                &IpcResponse::Events { session_id, events },
                            )
                            .await?;
                        }
                        IpcResponse::Events { .. } => break,
                        error @ IpcResponse::Error { .. } => {
                            write_response(&mut writer, &error).await?;
                            return Ok(());
                        }
                        _ => unreachable!("stream request returns events or error"),
                    }
                }
            }
            read = read_bounded_line(&mut reader) => {
                let line = match read? {
                    LineRead::Eof => return Ok(()),
                    LineRead::TooLarge => {
                        write_response(&mut writer, &IpcResponse::Error {
                        code: IpcErrorCode::InvalidRequest,
                        message: "request exceeds 64 KiB".into(),
                        }).await?;
                        return Ok(());
                    }
                    LineRead::Line(line) => line,
                };
                let response = match serde_json::from_slice::<IpcRequest>(&line) {
                    Ok(request @ IpcRequest::Hello { .. }) => {
                        let response = harness.handle(request);
                        match &response {
                            IpcResponse::Hello { capabilities, .. } => {
                                negotiated = Some(capabilities.iter().cloned().collect());
                            }
                            IpcResponse::Incompatible { .. } => {
                                write_response(&mut writer, &response).await?;
                                return Ok(());
                            }
                            _ => unreachable!("hello returns hello or incompatible"),
                        }
                        response
                    }
                    Ok(request) => {
                        let Some(capabilities) = negotiated.as_ref() else {
                            write_response(&mut writer, &IpcResponse::Error {
                                code: IpcErrorCode::InvalidRequest,
                                message: "successful hello is required before requests".into(),
                            }).await?;
                            continue;
                        };
                        let required = required_capability(&request);
                        if !capabilities.contains(required) {
                            IpcResponse::Error {
                                code: IpcErrorCode::Unavailable,
                                message: format!("capability `{required}` was not negotiated"),
                            }
                        } else {
                            let requested_subscription = match &request {
                                IpcRequest::Subscribe { session_id, after_sequence } => {
                                    Some((*session_id, *after_sequence))
                                }
                                _ => None,
                            };
                            let response = harness.handle(request);
                            if matches!(response, IpcResponse::Subscribed { .. }) {
                                subscription = requested_subscription;
                                // Initialize notification receiver on first subscription
                                if notification_receiver.is_none() {
                                    notification_receiver = Some(harness.store().subscribe_notifications());
                                }
                            }
                            response
                        }
                    }
                    Err(error) => IpcResponse::Error {
                            code: IpcErrorCode::InvalidRequest,
                            message: error.to_string(),
                    },
                };
                write_response(&mut writer, &response).await?;
            }
        }
    }
}

async fn write_response(
    writer: &mut tokio::net::unix::OwnedWriteHalf,
    response: &IpcResponse,
) -> Result<()> {
    writer
        .write_all(serde_json::to_string(response)?.as_bytes())
        .await?;
    writer.write_all(b"\n").await?;
    writer.flush().await?;
    Ok(())
}

fn required_capability(request: &IpcRequest) -> &'static str {
    match request {
        IpcRequest::Hello { .. } => unreachable!("hello is negotiated separately"),
        IpcRequest::CreateSession { .. } => "session_create",
        IpcRequest::Attach { .. } => "session_attach",
        IpcRequest::ListSessions => "session_list",
        IpcRequest::ForkSession { .. } => "session_fork",
        IpcRequest::CreateCheckpoint { .. }
        | IpcRequest::ListCheckpoints { .. }
        | IpcRequest::RestoreCheckpoint { .. } => "session_checkpoint",
        IpcRequest::Stream { .. } => "event_stream",
        IpcRequest::Prompt { .. } => "prompt",
        IpcRequest::Context { .. } => "context",
        IpcRequest::Cancel { .. } => "cancel",
        IpcRequest::Tool { .. } => "tool",
        IpcRequest::Subscribe { .. } => "subscribe",
        IpcRequest::ResolveApproval { .. } => "resolve_approval",
        IpcRequest::GetAttachment { .. } => "get_attachment",
        IpcRequest::GetApprovalDetail { .. } => "get_approval_detail",
        IpcRequest::BeginArtifactUpload { .. }
        | IpcRequest::AppendArtifactChunk { .. }
        | IpcRequest::FinishArtifactUpload { .. }
        | IpcRequest::AbortArtifactUpload { .. } => "artifact_upload",
        IpcRequest::ReadArtifact { .. }
        | IpcRequest::GetArtifactMetadata { .. }
        | IpcRequest::ReadArtifactRange { .. } => "artifact_read",
        IpcRequest::Diagnostics => "diagnostics",
        IpcRequest::GotoDefinition { .. } => "coding_definition",
        IpcRequest::Hover { .. } => "coding_hover",
        IpcRequest::SetExecutionMode { .. } | IpcRequest::GetExecutionMode { .. } => {
            "execution_mode"
        }
        IpcRequest::ReloadPolicyConfig { .. } => "reload_policy_config",
        IpcRequest::ReloadPolicyStore { .. } | IpcRequest::GetPolicyStore => "reload_policy_store",
        IpcRequest::ListChildRuns { .. } | IpcRequest::GetChildRun { .. } => "list_child_runs",
        IpcRequest::StartWorkflow { .. }
        | IpcRequest::CancelWorkflow { .. }
        | IpcRequest::AdvanceWorkflow { .. } => "workflow_control",
        IpcRequest::ListWorkspaceDir { .. } => "workspace_list_dir",
        IpcRequest::StatWorkspaceFile { .. } => "workspace_stat_file",
        IpcRequest::ReadWorkspaceFile { .. } => "workspace_read_file",
        IpcRequest::SearchWorkspaceFiles { .. } => "workspace_search_files",
        IpcRequest::GetRepositoryState { .. }
        | IpcRequest::ListBranches { .. }
        | IpcRequest::GetCurrentBranch { .. }
        | IpcRequest::CreateBranch { .. }
        | IpcRequest::SwitchBranch { .. }
        | IpcRequest::GitStatus { .. }
        | IpcRequest::ListChangedFiles { .. }
        | IpcRequest::GetDiff { .. }
        | IpcRequest::GetFileDiff { .. } => "git",
        IpcRequest::PtyStart { .. }
        | IpcRequest::PtyAttach { .. }
        | IpcRequest::PtyInput { .. }
        | IpcRequest::PtyOutput { .. }
        | IpcRequest::PtyResize { .. }
        | IpcRequest::PtyDetach { .. }
        | IpcRequest::PtyTerminate { .. }
        | IpcRequest::PtyStatus { .. } => "pty",
        IpcRequest::ListMcpServers => "list_mcp",
        IpcRequest::ListModels => "list_models",
    }
}

enum LineRead {
    Eof,
    Line(Vec<u8>),
    TooLarge,
}

async fn read_bounded_line<R>(reader: &mut R) -> std::io::Result<LineRead>
where
    R: AsyncBufRead + Unpin,
{
    let mut output = Vec::new();
    loop {
        let (chunk, complete) = {
            let available = reader.fill_buf().await?;
            if available.is_empty() {
                if output.is_empty() {
                    return Ok(LineRead::Eof);
                }
                (Vec::new(), true)
            } else if let Some(newline) = available.iter().position(|byte| *byte == b'\n') {
                (available[..=newline].to_vec(), true)
            } else {
                (available.to_vec(), false)
            }
        };
        reader.consume(chunk.len());
        if output.len().saturating_add(chunk.len()) > MAX_IPC_LINE_BYTES {
            return Ok(LineRead::TooLarge);
        }
        output.extend_from_slice(&chunk);
        if complete {
            return Ok(LineRead::Line(output));
        }
    }
}

#[cfg(test)]
fn handle_request(store: Arc<dyn impetus_core::EventStore>, request: IpcRequest) -> IpcResponse {
    Harness::new(store, impetus_core::harness_api::policy()).handle(request)
}

fn data_root() -> Result<PathBuf> {
    Ok(std::env::var_os("IMPETUS_DATA_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").expect("HOME is set on macOS"))
                .join("Library/Application Support/Impetus")
        }))
}

fn socket_path() -> Result<PathBuf> {
    Ok(std::env::var_os("IMPETUS_SOCKET")
        .map(PathBuf::from)
        .unwrap_or(data_root()?.join("harness.sock")))
}

#[cfg(unix)]
fn set_socket_permissions(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .context("restrict harness socket permissions")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use impetus_core::{
        EventPayload, EventStore, IPC_VERSION, MemoryEventStore, NoticeEvent, ReadOnlyToolKind,
        RuntimeStatus, ToolOutcome,
    };
    use tokio_util::sync::CancellationToken;

    fn test_temp_dir(label: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "impetusd-{label}-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&path).expect("create temp dir");
        path
    }

    #[test]
    fn noninteractive_keychain_read_fails_closed_without_platform_access() {
        assert_eq!(
            read_keychain_credential_with(
                "impetus.test",
                "api-key",
                |_, _| panic!("keychain must not be accessed in non-interactive mode"),
                true,
            ),
            Err(ProviderError::MissingCredential),
        );
    }

    #[test]
    fn interactive_keychain_read_uses_fetcher_when_noninteractive_false() {
        assert_eq!(
            read_keychain_credential_with(
                "impetus.test",
                "api-key",
                |service, account| {
                    assert_eq!(service, "impetus.test");
                    assert_eq!(account, "api-key");
                    Ok(b"token".to_vec())
                },
                false,
            ),
            Ok("token".to_string()),
        );
    }

    #[test]
    fn artifact_gc_retention_is_seven_days() {
        assert_eq!(
            impetus_core::ARTIFACT_GC_RETENTION,
            std::time::Duration::from_secs(7 * 24 * 60 * 60)
        );
        assert!(impetus_core::ARTIFACT_GC_INTERVAL.as_secs() >= 3600);
    }

    #[tokio::test]
    async fn artifact_gc_loop_fail_safe_on_bad_root() {
        let dir = test_temp_dir("artifact-gc-bad");
        let bad_root = dir.join("not-a-directory");
        std::fs::write(&bad_root, b"x").expect("file as root");
        // First interval tick fires immediately; Err must not abort the task.
        spawn_artifact_gc_loop(bad_root);
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn startup_policy_optional_default_is_empty_overrides() {
        let dir = test_temp_dir("policy-optional");
        let path = dir.join("policy.json");
        let config = PolicyConfig::load_optional(&path).expect("missing ok");
        let engine = PolicyEngine::with_config(SandboxScope::local_workspace("."), config);
        assert!(engine.config().overrides.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn daemon_harness_wires_worktree_manager_under_data_root() {
        let dir = test_temp_dir("wt-wire");
        unsafe {
            std::env::set_var("IMPETUS_DATA_DIR", dir.to_str().expect("utf8 path"));
        }
        let store = Arc::new(MemoryEventStore::default());
        let harness = configured_harness(store, &dir, []).expect("configured harness");
        assert!(harness.has_worktree_manager());
        assert!(dir.join("worktrees.sqlite3").is_file());
        assert!(dir.join("worktrees").is_dir());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn daemon_harness_wires_explore_spawn_with_default_mock_provider() {
        let dir = test_temp_dir("explore-wire");
        unsafe {
            std::env::set_var("IMPETUS_DATA_DIR", dir.to_str().expect("utf8 path"));
        }
        let store = Arc::new(MemoryEventStore::default());
        let harness =
            configured_harness(store, &dir, []).expect("configured harness with explore spawn");
        assert!(harness.has_explore_spawn());
        assert!(harness.has_worktree_manager());

        let workspace = std::env::current_dir().expect("cwd");
        let outcome = harness
            .spawn_explore(
                impetus_core::ExploreChildRequest {
                    parent_session_id: "parent-daemon".into(),
                    child_id: "child-daemon".into(),
                    cwd: workspace,
                    allowed_tools: impetus_core::EXPLORE_ALLOWED_TOOLS
                        .iter()
                        .map(|tool| tool.to_string())
                        .collect(),
                    context_label: "daemon explore smoke".into(),
                    max_tokens: 2_000,
                    max_time_ms: 10_000,
                    max_depth: 1,
                },
                CancellationToken::new(),
            )
            .expect("explore spawn");
        assert_eq!(outcome.parent_session_id, "parent-daemon");
        assert_eq!(outcome.child_id, "child-daemon");
        let resumed = harness
            .complete_explore_and_gate("parent-daemon", &["child-daemon"])
            .expect("parent resume");
        assert!(resumed.contains("Mock response"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn daemon_mcp_autoload_fails_closed_on_invalid_config() {
        let dir = test_temp_dir("mcp-bad");
        let mcp_dir = dir.join("mcp");
        std::fs::create_dir_all(&mcp_dir).expect("mcp dir");
        std::fs::write(mcp_dir.join("broken.json"), b"{").expect("write bad json");
        unsafe {
            std::env::set_var("IMPETUS_DATA_DIR", dir.to_str().expect("utf8 path"));
        }
        let store = Arc::new(MemoryEventStore::default());
        let err = configured_harness(store, &dir, [])
            .err()
            .expect("bad mcp config should fail");
        assert!(err.to_string().contains("MCP autoload"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn daemon_hook_autoload_registers_valid_catalog() {
        let dir = test_temp_dir("hooks-ok");
        std::fs::write(
            dir.join("hooks.json"),
            r#"[{"pattern":"sleep","action":"deny"}]"#,
        )
        .expect("write hooks");
        unsafe {
            std::env::set_var("IMPETUS_DATA_DIR", dir.to_str().expect("utf8 path"));
        }
        let store = Arc::new(MemoryEventStore::default());
        let harness = configured_harness(store, &dir, []).expect("hook autoload harness");
        assert!(harness.has_hook_prefilter());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn daemon_hook_autoload_fails_closed_on_invalid_config() {
        let dir = test_temp_dir("hooks-bad");
        std::fs::write(dir.join("hooks.json"), b"{").expect("write bad json");
        unsafe {
            std::env::set_var("IMPETUS_DATA_DIR", dir.to_str().expect("utf8 path"));
        }
        let store = Arc::new(MemoryEventStore::default());
        let err = configured_harness(store, &dir, [])
            .err()
            .expect("bad hook config should fail");
        assert!(err.to_string().contains("hook prefilter"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn daemon_policy_store_autoload_registers_valid_config() {
        let dir = test_temp_dir("policy-store-ok");
        std::fs::write(
            dir.join("policy_store.json"),
            r#"{"version":1,"instructions":[{"id":"gov-1","label":"Rules"}]}"#,
        )
        .expect("write policy store");
        unsafe {
            std::env::set_var("IMPETUS_DATA_DIR", dir.to_str().expect("utf8 path"));
        }
        let store = Arc::new(MemoryEventStore::default());
        let harness = configured_harness(store, &dir, []).expect("policy store harness");
        assert!(harness.has_policy_store());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn daemon_mcp_autoload_registers_valid_config() {
        let dir = test_temp_dir("mcp-ok");
        let mcp_dir = dir.join("mcp");
        std::fs::create_dir_all(&mcp_dir).expect("mcp dir");
        std::fs::write(
            mcp_dir.join("echo.json"),
            br#"{
                "name": "echo",
                "command": "true",
                "args": [],
                "env": {},
                "transport": "stdio",
                "capabilities": { "tools": true, "resources": false, "prompts": false, "sampling": false }
            }"#,
        )
        .expect("write mcp");
        unsafe {
            std::env::set_var("IMPETUS_DATA_DIR", dir.to_str().expect("utf8 path"));
        }
        let store = Arc::new(MemoryEventStore::default());
        let harness = configured_harness(store, &dir, []).expect("mcp autoload harness");
        assert!(harness.has_tool_providers());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn startup_policy_explicit_file_applies_override() {
        let dir = test_temp_dir("policy-explicit");
        let path = dir.join("policy.json");
        std::fs::write(
            &path,
            r#"{"version":1,"overrides":{"spawn_process":"deny"}}"#,
        )
        .expect("write");
        let config = PolicyConfig::load_from_path(&path).expect("load");
        let engine = PolicyEngine::with_config(SandboxScope::local_workspace("."), config);
        assert_eq!(
            engine
                .config()
                .override_for(impetus_core::ActionKind::SpawnProcess),
            Some(impetus_core::PolicyConfigDecision::Deny)
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reload_policy_config_via_ipc_updates_live_policy() {
        let store = Arc::new(MemoryEventStore::default());
        let harness = Harness::new(store, impetus_core::harness_api::policy());
        let IpcResponse::PolicyConfig { config } = harness.handle(IpcRequest::ReloadPolicyConfig {
            path: None,
            config_json: Some(r#"{"version":1,"overrides":{"spawn_process":"deny"}}"#.into()),
        }) else {
            panic!("reload policy config");
        };
        assert_eq!(
            config.override_for(impetus_core::ActionKind::SpawnProcess),
            Some(impetus_core::PolicyConfigDecision::Deny)
        );
        assert_eq!(
            harness
                .policy()
                .config()
                .override_for(impetus_core::ActionKind::SpawnProcess),
            Some(impetus_core::PolicyConfigDecision::Deny)
        );
    }

    #[test]
    fn protocol_rejects_unknown_version() {
        assert_eq!(
            handle_request(
                Arc::new(MemoryEventStore::default()),
                IpcRequest::Hello {
                    version: IPC_VERSION + 1,
                    capabilities: vec![]
                }
            ),
            IpcResponse::Incompatible {
                supported_version: IPC_VERSION,
                client_version: IPC_VERSION + 1,
                upgrade_recommendation: Some(format!(
                    "Client version {} is newer than harness {}. Upgrade harness.",
                    IPC_VERSION + 1,
                    IPC_VERSION
                )),
            }
        );
    }

    #[tokio::test]
    async fn prompt_restarts_mock_provider_without_duplicating_durable_chunks() {
        let store = Arc::new(MemoryEventStore::default());
        let IpcResponse::Session { session_id, .. } = handle_request(
            store.clone(),
            IpcRequest::CreateSession {
                workspace_root: std::env::current_dir().unwrap().canonicalize().unwrap(),
            },
        ) else {
            panic!("create session response")
        };
        assert!(matches!(
            handle_request(
                store.clone(),
                IpcRequest::Prompt {
                    session_id,
                    text: "explain repository".into(),
                    artifact: None,
                    intent: Default::default(),
                }
            ),
            IpcResponse::Status {
                status: RuntimeStatus::Running,
                ..
            }
        ));
        tokio::time::sleep(std::time::Duration::from_millis(120)).await;
        assert!(matches!(
            handle_request(store.clone(), IpcRequest::Attach { session_id }),
            IpcResponse::Session {
                status: RuntimeStatus::Completed,
                ..
            }
        ));
        assert!(matches!(
            handle_request(store.clone(), IpcRequest::ListSessions),
            IpcResponse::Sessions { sessions }
                if sessions.len() == 1 && sessions[0].id == session_id
        ));
        let first = handle_request(
            store.clone(),
            IpcRequest::Stream {
                session_id,
                after_sequence: 0,
            },
        );
        let second = handle_request(
            store,
            IpcRequest::Stream {
                session_id,
                after_sequence: 0,
            },
        );
        assert_eq!(first, second);
        let IpcResponse::Events { events, .. } = first else {
            panic!("stream response")
        };
        let chunks = events
            .iter()
            .filter(|event| {
                matches!(
                    event.payload,
                    impetus_core::EventPayload::Agent(impetus_core::AgentEvent::Chunk { .. })
                )
            })
            .count();
        // Mock emits two small deltas; AgentLoop coalesces under AGENT_CHUNK_COALESCE_BYTES.
        assert_eq!(chunks, 1);
        assert!(events.iter().any(|event| matches!(
            event.payload,
            impetus_core::EventPayload::Run(RunEvent::Completed { .. })
        )));
    }

    #[tokio::test]
    async fn tool_read_outside_workspace_is_denied_over_ipc() {
        let store = Arc::new(MemoryEventStore::default());
        let IpcResponse::Session { session_id, .. } = handle_request(
            store.clone(),
            IpcRequest::CreateSession {
                workspace_root: std::env::current_dir().unwrap().canonicalize().unwrap(),
            },
        ) else {
            panic!("create session response")
        };
        // Workspace root for the harness test is the crate directory; an
        // absolute path outside it must be denied without leaking content.
        let response = handle_request(
            store,
            IpcRequest::Tool {
                session_id,
                kind: ReadOnlyToolKind::Read,
                target: "/etc/hosts".into(),
                pattern: None,
            },
        );
        assert!(matches!(
            response,
            IpcResponse::ToolResult {
                outcome: ToolOutcome::Denied { .. },
                ..
            }
        ));
        assert!(!format!("{response:?}").contains("127.0.0.1"));
    }

    #[tokio::test]
    async fn cancel_stops_mock_stream_before_restart_completes_it() {
        let store = Arc::new(MemoryEventStore::default());
        let IpcResponse::Session { session_id, .. } = handle_request(
            store.clone(),
            IpcRequest::CreateSession {
                workspace_root: std::env::current_dir().unwrap().canonicalize().unwrap(),
            },
        ) else {
            panic!("create session response")
        };
        assert!(matches!(
            handle_request(
                store.clone(),
                IpcRequest::Prompt {
                    session_id,
                    text: "cancel mock response".into(),
                    artifact: None,
                    intent: Default::default(),
                }
            ),
            IpcResponse::Status {
                status: RuntimeStatus::Running,
                ..
            }
        ));
        assert!(matches!(
            handle_request(store.clone(), IpcRequest::Cancel { session_id }),
            IpcResponse::Status {
                status: RuntimeStatus::Cancelled,
                ..
            }
        ));
        tokio::time::sleep(std::time::Duration::from_millis(120)).await;
        let IpcResponse::Events { events, .. } = handle_request(
            store,
            IpcRequest::Stream {
                session_id,
                after_sequence: 0,
            },
        ) else {
            panic!("stream response")
        };
        assert!(events.iter().any(|event| matches!(
            event.payload,
            impetus_core::EventPayload::Run(RunEvent::Cancelled { .. })
        )));
        assert!(!events.iter().any(|event| matches!(
            event.payload,
            impetus_core::EventPayload::Run(RunEvent::Completed { .. })
        )));
    }

    #[tokio::test]
    async fn second_prompt_is_rejected_while_run_is_active() {
        let store = Arc::new(MemoryEventStore::default());
        let harness = Harness::new(store.clone(), impetus_core::harness_api::policy());
        let IpcResponse::Session { session_id, .. } = harness.handle(IpcRequest::CreateSession {
            workspace_root: std::env::current_dir().unwrap().canonicalize().unwrap(),
        }) else {
            panic!("create session response")
        };
        assert!(matches!(
            harness.handle(IpcRequest::Prompt {
                session_id,
                text: "first".into(),
                artifact: None,
                intent: Default::default(),
            }),
            IpcResponse::Status {
                status: RuntimeStatus::Running,
                ..
            }
        ));
        assert!(matches!(
            harness.handle(IpcRequest::Prompt {
                session_id,
                text: "second".into(),
                artifact: None,
                intent: Default::default(),
            }),
            IpcResponse::Error {
                code: IpcErrorCode::Conflict,
                ..
            }
        ));
        let started = store
            .list(session_id)
            .expect("list events")
            .into_iter()
            .fold((0, 0), |(started, intents), event| match event.payload {
                EventPayload::Run(RunEvent::Started { .. }) => (started + 1, intents),
                EventPayload::Intent(_) => (started, intents + 1),
                _ => (started, intents),
            });
        assert_eq!(started, (1, 1));
    }

    #[tokio::test]
    async fn wire_requires_hello_and_negotiated_capability() {
        let harness = Arc::new(Harness::new(
            Arc::new(MemoryEventStore::default()),
            impetus_core::harness_api::policy(),
        ));
        let (server, client) = UnixStream::pair().expect("create Unix pair");
        let server_task = tokio::spawn(async move { serve_client(server, harness).await });
        let (reader, mut writer) = client.into_split();
        let mut lines = BufReader::new(reader).lines();

        writer
            .write_all(b"{\"method\":\"list_sessions\"}\n")
            .await
            .expect("send request before hello");
        writer.flush().await.expect("flush request");
        let response: IpcResponse = serde_json::from_str(
            &lines
                .next_line()
                .await
                .expect("read pre-hello response")
                .expect("pre-hello response"),
        )
        .expect("parse pre-hello response");
        assert!(matches!(
            response,
            IpcResponse::Error {
                code: IpcErrorCode::InvalidRequest,
                ..
            }
        ));

        for request in [
            IpcRequest::Hello {
                version: IPC_VERSION,
                capabilities: vec!["session_create".into()],
            },
            IpcRequest::ListSessions,
        ] {
            writer
                .write_all(format!("{}\n", serde_json::to_string(&request).unwrap()).as_bytes())
                .await
                .expect("send negotiated request");
        }
        writer.flush().await.expect("flush negotiated requests");
        assert!(matches!(
            serde_json::from_str::<IpcResponse>(
                &lines.next_line().await.unwrap().expect("hello response")
            )
            .unwrap(),
            IpcResponse::Hello { capabilities, .. }
                if capabilities == vec!["session_create"]
        ));
        assert!(matches!(
            serde_json::from_str::<IpcResponse>(
                &lines
                    .next_line()
                    .await
                    .unwrap()
                    .expect("capability response")
            )
            .unwrap(),
            IpcResponse::Error {
                code: IpcErrorCode::Unavailable,
                ..
            }
        ));
        server_task.abort();
    }

    #[tokio::test]
    async fn subscription_pushes_new_durable_events_after_backfill_cursor() {
        let store = Arc::new(MemoryEventStore::default());
        let session_id = store.create_session().expect("create session");
        let (server, client) = UnixStream::pair().expect("create Unix pair");
        let server_harness = Arc::new(Harness::new(
            store.clone(),
            impetus_core::harness_api::policy(),
        ));
        let server_task = tokio::spawn(async move { serve_client(server, server_harness).await });

        let (reader, mut writer) = client.into_split();
        writer
            .write_all(
                format!(
                    "{}\n{}\n",
                    serde_json::to_string(&IpcRequest::Hello {
                        version: IPC_VERSION,
                        capabilities: vec!["subscribe".into()],
                    })
                    .expect("encode hello"),
                    serde_json::to_string(&IpcRequest::Subscribe {
                        session_id,
                        after_sequence: 1,
                    })
                    .expect("encode subscription"),
                )
                .as_bytes(),
            )
            .await
            .expect("send subscription");
        writer.flush().await.expect("flush subscription");

        let mut lines = BufReader::new(reader).lines();
        assert!(matches!(
            serde_json::from_str::<IpcResponse>(
                &lines.next_line().await.expect("hello read").expect("hello")
            )
            .expect("parse hello"),
            IpcResponse::Hello { .. }
        ));
        assert!(matches!(
            serde_json::from_str::<IpcResponse>(
                &lines.next_line().await.expect("ack read").expect("ack")
            )
            .expect("parse ack"),
            IpcResponse::Subscribed { session_id: actual } if actual == session_id
        ));

        store
            .append_next(session_id, EventPayload::Notice(NoticeEvent::PolicyAllowed))
            .expect("append durable event");
        let response = serde_json::from_str::<IpcResponse>(
            &tokio::time::timeout(std::time::Duration::from_secs(1), lines.next_line())
                .await
                .expect("event push timeout")
                .expect("event read")
                .expect("event line"),
        )
        .expect("parse event");
        assert!(matches!(
            response,
            IpcResponse::Events { session_id: actual, events }
                if actual == session_id && events.len() == 1 && events[0].sequence == 2
        ));

        server_task.abort();
    }
}
