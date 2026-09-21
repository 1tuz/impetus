//! MCP (Model Context Protocol) adapter.
//!
//! Imports tools, resources and prompts from MCP stdio servers into the
//! canonical extension format. Implements a minimal JSON-RPC 2.0 client with
//! LSP-style Content-Length framing — no external MCP crate.

use crate::extension_compat::{
    CanonicalModuleKind, CanonicalModuleSpec, ExtensionSource, ImportCapability, ImportResult,
    Instruction, InstructionContext, InstructionPriority, McpCapabilities, McpModule, McpTransport,
    ToolHandler, ToolProvider,
};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::time::Duration;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// RPC response timeout for a single request/response round-trip.
const RPC_TIMEOUT: Duration = Duration::from_secs(15);

/// MCP tool as reported by `tools/list`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpTool {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default, rename = "inputSchema")]
    pub input_schema: serde_json::Value,
    /// Optional MCP tool annotations (`readOnlyHint`, `destructiveHint`, …).
    #[serde(default)]
    pub annotations: Option<serde_json::Value>,
}

/// MCP resource as reported by `resources/list`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpResource {
    pub uri: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
}

/// MCP prompt argument definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpPromptArgument {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub required: bool,
}

/// MCP prompt as reported by `prompts/list`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpPrompt {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub arguments: Vec<McpPromptArgument>,
}

/// Result of an MCP health check.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerHealth {
    pub reachable: bool,
    pub server_info: serde_json::Value,
    pub capabilities: McpCapabilities,
    pub latency_ms: u64,
}

/// Byte-stream abstraction so tests can plug an in-memory duplex instead of
/// spawning a subprocess.
#[async_trait::async_trait]
trait McpStream: Send {
    async fn read_message(&mut self) -> Result<String>;
    async fn write_message(&mut self, message: &str) -> Result<()>;
}

/// MCP stream backed by a spawned subprocess (stdio transport).
struct ProcessStream {
    // The child is never read after spawn; keeping it here is what makes the
    // subprocess die with the stream (kill_on_drop) instead of draining zombies.
    #[allow(dead_code)]
    child: tokio::process::Child,
    reader: tokio::io::BufReader<tokio::process::ChildStdout>,
    writer: tokio::io::BufWriter<tokio::process::ChildStdin>,
}

impl ProcessStream {
    async fn spawn(module: &McpModule) -> Result<Self> {
        let mut cmd = tokio::process::Command::new(&module.command);
        cmd.args(&module.args)
            .envs(&module.env)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true);

        let mut child = cmd
            .spawn()
            .with_context(|| format!("failed to spawn MCP server '{}'", module.command))?;

        let stdout = child
            .stdout
            .take()
            .context("MCP server stdout not available")?;
        let stdin = child
            .stdin
            .take()
            .context("MCP server stdin not available")?;

        Ok(Self {
            child,
            reader: tokio::io::BufReader::new(stdout),
            writer: tokio::io::BufWriter::new(stdin),
        })
    }
}

#[async_trait::async_trait]
impl McpStream for ProcessStream {
    async fn read_message(&mut self) -> Result<String> {
        read_mcp_message(&mut self.reader).await
    }

    async fn write_message(&mut self, message: &str) -> Result<()> {
        write_mcp_message(&mut self.writer, message).await
    }
}

/// Read a single MCP message: LSP-style `Content-Length` framing with a
/// per-line JSON fallback for servers that ignore framing.
async fn read_mcp_message<R: AsyncBufRead + Unpin>(reader: &mut R) -> Result<String> {
    let mut first_line = String::new();
    let n = reader.read_line(&mut first_line).await?;
    if n == 0 {
        bail!("MCP server closed the connection");
    }
    let trimmed = first_line.trim_end();

    match trimmed.strip_prefix("Content-Length:") {
        Some(len_str) => {
            let len: usize = len_str
                .trim()
                .parse()
                .with_context(|| format!("invalid Content-Length header: {}", trimmed))?;

            // Consume remaining headers until the blank line.
            loop {
                let mut line = String::new();
                let n = reader.read_line(&mut line).await?;
                if n == 0 {
                    bail!("MCP server closed the connection mid-headers");
                }
                if line.trim().is_empty() {
                    break;
                }
            }

            let mut buf = vec![0u8; len];
            reader.read_exact(&mut buf).await?;
            String::from_utf8(buf).context("MCP message is not valid UTF-8")
        }
        None => Ok(trimmed.to_string()),
    }
}

/// Write a single MCP message with `Content-Length` framing.
async fn write_mcp_message<W: AsyncWrite + Unpin>(writer: &mut W, message: &str) -> Result<()> {
    writer
        .write_all(format!("Content-Length: {}\r\n\r\n{}", message.len(), message).as_bytes())
        .await?;
    writer.flush().await?;
    Ok(())
}

/// Minimal JSON-RPC 2.0 client over an MCP stream.
pub struct McpAdapter {
    stream: Box<dyn McpStream>,
    next_id: u64,
    /// `serverInfo` from the initialize response.
    server_info: Value,
    /// Capabilities negotiated with the server.
    capabilities: McpCapabilities,
}

impl McpAdapter {
    /// Connect to a configured MCP stdio server and run `initialize`.
    pub async fn connect(module: &McpModule) -> Result<Self> {
        if module.transport != McpTransport::Stdio {
            bail!(
                "MCP transport {:?} not supported, only stdio is implemented",
                module.transport
            );
        }
        let stream = ProcessStream::spawn(module).await?;
        let mut adapter = Self::from_stream(Box::new(stream))?;
        adapter
            .initialize()
            .await
            .context("MCP initialize handshake failed")?;
        Ok(adapter)
    }

    /// Wrap an arbitrary byte stream (used by tests with in-memory duplex).
    fn from_stream(stream: Box<dyn McpStream>) -> Result<Self> {
        Ok(Self {
            stream,
            next_id: 1,
            server_info: Value::Null,
            capabilities: McpCapabilities::default(),
        })
    }

    /// Run the MCP `initialize` handshake and store negotiated capabilities.
    async fn initialize(&mut self) -> Result<()> {
        let result = self
            .request(
                "initialize",
                json!({
                    "protocolVersion": "2024-11-05",
                    "capabilities": {},
                    "clientInfo": {
                        "name": "impetus",
                        "version": env!("CARGO_PKG_VERSION"),
                    }
                }),
            )
            .await?;

        self.server_info = result.get("serverInfo").cloned().unwrap_or_default();
        let caps = result.get("capabilities").cloned().unwrap_or_default();
        self.capabilities = McpCapabilities {
            tools: caps.get("tools").is_some(),
            resources: caps.get("resources").is_some(),
            prompts: caps.get("prompts").is_some(),
            sampling: caps.get("sampling").is_some(),
        };

        // Some minimal servers expect an initialized notification.
        let initialized = json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized",
            "params": {}
        });
        self.send_notification(&initialized).await?;
        Ok(())
    }

    /// Send a JSON-RPC request and return the `result` field.
    async fn request(&mut self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id;
        self.next_id += 1;

        let request = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        self.stream
            .write_message(&request.to_string())
            .await
            .with_context(|| format!("failed to send {} request", method))?;

        loop {
            let message = tokio::time::timeout(RPC_TIMEOUT, self.stream.read_message())
                .await
                .with_context(|| format!("timed out waiting for {} response", method))??;

            let msg: Value = serde_json::from_str(&message).context("invalid JSON-RPC response")?;

            // Skip server notifications (method without id) until our response.
            if msg.get("id").is_none() {
                continue;
            }

            if let Some(error) = msg.get("error") {
                bail!(
                    "MCP {} failed: {}",
                    method,
                    error
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown error")
                );
            }

            return msg
                .get("result")
                .cloned()
                .context("JSON-RPC response is missing result");
        }
    }

    /// Send a JSON-RPC notification (no id, no response expected).
    async fn send_notification(&mut self, message: &Value) -> Result<()> {
        self.stream.write_message(&message.to_string()).await?;
        Ok(())
    }

    /// List tools exposed by the server.
    pub async fn list_tools(&mut self) -> Result<Vec<McpTool>> {
        let result = self.request("tools/list", json!({})).await?;
        let tools = result.get("tools").cloned().unwrap_or_else(|| json!([]));
        serde_json::from_value(tools).context("invalid tools/list result")
    }

    /// Invoke a tool via MCP `tools/call`.
    pub async fn call_tool(&mut self, name: &str, arguments: Value) -> Result<Value> {
        self.request(
            "tools/call",
            json!({
                "name": name,
                "arguments": arguments,
            }),
        )
        .await
        .with_context(|| format!("MCP tools/call failed for `{name}`"))
    }

    /// List resources exposed by the server.
    pub async fn list_resources(&mut self) -> Result<Vec<McpResource>> {
        let result = self.request("resources/list", json!({})).await?;
        let resources = result
            .get("resources")
            .cloned()
            .unwrap_or_else(|| json!([]));
        serde_json::from_value(resources).context("invalid resources/list result")
    }

    /// List prompts exposed by the server.
    pub async fn list_prompts(&mut self) -> Result<Vec<McpPrompt>> {
        let result = self.request("prompts/list", json!({})).await?;
        let prompts = result.get("prompts").cloned().unwrap_or_else(|| json!([]));
        serde_json::from_value(prompts).context("invalid prompts/list result")
    }

    /// Ping the server (health check). Returns false on any failure.
    pub async fn ping(&mut self) -> bool {
        self.request("ping", json!({})).await.is_ok()
    }

    /// Shut the server down gracefully.
    pub async fn shutdown(&mut self) -> Result<()> {
        self.request("shutdown", json!({})).await?;
        Ok(())
    }

    /// Health-check a configured MCP server without importing anything.
    pub async fn health_check(module: &McpModule) -> McpServerHealth {
        let started = std::time::Instant::now();
        match Self::connect(module).await {
            Ok(mut adapter) => {
                let reachable = adapter.ping().await;
                McpServerHealth {
                    reachable,
                    server_info: adapter.server_info.clone(),
                    capabilities: adapter.capabilities.clone(),
                    latency_ms: started.elapsed().as_millis() as u64,
                }
            }
            Err(_) => McpServerHealth {
                reachable: false,
                server_info: Value::Null,
                capabilities: McpCapabilities::default(),
                latency_ms: started.elapsed().as_millis() as u64,
            },
        }
    }

    /// Import everything the server exposes into canonical form.
    pub async fn import(module: &McpModule) -> ImportResult {
        if module.transport != McpTransport::Stdio {
            return ImportResult {
                source: ExtensionSource::Mcp,
                capability: ImportCapability::Incompatible,
                canonical: None,
                warnings: Vec::new(),
                errors: vec![format!(
                    "MCP transport {:?} is not implemented, only stdio is supported",
                    module.transport
                )],
            };
        }

        let mut adapter = match Self::connect(module).await {
            Ok(adapter) => adapter,
            Err(e) => {
                return ImportResult {
                    source: ExtensionSource::Mcp,
                    capability: ImportCapability::Incompatible,
                    canonical: None,
                    warnings: Vec::new(),
                    errors: vec![format!("Failed to connect to MCP server: {}", e)],
                };
            }
        };

        adapter.build_import_result(module).await
    }

    /// Turn an already-connected session into a canonical import result.
    async fn build_import_result(&mut self, module: &McpModule) -> ImportResult {
        let mut warnings = Vec::new();
        let errors = Vec::new();

        let mut capabilities = Vec::new();
        let mut tools: Vec<ToolProvider> = Vec::new();
        let mut resources: Vec<McpResource> = Vec::new();
        let mut prompts: Vec<McpPrompt> = Vec::new();
        let mut instructions: Vec<Instruction> = Vec::new();

        if self.capabilities.tools {
            match self.list_tools().await {
                Ok(list) => {
                    tools = list
                        .iter()
                        .map(|t| ToolProvider {
                            name: t.name.clone(),
                            description: t.description.clone(),
                            schema: if t.input_schema.is_null() {
                                json!({"type": "object"})
                            } else {
                                t.input_schema.clone()
                            },
                            handler: ToolHandler::Mcp {
                                server: module.name.clone(),
                                tool: t.name.clone(),
                            },
                        })
                        .collect();
                    capabilities.push("tools".to_string());
                }
                Err(e) => warnings.push(format!("tools/list failed: {}", e)),
            }
        } else {
            warnings.push("Server does not advertise tools capability".to_string());
        }

        if self.capabilities.resources {
            match self.list_resources().await {
                Ok(list) => {
                    resources = list;
                    if !resources.is_empty() {
                        capabilities.push("resources".to_string());
                    }
                }
                Err(e) => warnings.push(format!("resources/list failed: {}", e)),
            }
        } else {
            warnings.push("Server does not advertise resources capability".to_string());
        }

        if self.capabilities.prompts {
            match self.list_prompts().await {
                Ok(list) => {
                    prompts = list;
                    if !prompts.is_empty() {
                        for p in &prompts {
                            instructions.push(Instruction {
                                content: if p.description.is_empty() {
                                    format!("MCP prompt '{}'", p.name)
                                } else {
                                    format!("{}\n\nMCP prompt: {}", p.description, p.name)
                                },
                                context: InstructionContext::Project,
                                priority: InstructionPriority::Normal,
                            });
                        }
                        capabilities.push("prompts".to_string());
                    }
                }
                Err(e) => warnings.push(format!("prompts/list failed: {}", e)),
            }
        } else {
            warnings.push("Server does not advertise prompts capability".to_string());
        }

        let _ = self.shutdown().await;
        let overall = if capabilities.is_empty() {
            ImportCapability::Unsupported
        } else {
            ImportCapability::Supported
        };

        let mut metadata: HashMap<String, Value> = HashMap::new();
        metadata.insert(
            "server_info".to_string(),
            serde_json::to_value(self.server_info.clone()).unwrap_or_default(),
        );
        metadata.insert(
            "tools".to_string(),
            serde_json::to_value(&tools).unwrap_or_default(),
        );
        metadata.insert(
            "resources".to_string(),
            serde_json::to_value(&resources).unwrap_or_default(),
        );
        metadata.insert(
            "prompts".to_string(),
            serde_json::to_value(&prompts).unwrap_or_default(),
        );
        metadata.insert(
            "instructions".to_string(),
            serde_json::to_value(&instructions).unwrap_or_default(),
        );

        if capabilities.is_empty() {
            warnings.push("Server exposed no importable tools, resources or prompts".to_string());
            return ImportResult {
                source: ExtensionSource::Mcp,
                capability: ImportCapability::Unsupported,
                canonical: None,
                warnings,
                errors,
            };
        }

        let spec = CanonicalModuleSpec {
            id: format!("mcp-{}", module.name),
            name: module.name.clone(),
            version: self
                .server_info
                .get("version")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .to_string(),
            source: ExtensionSource::Mcp,
            kind: CanonicalModuleKind::McpServer,
            capabilities,
            metadata,
        };

        ImportResult {
            source: ExtensionSource::Mcp,
            capability: overall,
            canonical: Some(spec),
            warnings,
            errors,
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use tokio::io::DuplexStream;

    /// In-memory mock MCP server speaking the same framing over a duplex.
    async fn mock_server_loop(stream: DuplexStream) {
        let (read_half, mut write_half) = tokio::io::split(stream);
        let mut reader = tokio::io::BufReader::new(read_half);

        loop {
            let message = match read_mcp_message(&mut reader).await {
                Ok(m) => m,
                Err(_) => return,
            };
            let msg: Value = match serde_json::from_str(&message) {
                Ok(m) => m,
                Err(_) => continue,
            };

            // Notifications: no response expected.
            if msg.get("id").is_none() {
                continue;
            }

            let id = msg["id"].clone();
            let method = msg["method"].as_str().unwrap_or_default().to_string();
            let result: Value = match method.as_str() {
                "initialize" => json!({
                    "protocolVersion": "2024-11-05",
                    "capabilities": {
                        "tools": {"listChanged": false},
                        "resources": {"subscribe": false, "listChanged": false},
                        "prompts": {"listChanged": false}
                    },
                    "serverInfo": {"name": "mock-server", "version": "9.9.9"}
                }),
                "tools/list" => json!({
                    "tools": [
                        {
                            "name": "echo",
                            "description": "Echo text back",
                            "inputSchema": {
                                "type": "object",
                                "properties": {"text": {"type": "string"}},
                                "required": ["text"]
                            },
                            "annotations": {"readOnlyHint": true}
                        },
                        {
                            "name": "sum",
                            "description": "Add two numbers",
                            "inputSchema": {"type": "object"}
                        }
                    ]
                }),
                "tools/call" => {
                    let params = msg.get("params").cloned().unwrap_or(json!({}));
                    let tool_name = params
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    let args = params.get("arguments").cloned().unwrap_or(json!({}));
                    match tool_name {
                        "echo" => {
                            let text = args.get("text").and_then(Value::as_str).unwrap_or_default();
                            json!({
                                "content": [{"type": "text", "text": format!("echo:{text}")}],
                                "isError": false
                            })
                        }
                        "sum" => {
                            let a = args.get("a").and_then(Value::as_i64).unwrap_or(0);
                            let b = args.get("b").and_then(Value::as_i64).unwrap_or(0);
                            json!({
                                "content": [{"type": "text", "text": format!("{}", a + b)}],
                                "isError": false
                            })
                        }
                        other => json!({
                            "content": [{"type": "text", "text": format!("unknown tool: {other}")}],
                            "isError": true
                        }),
                    }
                }
                "resources/list" => json!({
                    "resources": [
                        {"uri": "file:///data/schema.sql", "name": "schema", "description": "DB schema"}
                    ]
                }),
                "prompts/list" => json!({
                    "prompts": [
                        {
                            "name": "review",
                            "description": "Review the current diff",
                            "arguments": [{"name": "scope", "description": "Review scope", "required": false}]
                        }
                    ]
                }),
                "ping" => json!({}),
                "shutdown" => json!({}),
                other => json!({
                    "error": {"code": -32601, "message": format!("method not found: {}", other)}
                }),
            };

            let response = json!({"jsonrpc": "2.0", "id": id, "result": result});
            if write_mcp_message(&mut write_half, &response.to_string())
                .await
                .is_err()
            {
                return;
            }
        }
    }

    pub(crate) async fn duplex_adapter() -> (McpAdapter, tokio::task::JoinHandle<()>) {
        let (client, server) = tokio::io::duplex(8192);
        let server_task = tokio::spawn(mock_server_loop(server));
        let mut adapter = McpAdapter::from_stream(Box::new(MockStream { stream: client })).unwrap();
        adapter.initialize().await.unwrap();
        (adapter, server_task)
    }

    struct MockStream {
        stream: DuplexStream,
    }

    #[async_trait::async_trait]
    impl McpStream for MockStream {
        async fn read_message(&mut self) -> Result<String> {
            read_mcp_message(&mut tokio::io::BufReader::new(&mut self.stream)).await
        }

        async fn write_message(&mut self, message: &str) -> Result<()> {
            write_mcp_message(&mut self.stream, message).await
        }
    }

    #[test]
    fn mcp_module_parses_from_json() {
        let json = r#"{
            "name": "filesystem",
            "command": "npx",
            "args": ["-y", "@modelcontextprotocol/server-filesystem"],
            "env": {"HOME": "/tmp"},
            "transport": "stdio",
            "capabilities": {"tools": true, "resources": true, "prompts": false, "sampling": false}
        }"#;
        let module: McpModule = serde_json::from_str(json).unwrap();
        assert_eq!(module.transport, McpTransport::Stdio);
        assert!(module.capabilities.tools);
        assert_eq!(module.args.len(), 2);
    }

    #[test]
    fn mcp_transport_variants_parse() {
        let http: McpTransport = serde_json::from_str("\"http\"").unwrap();
        let sse: McpTransport = serde_json::from_str("\"sse\"").unwrap();
        assert_eq!(http, McpTransport::Http);
        assert_eq!(sse, McpTransport::Sse);
    }

    #[tokio::test]
    async fn content_length_framing_roundtrip() {
        let messages = [
            r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#,
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}"#,
        ];
        let mut buf: Vec<u8> = Vec::new();
        for m in &messages {
            write_mcp_message(&mut buf, m).await.unwrap();
        }

        let mut cursor = tokio::io::BufReader::new(std::io::Cursor::new(buf));
        let first = read_mcp_message(&mut cursor).await.unwrap();
        let second = read_mcp_message(&mut cursor).await.unwrap();
        assert_eq!(first, messages[0]);
        assert_eq!(second, messages[1]);
    }

    #[tokio::test]
    async fn line_based_fallback_reads_bare_json() {
        let line = r#"{"jsonrpc":"2.0","id":3,"method":"ping"}"#;
        let mut buf: Vec<u8> = format!("{}\n", line).into_bytes();
        buf.extend_from_slice("Content-Length: 2\r\n\r\n{}".as_bytes());

        let mut cursor = tokio::io::BufReader::new(std::io::Cursor::new(buf));
        assert_eq!(read_mcp_message(&mut cursor).await.unwrap(), line);
        assert_eq!(read_mcp_message(&mut cursor).await.unwrap(), "{}");
    }

    #[test]
    fn unsupported_transport_rejected() {
        let module = McpModule {
            name: "http-server".to_string(),
            command: "echo".to_string(),
            args: vec![],
            env: HashMap::new(),
            transport: McpTransport::Http,
            capabilities: McpCapabilities::default(),
        };
        let result = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(McpAdapter::import(&module));
        assert_eq!(result.capability, ImportCapability::Incompatible);
        assert!(result.canonical.is_none());
        assert!(!result.errors.is_empty());
    }

    #[tokio::test]
    async fn lists_tools_resources_prompts() {
        let (mut adapter, server_task) = duplex_adapter().await;

        let tools = adapter.list_tools().await.unwrap();
        assert_eq!(tools.len(), 2);
        assert_eq!(tools[0].name, "echo");
        assert_eq!(
            tools[0].input_schema["properties"]["text"]["type"],
            "string"
        );
        assert_eq!(tools[0].annotations.as_ref().unwrap()["readOnlyHint"], true);

        let resources = adapter.list_resources().await.unwrap();
        assert_eq!(resources.len(), 1);
        assert_eq!(resources[0].name, "schema");

        let prompts = adapter.list_prompts().await.unwrap();
        assert_eq!(prompts.len(), 1);
        assert_eq!(prompts[0].name, "review");
        assert_eq!(prompts[0].arguments[0].name, "scope");

        adapter.shutdown().await.unwrap();
        server_task.abort();
    }

    #[tokio::test]
    async fn call_tool_echo_roundtrip() {
        let (mut adapter, server_task) = duplex_adapter().await;
        let result = adapter
            .call_tool("echo", json!({"text": "ping"}))
            .await
            .unwrap();
        assert_eq!(result["isError"], false);
        assert_eq!(result["content"][0]["text"], "echo:ping");
        server_task.abort();
    }

    #[tokio::test]
    async fn ping_reaches_server() {
        let (mut adapter, server_task) = duplex_adapter().await;
        assert!(adapter.ping().await);
        server_task.abort();
    }

    #[tokio::test]
    async fn import_builds_canonical_spec() {
        let module = McpModule {
            name: "mock-server".to_string(),
            command: "ignored".to_string(),
            args: vec![],
            env: HashMap::new(),
            transport: McpTransport::Stdio,
            capabilities: McpCapabilities {
                tools: true,
                resources: true,
                prompts: true,
                sampling: false,
            },
        };

        // Exercise import's shared inner logic against a duplex server.
        let (client, server) = tokio::io::duplex(8192);
        let server_task = tokio::spawn(mock_server_loop(server));
        let mut adapter = McpAdapter::from_stream(Box::new(MockStream { stream: client })).unwrap();
        adapter.initialize().await.unwrap();

        let result = adapter.build_import_result(&module).await;
        assert_eq!(result.capability, ImportCapability::Supported);
        let spec = result.canonical.unwrap();
        assert_eq!(spec.id, "mcp-mock-server");
        assert_eq!(spec.kind, CanonicalModuleKind::McpServer);
        assert_eq!(spec.source, ExtensionSource::Mcp);
        assert!(spec.capabilities.contains(&"tools".to_string()));
        assert_eq!(spec.capabilities.len(), 3);
        assert_eq!(spec.version, "9.9.9");

        let tools: Vec<ToolProvider> =
            serde_json::from_value(spec.metadata["tools"].clone()).unwrap();
        assert_eq!(tools.len(), 2);
        assert!(matches!(tools[0].handler, ToolHandler::Mcp { .. }));

        let instructions: Vec<Instruction> =
            serde_json::from_value(spec.metadata["instructions"].clone()).unwrap();
        assert_eq!(instructions.len(), 1);
        assert!(instructions[0].content.contains("Review the current diff"));

        server_task.abort();
    }
}
