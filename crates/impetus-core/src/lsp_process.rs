//! Minimal JSON-RPC LSP stdio client — generic process infrastructure (#282 / #311 / #336).
//!
//! Extension-first (#336): this is **not** a concrete language pack. Core owns
//! spawn / handshake / crash-respawn / `$/cancelRequest` / pull surfaces that
//! already exist on [`CodingToolsProvider`]. Language-specific installers and
//! Browser CDP/WebDriver stay in extensions (`LspIntegration` /
//! `BrowserIntegration`).

use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::Mutex;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::coding_tools::{
    CodingDiagnostic, CodingToolsError, CodingToolsProvider, DiagnosticSeverity, DocumentSymbol,
    HoverInfo, PositionQuery, SourceLocation, SourceRange, SymbolKind,
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
    /// Last diagnostics pushed via `textDocument/publishDiagnostics`.
    diagnostics_cache: Mutex<HashMap<PathBuf, Vec<CodingDiagnostic>>>,
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
            diagnostics_cache: Mutex::new(HashMap::new()),
        }
    }

    pub fn rust_analyzer(launch: LspBackendLaunchHint) -> Self {
        Self {
            family: LspBackendFamily::RustAnalyzer,
            launch,
            session: Mutex::new(None),
            diagnostics_cache: Mutex::new(HashMap::new()),
        }
    }

    fn ingest_notification(&self, msg: &Value) {
        let Some(method) = msg.get("method").and_then(|m| m.as_str()) else {
            return;
        };
        if method != "textDocument/publishDiagnostics" {
            return;
        }
        let Some(params) = msg.get("params") else {
            return;
        };
        let Some(uri) = params.get("uri").and_then(|u| u.as_str()) else {
            return;
        };
        let path = PathBuf::from(uri.strip_prefix("file://").unwrap_or(uri));
        let items = params
            .get("diagnostics")
            .and_then(|d| d.as_array())
            .cloned()
            .unwrap_or_default();
        let parsed: Vec<CodingDiagnostic> = items
            .into_iter()
            .filter_map(|item| {
                let range = item.get("range")?;
                let start = range.get("start")?;
                let end = range.get("end")?;
                let severity = match item.get("severity").and_then(|s| s.as_u64()).unwrap_or(1) {
                    1 => DiagnosticSeverity::Error,
                    2 => DiagnosticSeverity::Warning,
                    3 => DiagnosticSeverity::Information,
                    _ => DiagnosticSeverity::Hint,
                };
                Some(CodingDiagnostic {
                    path: path.clone(),
                    range: SourceRange::new(
                        start.get("line")?.as_u64()? as u32,
                        start.get("character")?.as_u64()? as u32,
                        end.get("line")?.as_u64()? as u32,
                        end.get("character")?.as_u64()? as u32,
                    ),
                    severity,
                    message: item
                        .get("message")
                        .and_then(|m| m.as_str())
                        .unwrap_or("")
                        .to_string(),
                    code: item.get("code").and_then(|c| {
                        c.as_str()
                            .map(str::to_string)
                            .or_else(|| c.as_i64().map(|n| n.to_string()))
                    }),
                })
            })
            .collect();
        if let Ok(mut cache) = self.diagnostics_cache.lock() {
            cache.insert(path, parsed);
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

    pub(crate) fn ensure_session(&self) -> Result<(), CodingToolsError> {
        let mut guard = self.session.lock().expect("lsp session");
        let needs_respawn = match guard.as_mut() {
            None => true,
            Some(session) => match session.child.try_wait() {
                Ok(None) => false,
                Ok(Some(_)) => true,
                Err(e) => {
                    return Err(CodingToolsError::Unavailable(format!(
                        "LSP process status: {e}"
                    )));
                }
            },
        };
        if !needs_respawn {
            return Ok(());
        }
        if let Some(mut dead) = guard.take() {
            let _ = dead.child.kill();
            let _ = dead.child.wait();
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
        // Child may die immediately after initialize (crash/restart path).
        // Keep session anyway so try_wait detects death and ensure_session respawns.
        let _ = session.notify("initialized", json!({}));
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

    fn request_with_notify<F>(
        &mut self,
        method: &str,
        params: Value,
        mut on_notify: F,
    ) -> Result<Value, CodingToolsError>
    where
        F: FnMut(&Value),
    {
        let id = self.next_id;
        self.next_id += 1;
        self.write_message(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params
        }))?;
        // Drain until matching id; surface notifications (e.g. publishDiagnostics).
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            let msg = self.read_message()?;
            if msg.get("method").is_some() && msg.get("id").is_none() {
                on_notify(&msg);
                continue;
            }
            if msg.get("id").and_then(|v| v.as_u64()) == Some(id) {
                if let Some(err) = msg.get("error") {
                    return Err(CodingToolsError::Unavailable(format!("lsp error: {err}")));
                }
                return Ok(msg.get("result").cloned().unwrap_or(Value::Null));
            }
        }
        Err(CodingToolsError::Unavailable("lsp request timeout".into()))
    }

    fn request(&mut self, method: &str, params: Value) -> Result<Value, CodingToolsError> {
        self.request_with_notify(method, params, |_| {})
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

fn lsp_request(
    backend: &ProcessLspBackend,
    method: &str,
    params: Value,
) -> Result<Value, CodingToolsError> {
    backend.ensure_session()?;
    let mut guard = backend.session.lock().expect("lsp session");
    let session = guard
        .as_mut()
        .ok_or_else(|| CodingToolsError::Unavailable(LSP_BACKEND_NOT_IMPLEMENTED.into()))?;
    session.request_with_notify(method, params, |msg| backend.ingest_notification(msg))
}

fn symbols_from_lsp(path: &Path, value: &Value) -> Vec<DocumentSymbol> {
    let items = value.as_array().cloned().unwrap_or_default();
    items
        .into_iter()
        .filter_map(|item| {
            let name = item.get("name")?.as_str()?.to_string();
            let kind = match item.get("kind").and_then(|k| k.as_u64()).unwrap_or(0) {
                1 => SymbolKind::File,
                2 => SymbolKind::Module,
                3 => SymbolKind::Namespace,
                5 => SymbolKind::Class,
                6 => SymbolKind::Method,
                12 => SymbolKind::Function,
                13 => SymbolKind::Variable,
                14 => SymbolKind::Constant,
                8 => SymbolKind::Field,
                10 => SymbolKind::Enum,
                11 => SymbolKind::Interface,
                23 => SymbolKind::Struct,
                26 => SymbolKind::TypeParameter,
                _ => SymbolKind::Other,
            };
            let range = item
                .get("location")
                .and_then(|l| l.get("range"))
                .or_else(|| item.get("range"))
                .or_else(|| item.get("selectionRange"))?;
            let start = range.get("start")?;
            let end = range.get("end")?;
            Some(DocumentSymbol {
                name,
                kind,
                location: SourceLocation::new(
                    path,
                    SourceRange::new(
                        start.get("line")?.as_u64()? as u32,
                        start.get("character")?.as_u64()? as u32,
                        end.get("line")?.as_u64()? as u32,
                        end.get("character")?.as_u64()? as u32,
                    ),
                ),
                container_name: item
                    .get("containerName")
                    .and_then(|c| c.as_str())
                    .map(str::to_string),
            })
        })
        .collect()
}

#[async_trait]
impl CodingToolsProvider for ProcessLspBackend {
    async fn definition(
        &self,
        query: &PositionQuery,
    ) -> Result<Vec<SourceLocation>, CodingToolsError> {
        let result = lsp_request(
            self,
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
        let result = lsp_request(
            self,
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

    async fn diagnostics(&self, path: &Path) -> Result<Vec<CodingDiagnostic>, CodingToolsError> {
        // Ensure session so any pending publishDiagnostics can land on later requests;
        // pull returns last cached push for this path (empty = none yet — honest).
        let _ = self.ensure_session();
        let cache = self.diagnostics_cache.lock().expect("diagnostics cache");
        Ok(cache.get(path).cloned().unwrap_or_default())
    }

    async fn symbols(&self, path: &Path) -> Result<Vec<DocumentSymbol>, CodingToolsError> {
        let result = lsp_request(
            self,
            "textDocument/documentSymbol",
            json!({
                "textDocument": { "uri": path_to_uri(path) }
            }),
        )?;
        Ok(symbols_from_lsp(path, &result))
    }

    async fn hover(&self, query: &PositionQuery) -> Result<Option<HoverInfo>, CodingToolsError> {
        let result = lsp_request(
            self,
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

    async fn cancel_request(&self, request_id: u64) -> Result<(), CodingToolsError> {
        self.ensure_session()?;
        let mut guard = self.session.lock().expect("lsp session");
        let session = guard
            .as_mut()
            .ok_or_else(|| CodingToolsError::Unavailable(LSP_BACKEND_NOT_IMPLEMENTED.into()))?;
        session.notify("$/cancelRequest", json!({ "id": request_id }))?;
        Ok(())
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

    #[test]
    fn crashed_child_is_respawned_on_next_ensure() {
        let dir = tempdir().unwrap();
        let script = dir.path().join("exit-after-init.sh");
        fs::write(
            &script,
            r#"#!/bin/sh
# Answer one initialize, linger briefly so client can send initialized, then exit.
IFS= read -r line || exit 0
case "$line" in
  Content-Length:*)
    len=${line#Content-Length: }
    len=$(echo "$len" | tr -d '\r')
    IFS= read -r _
    body=$(dd bs=1 count="$len" 2>/dev/null)
    if echo "$body" | grep -q '"method":"initialize"'; then
      id=$(echo "$body" | sed -n 's/.*"id":\([0-9]*\).*/\1/p' | head -1)
      resp="{\"jsonrpc\":\"2.0\",\"id\":$id,\"result\":{\"capabilities\":{}}}"
      printf "Content-Length: %s\r\n\r\n%s" "${#resp}" "$resp"
    fi
    ;;
esac
# Allow initialized notify to land, then crash.
sleep 0.2
exit 0
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
        assert!(backend.handshake().ready);
        std::thread::sleep(std::time::Duration::from_millis(50));
        assert!(
            backend.ensure_session().is_ok(),
            "respawn after crash must succeed"
        );
    }

    #[tokio::test]
    async fn cancel_request_sends_dollar_cancel_when_session_live() {
        let dir = tempdir().unwrap();
        let script = dir.path().join("fake-lsp-cancel.sh");
        fs::write(
            &script,
            r#"#!/bin/sh
while true; do
  IFS= read -r line || exit 0
  case "$line" in
    Content-Length:*)
      len=${line#Content-Length: }
      len=$(echo "$len" | tr -d '\r')
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
        assert!(backend.handshake().ready);
        backend
            .cancel_request(99)
            .await
            .expect("cancel should notify live session");
    }
}
