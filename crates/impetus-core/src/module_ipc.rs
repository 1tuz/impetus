use crate::module::{CapabilityProbe, ModuleDescriptor, ModuleState};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{Mutex, RwLock};

/// External module process handle
pub struct ExternalModule {
    descriptor: ModuleDescriptor,
    process: Arc<Mutex<Option<Child>>>,
    socket_path: PathBuf,
    state: Arc<RwLock<ModuleState>>,
}

/// IPC message types for module communication
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ModuleMessage {
    /// Request from harness to module
    Request {
        id: String,
        method: String,
        params: serde_json::Value,
    },
    /// Response from module to harness
    Response {
        id: String,
        result: Option<serde_json::Value>,
        error: Option<String>,
    },
    /// Module state notification
    StateChange {
        state: ModuleState,
        reason: Option<String>,
    },
    /// Capability probe request
    ProbeCapabilities,
    /// Capability probe response
    Capabilities { probes: Vec<CapabilityProbe> },
    /// Health check ping
    Ping,
    /// Health check pong
    Pong,
    /// Shutdown request
    Shutdown,
}

impl ExternalModule {
    /// Create new external module instance
    pub fn new(descriptor: ModuleDescriptor, socket_path: PathBuf) -> Self {
        Self {
            descriptor,
            process: Arc::new(Mutex::new(None)),
            socket_path,
            state: Arc::new(RwLock::new(ModuleState::Discovered)),
        }
    }

    /// Spawn module process and establish IPC connection
    pub async fn spawn(&self, binary_path: PathBuf) -> Result<()> {
        let mut process_guard = self.process.lock().await;
        if process_guard.is_some() {
            anyhow::bail!("Module process already running");
        }

        // Set up socket listener before spawning process
        let listener =
            UnixListener::bind(&self.socket_path).context("Failed to bind module IPC socket")?;

        // Spawn module process with socket path
        let child = Command::new(binary_path)
            .arg("--socket")
            .arg(&self.socket_path)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("Failed to spawn module process")?;

        *process_guard = Some(child);
        drop(process_guard);

        // Wait for module to connect
        let (_stream, _) =
            tokio::time::timeout(std::time::Duration::from_secs(5), listener.accept())
                .await
                .context("Module connection timeout")?
                .context("Failed to accept module connection")?;

        self.set_state(ModuleState::Ready).await;

        // Clean up listener
        drop(listener);
        let _ = tokio::fs::remove_file(&self.socket_path).await;

        Ok(())
    }

    /// Send message to module over IPC
    pub async fn send_message(
        &self,
        stream: &mut UnixStream,
        message: ModuleMessage,
    ) -> Result<()> {
        let json = serde_json::to_string(&message)?;
        stream.write_all(json.as_bytes()).await?;
        stream.write_all(b"\n").await?;
        stream.flush().await?;
        Ok(())
    }

    /// Receive message from module over IPC
    pub async fn receive_message(&self, stream: &mut UnixStream) -> Result<ModuleMessage> {
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        reader.read_line(&mut line).await?;
        let message: ModuleMessage = serde_json::from_str(&line)?;
        Ok(message)
    }

    /// Probe module capabilities via IPC
    pub async fn probe_capabilities(
        &self,
        stream: &mut UnixStream,
    ) -> Result<Vec<CapabilityProbe>> {
        self.send_message(stream, ModuleMessage::ProbeCapabilities)
            .await?;

        let response = tokio::time::timeout(
            std::time::Duration::from_secs(3),
            self.receive_message(stream),
        )
        .await
        .context("Capability probe timeout")??;

        match response {
            ModuleMessage::Capabilities { probes } => Ok(probes),
            _ => anyhow::bail!("Unexpected response to capability probe"),
        }
    }

    /// Health check via IPC ping
    pub async fn health_check(&self, stream: &mut UnixStream) -> Result<()> {
        self.send_message(stream, ModuleMessage::Ping).await?;

        let response = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            self.receive_message(stream),
        )
        .await
        .context("Health check timeout")??;

        match response {
            ModuleMessage::Pong => Ok(()),
            _ => anyhow::bail!("Unexpected response to health check"),
        }
    }

    /// Request graceful shutdown via IPC
    pub async fn shutdown(&self, stream: &mut UnixStream) -> Result<()> {
        self.send_message(stream, ModuleMessage::Shutdown).await?;
        self.set_state(ModuleState::Stopping).await;

        // Wait for process to exit
        let mut process_guard = self.process.lock().await;
        if let Some(mut child) = process_guard.take() {
            tokio::time::timeout(std::time::Duration::from_secs(5), async {
                loop {
                    match child.try_wait() {
                        Ok(Some(_)) => break,
                        Ok(None) => tokio::time::sleep(std::time::Duration::from_millis(100)).await,
                        Err(e) => return Err(anyhow::anyhow!("Failed to wait for process: {}", e)),
                    }
                }
                Ok(())
            })
            .await
            .context("Module shutdown timeout")??;
        }

        self.set_state(ModuleState::Stopped).await;
        Ok(())
    }

    /// Force kill module process
    pub async fn kill(&self) -> Result<()> {
        let mut process_guard = self.process.lock().await;
        if let Some(mut child) = process_guard.take() {
            child.kill().context("Failed to kill module process")?;
        }
        self.set_state(ModuleState::Stopped).await;
        Ok(())
    }

    /// Get current module state
    pub async fn state(&self) -> ModuleState {
        *self.state.read().await
    }

    /// Set module state
    async fn set_state(&self, state: ModuleState) {
        *self.state.write().await = state;
    }

    /// Get module descriptor
    pub fn descriptor(&self) -> &ModuleDescriptor {
        &self.descriptor
    }
}

/// Manager for external module processes
pub struct ExternalModuleManager {
    modules: Arc<RwLock<std::collections::HashMap<String, Arc<ExternalModule>>>>,
    socket_dir: PathBuf,
}

impl ExternalModuleManager {
    pub fn new(socket_dir: PathBuf) -> Self {
        Self {
            modules: Arc::new(RwLock::new(std::collections::HashMap::new())),
            socket_dir,
        }
    }

    /// Register and spawn external module
    pub async fn spawn_module(
        &self,
        descriptor: ModuleDescriptor,
        binary_path: PathBuf,
    ) -> Result<Arc<ExternalModule>> {
        let socket_path = self.socket_dir.join(format!("{}.sock", descriptor.id));
        let module = Arc::new(ExternalModule::new(descriptor.clone(), socket_path));

        module.spawn(binary_path).await?;

        self.modules
            .write()
            .await
            .insert(descriptor.id.clone(), module.clone());

        Ok(module)
    }

    /// Get module by ID
    pub async fn get_module(&self, module_id: &str) -> Option<Arc<ExternalModule>> {
        self.modules.read().await.get(module_id).cloned()
    }

    /// Shutdown module
    pub async fn shutdown_module(&self, module_id: &str) -> Result<()> {
        let module = self
            .get_module(module_id)
            .await
            .context("Module not found")?;

        // Attempt graceful shutdown, fall back to kill
        let stream_result = UnixStream::connect(&module.socket_path).await;
        if let Ok(mut stream) = stream_result
            && module.shutdown(&mut stream).await.is_ok()
        {
            self.modules.write().await.remove(module_id);
            return Ok(());
        }

        // Force kill if graceful shutdown failed
        module.kill().await?;
        self.modules.write().await.remove(module_id);
        Ok(())
    }

    /// Shutdown all modules
    pub async fn shutdown_all(&self) -> Result<()> {
        let module_ids: Vec<String> = self.modules.read().await.keys().cloned().collect();

        for module_id in module_ids {
            if let Err(e) = self.shutdown_module(&module_id).await {
                eprintln!("Failed to shutdown module {}: {}", module_id, e);
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::module::{ModuleKind, ModulePermissions};

    #[test]
    fn external_module_creation() {
        let descriptor = ModuleDescriptor {
            id: "test-external".to_string(),
            name: "Test External Module".to_string(),
            version: "1.0.0".to_string(),
            kind: ModuleKind::Custom,
            provides: vec!["test".to_string()],
            requires: vec![],
            capabilities: vec!["test_cap".to_string()],
            permissions: ModulePermissions::default(),
        };

        let socket_path = PathBuf::from("/tmp/test-module.sock");
        let module = ExternalModule::new(descriptor.clone(), socket_path);

        assert_eq!(module.descriptor().id, "test-external");
    }

    #[tokio::test]
    async fn message_serialization() {
        let msg = ModuleMessage::Request {
            id: "req-1".to_string(),
            method: "test_method".to_string(),
            params: serde_json::json!({"key": "value"}),
        };

        let json = serde_json::to_string(&msg).unwrap();
        let deserialized: ModuleMessage = serde_json::from_str(&json).unwrap();

        match deserialized {
            ModuleMessage::Request { id, method, .. } => {
                assert_eq!(id, "req-1");
                assert_eq!(method, "test_method");
            }
            _ => panic!("Wrong message type"),
        }
    }
}
