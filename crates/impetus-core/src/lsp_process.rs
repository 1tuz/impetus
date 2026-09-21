//! Minimal JSON-RPC LSP stdio client for optional real process spawn (#282 / #311).

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::Mutex;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::coding_tools::{
    CodingDiagnostic, CodingToolsError, CodingToolsProvider, DocumentSymbol, HoverInfo,
    PositionQuery, SourceLocation, SourceRange,
};
use crate::lsp_backend::{
    LSP_BACKEND_NOT_IMPLEMENTED, LspBackendFamily, LspBackendHandshake, LspBackendLaunchHint,
    LspBackendModule,
};

/// Live LSP process when binary exists; otherwise fail-closed Unavailable.
pub struct ProcessLspBackend {
    family: LspBackendFamily,
    launch: LspBackendLaunchHint,
    session: Mutex<Option<LspSession>>,
}

struct LspSession {
    child: Child,
    stdin: ChildStdin,
    stdout: ChildStdout,
    next_id: u64,
}

impl ProcessLspBackend {
    pub fn from_module(module: &LspBackendModule) -> Self {
        Self {
            family: module.family(),
            launch: module.launch_hint().clone(),
            session: Mutex::new(None),
        }
    }

    pub fn rust_analyzer(launch: LspBackendLaunchHint) -> Self {
        Self {
            family: LspBackendFamily::RustAnalyzer,
            launch,
            session: Mutex::new(None),
        }
    }

    fn resolve_binary(&self) -> Result<PathBuf, CodingToolsError> {
        let path = self.launch.binary_path.clone().ok_or_else(|| {
            CodingToolsError::Unavailable("LSP binary path not configured (fail-closed)".into())
        })?;
        if !path.is_file() {
            return Err(CodingToolsError::Unavailable(format!(
                "LSP binary missing: {}",
                path.display()
            )));
        }
        Ok(path)
    }

    fn ensure_session(&self) -> Result<(), CodingToolsError> {
        let mut guard = self.session.lock().expect("lsp session");
        if guard.is_some() {
            return Ok(());
        }
        let binary = self.resolve_binary()?;
        let mut child = Command::new(&binary)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| CodingToolsError::Unavailable(format!("LSP spawn failed: {e}")))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| CodingToolsError::Unavailable("LSP stdin missing".into()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| CodingToolsError::Unavailable("LSP stdout missing".into()))?;
        let mut session = LspSession {
            child,
            stdin,
            stdout,
            next_id: 1,
        };
        // Minimal initialize handshake.
        let _ = session.request(
            "initialize",
            json!({
                "processId": null,
                "capabilities": {},
                "rootUri": null
            }),
        )?;
        session.notify("initialized", json!({}))?;
        *guard = Some(session);
        Ok(())
    }

    pub fn handshake(&self) -> LspBackendHandshake {
        match self.resolve_binary() {
            Ok(path) => match self.ensure_session() {
                Ok(()) => LspBackendHandshake {
                    family_id: self.family.id().into(),
                    ready: true,
                    spawn_implemented: true,
                    binary_path: Some(path),
                    reason: "lsp process spawned".into(),
                },
                Err(err) => LspBackendHandshake {
                    family_id: self.family.id().into(),
                    ready: false,
                    spawn_implemented: true,
                    binary_path: Some(path),
                    reason: err.to_string(),
                },
            },
            Err(err) => LspBackendHandshake {
                family_id: self.family.id().into(),
                ready: false,
                spawn_implemented: false,
                binary_path: self.launch.binary_path.clone(),
                reason: err.to_string(),
            },
        }
    }
}

impl LspSession {
    fn write_message(&mut self, body: &Value) -> Result<(), CodingToolsError> {
        let payload = serde_json::to_vec(body)
            .map_err(|e| CodingToolsError::Unavailable(format!("lsp encode: {e}")))?;
        let header = format!("Content-Length: {}\r\n\r\n", payload.len());
        self.stdin
            .write_all(header.as_bytes())
            .and_then(|_| self.stdin.write_all(&payload))
            .and_then(|_| self.stdin.flush())
            .map_err(|e| CodingToolsError::Unavailable(format!("lsp write: {e}")))
    }

    fn read_message(&mut self) -> Result<Value, CodingToolsError> {
        // Read headers until blank line.
        let mut header = Vec::new();
        let mut buf = [0u8; 1];
        loop {
            self.stdout
                .read_exact(&mut buf)
                .map_err(|e| CodingToolsError::Unavailable(format!("lsp read: {e}")))?;
            header.push(buf[0]);
            if header.ends_with(b"\r\n\r\n") {
                break;
            }
            if header.len() > 4096 {
                return Err(CodingToolsError::Unavailable("lsp header too large".into()));
            }
        }
        let header_str = String::from_utf8_lossy(&header);
        let mut content_length = None;
        for line in header_str.lines() {
            let lower = line.to_ascii_lowercase();
            if let Some(rest) = lower.strip_prefix("content-length:") {
                content_length = rest.trim().parse::<usize>().ok();
            }
        }
        let len = content_length
            .ok_or_else(|| CodingToolsError::Unavailable("lsp missing Content-Length".into()))?;
        let mut body = vec![0u8; len];
        self.stdout
            .read_exact(&mut body)
            .map_err(|e| CodingToolsError::Unavailable(format!("lsp body: {e}")))?;
        serde_json::from_slice(&body)
            .map_err(|e| CodingToolsError::Unavailable(format!("lsp json: {e}")))
    }

    fn request(&mut self, method: &str, params: Value) -> Result<Value, CodingToolsError> {
        let id = self.next_id;
        self.next_id += 1;
        self.write_message(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params
        }))?;
        // Drain until matching id (skip notifications).
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            let msg = self.read_message()?;
            if msg.get("id").and_then(|v| v.as_u64()) == Some(id) {
                if let Some(err) = msg.get("error") {
                    return Err(CodingToolsError::Unavailable(format!("lsp error: {err}")));
                }
                return Ok(msg.get("result").cloned().unwrap_or(Value::Null));
            }
        }
        Err(CodingToolsError::Unavailable("lsp request timeout".into()))
    }

    fn notify(&mut self, method: &str, params: Value) -> Result<(), CodingToolsError> {
        self.write_message(&json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params
        }))
    }
}

impl Drop for ProcessLspBackend {
    fn drop(&mut self) {
        if let Ok(mut guard) = self.session.lock()
            && let Some(mut session) = guard.take()
        {
            let _ = session.notify("exit", Value::Null);
            let _ = session.child.kill();
            let _ = session.child.wait();
        }
    }
}

fn path_to_uri(path: &Path) -> String {
    format!("file://{}", path.display())
}

fn locations_from_lsp(value: &Value) -> Vec<SourceLocation> {
    let items = if value.is_array() {
        value.as_array().cloned().unwrap_or_default()
    } else if value.is_object() {
        vec![value.clone()]
    } else {
        Vec::new()
    };
    items
        .into_iter()
        .filter_map(|item| {
            let uri = item.get("uri")?.as_str()?;
            let path = uri.strip_prefix("file://").unwrap_or(uri);
            let range = item.get("range")?;
            let start = range.get("start")?;
            let end = range.get("end")?;
            Some(SourceLocation::new(
                path,
                SourceRange::new(
                    start.get("line")?.as_u64()? as u32,
                    start.get("character")?.as_u64()? as u32,
                    end.get("line")?.as_u64()? as u32,
                    end.get("character")?.as_u64()? as u32,
                ),
            ))
        })
        .collect()
}

#[async_trait]
impl CodingToolsProvider for ProcessLspBackend {
    async fn definition(
        &self,
        query: &PositionQuery,
    ) -> Result<Vec<SourceLocation>, CodingToolsError> {
        self.ensure_session()?;
        let mut guard = self.session.lock().expect("lsp session");
        let session = guard
            .as_mut()
            .ok_or_else(|| CodingToolsError::Unavailable(LSP_BACKEND_NOT_IMPLEMENTED.into()))?;
        let result = session.request(
            "textDocument/definition",
            json!({
                "textDocument": { "uri": path_to_uri(&query.path) },
                "position": {
                    "line": query.position.line,
                    "character": query.position.character
                }
            }),
        )?;
        Ok(locations_from_lsp(&result))
    }

    async fn references(
        &self,
        query: &PositionQuery,
    ) -> Result<Vec<SourceLocation>, CodingToolsError> {
        self.ensure_session()?;
        let mut guard = self.session.lock().expect("lsp session");
        let session = guard
            .as_mut()
            .ok_or_else(|| CodingToolsError::Unavailable(LSP_BACKEND_NOT_IMPLEMENTED.into()))?;
        let result = session.request(
            "textDocument/references",
            json!({
                "textDocument": { "uri": path_to_uri(&query.path) },
                "position": {
                    "line": query.position.line,
                    "character": query.position.character
                },
                "context": { "includeDeclaration": true }
            }),
        )?;
        Ok(locations_from_lsp(&result))
    }

    async fn diagnostics(&self, _path: &Path) -> Result<Vec<CodingDiagnostic>, CodingToolsError> {
        Err(CodingToolsError::Unavailable(
            "LSP diagnostics push-only in this slice".into(),
        ))
    }

    async fn symbols(&self, _path: &Path) -> Result<Vec<DocumentSymbol>, CodingToolsError> {
        Err(CodingToolsError::Unavailable(
            "LSP documentSymbol not wired in this slice".into(),
        ))
    }

    async fn hover(&self, query: &PositionQuery) -> Result<Option<HoverInfo>, CodingToolsError> {
        self.ensure_session()?;
        let mut guard = self.session.lock().expect("lsp session");
        let session = guard
            .as_mut()
            .ok_or_else(|| CodingToolsError::Unavailable(LSP_BACKEND_NOT_IMPLEMENTED.into()))?;
        let result = session.request(
            "textDocument/hover",
            json!({
                "textDocument": { "uri": path_to_uri(&query.path) },
                "position": {
                    "line": query.position.line,
                    "character": query.position.character
                }
            }),
        )?;
        if result.is_null() {
            return Ok(None);
        }
        let contents = result
            .get("contents")
            .map(|c| {
                if let Some(s) = c.as_str() {
                    s.to_string()
                } else if let Some(obj) = c.as_object() {
                    obj.get("value")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string()
                } else if let Some(arr) = c.as_array() {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(str::to_string))
                        .collect::<Vec<_>>()
                        .join("\n")
                } else {
                    String::new()
                }
            })
            .unwrap_or_default();
        Ok(Some(HoverInfo {
            contents,
            range: None,
        }))
    }
}

#[cfg(test)]
mod process_tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn missing_binary_fail_closed() {
        let backend = ProcessLspBackend::rust_analyzer(LspBackendLaunchHint::with_runtime_binary(
            "/tmp/impetus-no-such-ra-binary",
        ));
        let hs = backend.handshake();
        assert!(!hs.ready);
        assert!(!hs.spawn_implemented);
    }

    #[test]
    fn present_script_spawns_and_handshakes() {
        let dir = tempdir().unwrap();
        let script = dir.path().join("fake-lsp.sh");
        fs::write(
            &script,
            r#"#!/bin/sh
# Minimal LSP: answer initialize, ignore rest until exit
while true; do
  IFS= read -r line || exit 0
  case "$line" in
    Content-Length:*)
      len=${line#Content-Length: }
      len=$(echo "$len" | tr -d '\r')
      # skip blank
      IFS= read -r _
      body=$(dd bs=1 count="$len" 2>/dev/null)
      if echo "$body" | grep -q '"method":"initialize"'; then
        id=$(echo "$body" | sed -n 's/.*"id":\([0-9]*\).*/\1/p' | head -1)
        resp="{\"jsonrpc\":\"2.0\",\"id\":$id,\"result\":{\"capabilities\":{}}}"
        printf "Content-Length: %s\r\n\r\n%s" "${#resp}" "$resp"
      fi
      ;;
  esac
done
"#,
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(&script).unwrap().permissions();
            perms.set_mode(0o755);
            fs::set_permissions(&script, perms).unwrap();
        }
        let backend =
            ProcessLspBackend::rust_analyzer(LspBackendLaunchHint::with_runtime_binary(script));
        let hs = backend.handshake();
        assert!(hs.spawn_implemented);
        assert!(hs.ready, "handshake reason: {}", hs.reason);
    }
}
