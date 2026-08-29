use anyhow::{Context, Result, bail};
use impetus_core::{
    CredentialResolver, CredentialStrategy, Harness, IpcErrorCode, IpcRequest, IpcResponse,
    OpenAiCompatibleProvider, ProviderError, ProviderProfile, RetryBudget, SqliteEventStore,
};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};

#[cfg(test)]
use impetus_core::RunEvent;

const MAX_IPC_LINE_BYTES: usize = 64 * 1024;

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
    let harness = Arc::new(configured_harness(store)?);
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

/// Direct providers are enabled only by an explicit daemon-start profile file.
/// The file is deserialized into a deny-unknown-fields DTO, so a raw token
/// cannot be silently accepted as configuration.
fn configured_harness(store: Arc<dyn impetus_core::EventStore>) -> Result<Harness> {
    let mut arguments = std::env::args_os().skip(1);
    let Some(flag) = arguments.next() else {
        return Ok(Harness::new(store, impetus_core::harness_api::policy()));
    };
    if flag != "--provider-profile" {
        bail!("usage: impetus [--provider-profile PATH]");
    }
    let profile_path = arguments
        .next()
        .context("--provider-profile requires PATH")?;
    if arguments.next().is_some() {
        bail!("usage: impetus [--provider-profile PATH]");
    }
    let profile_bytes = std::fs::read(profile_path).context("read provider profile")?;
    let profile: ProviderProfile = serde_json::from_slice(&profile_bytes)
        .context("provider profile must contain only the documented non-secret fields")?;
    let provider = OpenAiCompatibleProvider::new(profile, RetryBudget::default())
        .map_err(|error| anyhow::anyhow!(error.to_string()))?;
    Ok(Harness::with_openai_provider_and_resolver(
        store,
        impetus_core::harness_api::policy(),
        provider,
        Arc::new(MacosKeychainResolver),
    ))
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

#[cfg(target_os = "macos")]
fn read_keychain_credential(service: &str, account: &str) -> Result<String, ProviderError> {
    let bytes = security_framework::passwords::get_generic_password(service, account)
        .map_err(|_| ProviderError::MissingCredential)?;
    String::from_utf8(bytes).map_err(|_| ProviderError::MissingCredential)
}

#[cfg(not(target_os = "macos"))]
fn read_keychain_credential(_service: &str, _account: &str) -> Result<String, ProviderError> {
    Err(ProviderError::MissingCredential)
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
                let (session_id, after_sequence) = subscription.expect("checked above");
                if notified_session_id != session_id {
                    continue;
                }
                match harness.handle(IpcRequest::Stream { session_id, after_sequence }) {
                    IpcResponse::Events { events, .. } if !events.is_empty() => {
                        subscription = events.last().map(|last| (session_id, last.sequence));
                        write_response(&mut writer, &IpcResponse::Events { session_id, events }).await?;
                    }
                    IpcResponse::Events { .. } => {}
                    error @ IpcResponse::Error { .. } => {
                        write_response(&mut writer, &error).await?;
                        return Ok(());
                    }
                    _ => unreachable!("stream request returns events or error"),
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
        IpcRequest::Stream { .. } => "event_stream",
        IpcRequest::Prompt { .. } => "prompt",
        IpcRequest::Context { .. } => "context",
        IpcRequest::Cancel { .. } => "cancel",
        IpcRequest::Tool { .. } => "tool",
        IpcRequest::Subscribe { .. } => "subscribe",
        IpcRequest::ResolveApproval { .. } => "resolve_approval",
        IpcRequest::GetAttachment { .. } => "get_attachment",
        IpcRequest::GetApprovalDetail { .. } => "get_approval_detail",
        IpcRequest::Diagnostics => "diagnostics",
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
                    text: "explain repository".into()
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
            IpcResponse::Sessions { sessions } if sessions == vec![session_id]
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
        assert_eq!(chunks, 2);
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
                    text: "cancel mock response".into()
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
