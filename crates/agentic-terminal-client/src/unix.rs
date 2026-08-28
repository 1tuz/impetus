//! Unix-socket transport for the harness client.
//!
//! Speaks the versioned, line-delimited JSON IPC implemented by the harness
//! daemon. The client sends one JSON request per line and reads one JSON
//! response per line. On connect it performs a `Hello` handshake and treats an
//! `Incompatible` response as a hard failure.

use crate::{EventSubscription, HarnessClient, IPC_VERSION, IpcRequest, IpcResponse};
use impetus_core::{IPC_CAPABILITIES, IpcErrorCode};
use anyhow::{Context, Result, anyhow, bail};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::sync::Mutex;

const MAX_IPC_LINE_BYTES: usize = 64 * 1024;

/// Client transport over a Unix domain socket.
///
/// The socket is shared behind a mutex so multiple concurrent requests from one
/// client are serialized on the wire (the harness is single-stream per
/// connection anyway).
pub struct UnixSocketTransport {
    stream: Arc<Mutex<UnixStream>>,
    socket_path: PathBuf,
}

impl UnixSocketTransport {
    /// Connect to the harness daemon and complete the version handshake.
    pub async fn connect(socket_path: impl AsRef<Path>) -> Result<Self> {
        let stream = UnixStream::connect(socket_path.as_ref())
            .await
            .with_context(|| {
                format!("connect harness socket {}", socket_path.as_ref().display())
            })?;
        let transport = Self {
            stream: Arc::new(Mutex::new(stream)),
            socket_path: socket_path.as_ref().to_path_buf(),
        };
        match transport.hello().await? {
            IpcResponse::Hello { .. } => Ok(transport),
            IpcResponse::Incompatible {
                supported_version,
                upgrade_recommendation,
                ..
            } => bail!(
                "harness protocol incompatible: client={} supported={} ({})",
                IPC_VERSION,
                supported_version,
                upgrade_recommendation
                    .as_deref()
                    .unwrap_or("no recommendation")
            ),
            other => bail!("unexpected handshake response: {:?}", other),
        }
    }

    /// Serialize `request`, write it as one line, read one response line.
    async fn round_trip(&self, request: IpcRequest) -> Result<IpcResponse> {
        let line = serde_json::to_string(&request).context("serialize IPC request")?;
        let mut stream = self.stream.lock().await;
        let (reader, mut writer) = stream.split();
        writer
            .write_all(line.as_bytes())
            .await
            .context("write IPC request")?;
        writer.write_all(b"\n").await.context("write IPC newline")?;
        writer.flush().await.context("flush IPC request")?;

        let mut reader = BufReader::new(reader);
        let line = read_bounded_line(&mut reader, "IPC response").await?;
        serde_json::from_str(line.trim_end()).context("parse IPC response")
    }
}

/// Dedicated Unix connection for asynchronous durable events.
struct UnixEventSubscription {
    reader: BufReader<tokio::net::unix::OwnedReadHalf>,
}

impl EventSubscription for UnixEventSubscription {
    fn next_events(
        &mut self,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<impetus_core::Event>>> + Send + '_>> {
        Box::pin(async move {
            loop {
                let line = read_bounded_line(&mut self.reader, "harness event").await?;
                match serde_json::from_str::<IpcResponse>(line.trim_end())
                    .context("parse harness event")?
                {
                    IpcResponse::Events { events, .. } if !events.is_empty() => return Ok(events),
                    IpcResponse::Events { .. } | IpcResponse::Subscribed { .. } => {}
                    IpcResponse::Incompatible {
                        supported_version,
                        upgrade_recommendation,
                        ..
                    } => bail!(
                        "harness protocol incompatible: client={} supported={} ({})",
                        IPC_VERSION,
                        supported_version,
                        upgrade_recommendation
                            .as_deref()
                            .unwrap_or("no recommendation")
                    ),
                    IpcResponse::Error { message, .. } => {
                        bail!("event subscription failed: {message}")
                    }
                    response => bail!("unexpected event response: {response:?}"),
                }
            }
        })
    }
}

impl HarnessClient for UnixSocketTransport {
    async fn hello(&self) -> Result<IpcResponse> {
        self.round_trip(IpcRequest::Hello {
            version: IPC_VERSION,
            capabilities: IPC_CAPABILITIES
                .iter()
                .map(|capability| (*capability).to_owned())
                .collect(),
        })
        .await
    }

    async fn request(&self, request: IpcRequest) -> Result<IpcResponse> {
        let response = self.round_trip(request).await?;
        if let IpcResponse::Error { code, message } = &response
            && *code == IpcErrorCode::InvalidRequest
        {
            return Err(anyhow!("invalid request: {message}"));
        }
        Ok(response)
    }

    async fn subscribe_live(
        &self,
        session_id: uuid::Uuid,
        after_sequence: u64,
    ) -> Result<Box<dyn EventSubscription>> {
        let stream = UnixStream::connect(&self.socket_path)
            .await
            .with_context(|| format!("connect event socket {}", self.socket_path.display()))?;
        let (reader, mut writer) = stream.into_split();
        for request in [
            IpcRequest::Hello {
                version: IPC_VERSION,
                capabilities: vec!["subscribe".into()],
            },
            IpcRequest::Subscribe {
                session_id,
                after_sequence,
            },
        ] {
            writer
                .write_all(serde_json::to_string(&request)?.as_bytes())
                .await
                .context("write event subscription request")?;
            writer.write_all(b"\n").await?;
        }
        writer.flush().await.context("flush event subscription")?;

        let mut reader = BufReader::new(reader);
        let hello = read_bounded_line(&mut reader, "event handshake").await?;
        match serde_json::from_str(hello.trim_end()).context("parse event handshake")? {
            IpcResponse::Hello { capabilities, .. }
                if capabilities
                    .iter()
                    .any(|capability| capability == "subscribe") => {}
            IpcResponse::Hello { .. } => bail!("harness did not negotiate event subscription"),
            IpcResponse::Incompatible {
                supported_version,
                upgrade_recommendation,
                ..
            } => bail!(
                "harness protocol incompatible: client={} supported={} ({})",
                IPC_VERSION,
                supported_version,
                upgrade_recommendation
                    .as_deref()
                    .unwrap_or("no recommendation")
            ),
            response => bail!("unexpected event handshake: {response:?}"),
        }
        let subscribed = read_bounded_line(&mut reader, "subscription response").await?;
        match serde_json::from_str(subscribed.trim_end()).context("parse subscription ack")? {
            IpcResponse::Subscribed { session_id: actual } if actual == session_id => {}
            response => bail!("unexpected subscription response: {response:?}"),
        }
        Ok(Box::new(UnixEventSubscription { reader }))
    }
}

async fn read_bounded_line<R>(reader: &mut R, label: &str) -> Result<String>
where
    R: AsyncBufRead + Unpin,
{
    let mut output = Vec::new();
    let mut exceeded = false;
    loop {
        let (chunk, complete) = {
            let available = reader
                .fill_buf()
                .await
                .with_context(|| format!("read {label}"))?;
            if available.is_empty() {
                if output.is_empty() {
                    bail!("harness closed connection before {label}");
                }
                (Vec::new(), true)
            } else if let Some(newline) = available.iter().position(|byte| *byte == b'\n') {
                (available[..=newline].to_vec(), true)
            } else {
                (available.to_vec(), false)
            }
        };
        reader.consume(chunk.len());
        if exceeded || output.len().saturating_add(chunk.len()) > MAX_IPC_LINE_BYTES {
            exceeded = true;
        } else {
            output.extend_from_slice(&chunk);
        }
        if complete {
            if exceeded {
                bail!("{label} exceeds 64 KiB");
            }
            return String::from_utf8(output).with_context(|| format!("decode {label} as UTF-8"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn oversized_line_is_drained_before_next_response() {
        let mut input = vec![b'x'; MAX_IPC_LINE_BYTES + 1];
        input.extend_from_slice(b"\nok\n");
        let mut reader = BufReader::new(input.as_slice());

        let error = read_bounded_line(&mut reader, "test line")
            .await
            .expect_err("oversized line must fail");
        assert!(error.to_string().contains("exceeds 64 KiB"));
        assert_eq!(
            read_bounded_line(&mut reader, "next line").await.unwrap(),
            "ok\n"
        );
    }
}
