//! Out-of-process `host_process` spawn + newline-delimited JSON-RPC.
//!
//! Crash-isolated: child death does not panic the daemon. Commands never go
//! through a shell. Basename deny-list mirrors RiskGate sudo/escalation tools.
//!
//! Operate surface: request id + typed op/result/error, cancel, host-side
//! timeout, payload limits, reader-thread demux for concurrent in-flight ids.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::path::{Component, Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use impetus_extension_sdk::host_protocol::{
    CancelParams, DEFAULT_OPERATE_TIMEOUT_MS, HOST_PROTOCOL_VERSION, InitializeParams,
    InitializeResult, JsonRpcRequest, JsonRpcResponse, METHOD_CANCEL, METHOD_INITIALIZE,
    METHOD_OPERATE, METHOD_SHUTDOWN, OperateParams, OperateResult, check_rpc_line_size,
    error_codes, reject_secret_keys,
};
use impetus_extension_sdk::{CURRENT_SUPPORTED_RANGE, ExtensionApiVersion, check_compatibility};
use serde_json::Value;
use thiserror::Error;

type PendingMap = Arc<Mutex<HashMap<u64, Sender<RawLine>>>>;

struct RawLine {
    line: String,
}

/// Live child for an Active `host_process` package.
pub struct HostProcessSession {
    pub extension_id: String,
    child: Child,
    stdin: Arc<Mutex<ChildStdin>>,
    next_id: AtomicU64,
    pending: PendingMap,
    reader_alive: Arc<AtomicBool>,
    reader: Option<JoinHandle<()>>,
    /// Ops advertised during initialize (empty = no pre-filter).
    pub supported_ops: Vec<String>,
    pub negotiated_api_version: u32,
}

impl std::fmt::Debug for HostProcessSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HostProcessSession")
            .field("extension_id", &self.extension_id)
            .field("pid", &self.child.id())
            .field("supported_ops", &self.supported_ops)
            .field("negotiated_api_version", &self.negotiated_api_version)
            .finish()
    }
}

#[derive(Debug, Error)]
pub enum HostProcessError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Denied(String),
    #[error("host_process initialize failed: {0}")]
    Handshake(String),
    #[error("host_process protocol error: {0}")]
    Protocol(String),
    #[error("host_process operate timed out")]
    Timeout,
    #[error("host_process operate cancelled")]
    Cancelled,
    #[error("host_process child crashed")]
    Crashed,
    #[error("host_process payload too large: {0}")]
    PayloadTooLarge(String),
    #[error("host_process secrets forbidden: {0}")]
    SecretsForbidden(String),
    #[error("host_process unsupported op: {0}")]
    UnsupportedOp(String),
    #[error("host_process RPC error {code}: {message}")]
    Rpc { code: i64, message: String },
}

impl HostProcessError {
    pub fn is_crash(&self) -> bool {
        matches!(self, Self::Crashed | Self::Io(_))
    }
}

/// Spawn package command, run `extension/initialize`, return live session.
pub fn spawn_and_initialize(
    package_path: &Path,
    extension_id: &str,
    extension_api_version: u32,
    command: &str,
    args: &[String],
) -> Result<HostProcessSession, HostProcessError> {
    validate_spawn_argv(command, args)?;
    let resolved = resolve_command(package_path, command)?;

    let mut child = Command::new(&resolved)
        .args(args)
        .current_dir(package_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;

    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| HostProcessError::Handshake("stdin missing".into()))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| HostProcessError::Handshake("stdout missing".into()))?;

    let pending: PendingMap = Arc::new(Mutex::new(HashMap::new()));
    let reader_alive = Arc::new(AtomicBool::new(true));
    let reader = spawn_reader(stdout, Arc::clone(&pending), Arc::clone(&reader_alive));

    let mut session = HostProcessSession {
        extension_id: extension_id.to_string(),
        child,
        stdin: Arc::new(Mutex::new(stdin)),
        next_id: AtomicU64::new(1),
        pending,
        reader_alive,
        reader: Some(reader),
        supported_ops: Vec::new(),
        negotiated_api_version: extension_api_version,
    };

    match session.initialize(extension_api_version) {
        Ok(()) => Ok(session),
        Err(err) => {
            session.force_cleanup();
            Err(err)
        }
    }
}

fn spawn_reader(
    stdout: ChildStdout,
    pending: PendingMap,
    alive: Arc<AtomicBool>,
) -> JoinHandle<()> {
    thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        let mut buf = String::new();
        loop {
            buf.clear();
            match reader.read_line(&mut buf) {
                Ok(0) => break,
                Ok(_) => {
                    let line = buf.trim_end().to_string();
                    if line.is_empty() {
                        continue;
                    }
                    let id = peek_response_id(&line);
                    if let Some(id) = id {
                        let tx = {
                            let mut map = pending.lock().unwrap_or_else(|e| e.into_inner());
                            map.remove(&id)
                        };
                        if let Some(tx) = tx {
                            let _ = tx.send(RawLine { line });
                        }
                    }
                }
                Err(_) => break,
            }
        }
        alive.store(false, Ordering::SeqCst);
        // Fail any waiters still pending.
        let leftovers = {
            let mut map = pending.lock().unwrap_or_else(|e| e.into_inner());
            std::mem::take(&mut *map)
        };
        for (_id, tx) in leftovers {
            let _ = tx.send(RawLine {
                line: String::new(), // empty → treat as crash/EOF
            });
        }
    })
}

fn peek_response_id(line: &str) -> Option<u64> {
    let v: Value = serde_json::from_str(line).ok()?;
    v.get("id")?.as_u64()
}

impl HostProcessSession {
    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    pub fn is_reader_alive(&self) -> bool {
        self.reader_alive.load(Ordering::SeqCst)
    }

    fn alloc_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::SeqCst)
    }

    fn write_line(&self, line: &str) -> Result<(), HostProcessError> {
        check_rpc_line_size(line).map_err(HostProcessError::PayloadTooLarge)?;
        let mut stdin = self
            .stdin
            .lock()
            .map_err(|_| HostProcessError::Protocol("stdin lock poisoned".into()))?;
        writeln!(stdin, "{line}")?;
        stdin.flush()?;
        Ok(())
    }

    fn register_pending(&self, id: u64) -> Receiver<RawLine> {
        let (tx, rx) = mpsc::channel();
        let mut map = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        map.insert(id, tx);
        rx
    }

    fn unregister_pending(&self, id: u64) {
        let mut map = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        map.remove(&id);
    }

    fn wait_line(
        &self,
        id: u64,
        rx: Receiver<RawLine>,
        timeout: Duration,
    ) -> Result<String, HostProcessError> {
        match rx.recv_timeout(timeout) {
            Ok(raw) => {
                if raw.line.is_empty() {
                    return Err(HostProcessError::Crashed);
                }
                check_rpc_line_size(&raw.line).map_err(HostProcessError::PayloadTooLarge)?;
                Ok(raw.line)
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                self.unregister_pending(id);
                Err(HostProcessError::Timeout)
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(HostProcessError::Crashed),
        }
    }

    fn initialize(&mut self, extension_api_version: u32) -> Result<(), HostProcessError> {
        let id = self.alloc_id();
        let req = JsonRpcRequest::new(
            id,
            METHOD_INITIALIZE,
            InitializeParams {
                protocol_version: HOST_PROTOCOL_VERSION,
                extension_id: self.extension_id.clone(),
                extension_api_version,
            },
        );
        let line =
            serde_json::to_string(&req).map_err(|e| HostProcessError::Protocol(e.to_string()))?;
        let rx = self.register_pending(id);
        self.write_line(&line)?;
        let response_line = self.wait_line(id, rx, Duration::from_secs(10))?;
        let resp: JsonRpcResponse<InitializeResult> = serde_json::from_str(&response_line)
            .map_err(|e| HostProcessError::Protocol(format!("parse initialize: {e}")))?;
        if resp.id != id {
            return Err(HostProcessError::Protocol(format!(
                "initialize id mismatch: got {} want {id}",
                resp.id
            )));
        }
        if let Some(err) = resp.error {
            return Err(HostProcessError::Handshake(format!(
                "RPC error {}: {}",
                err.code, err.message
            )));
        }
        let Some(result) = resp.result else {
            return Err(HostProcessError::Handshake(
                "initialize missing result".into(),
            ));
        };
        if result.protocol_version != HOST_PROTOCOL_VERSION {
            return Err(HostProcessError::Handshake(format!(
                "unsupported protocol_version {} (want {HOST_PROTOCOL_VERSION})",
                result.protocol_version
            )));
        }
        let negotiated = result
            .extension_api_version
            .unwrap_or(extension_api_version);
        check_compatibility(ExtensionApiVersion(negotiated), CURRENT_SUPPORTED_RANGE)
            .map_err(|e| HostProcessError::Handshake(e.to_string()))?;
        self.negotiated_api_version = negotiated;
        self.supported_ops = result.supported_ops;
        Ok(())
    }

    /// Dispatch typed `extension/operate` with host timeout + payload limits.
    pub fn operate(
        &mut self,
        request_id: impl Into<String>,
        op: impl Into<String>,
        params: Value,
        permission: Option<impetus_extension_sdk::ExtensionPermission>,
        timeout: Option<Duration>,
    ) -> Result<OperateResult, HostProcessError> {
        if !self.is_reader_alive() || !self.is_alive() {
            return Err(HostProcessError::Crashed);
        }
        let request_id = request_id.into();
        let op = op.into();
        if request_id.trim().is_empty() {
            return Err(HostProcessError::Protocol(
                "operate request_id must be non-empty".into(),
            ));
        }
        reject_secret_keys(&params).map_err(HostProcessError::SecretsForbidden)?;
        if !self.supported_ops.is_empty() && !self.supported_ops.iter().any(|s| s == &op) {
            return Err(HostProcessError::UnsupportedOp(op));
        }

        let timeout = timeout.unwrap_or(Duration::from_millis(DEFAULT_OPERATE_TIMEOUT_MS));
        let id = self.alloc_id();
        let req = JsonRpcRequest::new(
            id,
            METHOD_OPERATE,
            OperateParams {
                request_id: request_id.clone(),
                op: op.clone(),
                params,
                permission,
            },
        );
        let line =
            serde_json::to_string(&req).map_err(|e| HostProcessError::Protocol(e.to_string()))?;
        let rx = self.register_pending(id);
        if let Err(err) = self.write_line(&line) {
            self.unregister_pending(id);
            return Err(err);
        }

        let response_line = match self.wait_line(id, rx, timeout) {
            Ok(line) => line,
            Err(HostProcessError::Timeout) => {
                // Best-effort cancel; do not wait forever.
                let _ = self.cancel_request(&request_id);
                return Err(HostProcessError::Timeout);
            }
            Err(err) => return Err(err),
        };

        let resp: JsonRpcResponse<OperateResult> = serde_json::from_str(&response_line)
            .map_err(|e| HostProcessError::Protocol(format!("parse operate: {e}")))?;
        if resp.id != id {
            return Err(HostProcessError::Protocol(format!(
                "operate id mismatch: got {} want {id}",
                resp.id
            )));
        }
        if let Some(err) = resp.error {
            return Err(map_rpc_error(err.code, err.message));
        }
        let Some(result) = resp.result else {
            return Err(HostProcessError::Protocol("operate missing result".into()));
        };
        if result.request_id != request_id {
            return Err(HostProcessError::Protocol(format!(
                "operate request_id mismatch: got {} want {request_id}",
                result.request_id
            )));
        }
        Ok(result)
    }

    /// Send `extension/cancel` for an in-flight operate `request_id`.
    pub fn cancel_request(&mut self, request_id: &str) -> Result<(), HostProcessError> {
        let id = self.alloc_id();
        let req = JsonRpcRequest::new(
            id,
            METHOD_CANCEL,
            CancelParams {
                request_id: request_id.to_string(),
            },
        );
        let line =
            serde_json::to_string(&req).map_err(|e| HostProcessError::Protocol(e.to_string()))?;
        // Fire-and-forget: register briefly so reader does not leak if child replies.
        let rx = self.register_pending(id);
        self.write_line(&line)?;
        let _ = self.wait_line(id, rx, Duration::from_millis(500));
        Ok(())
    }

    /// Best-effort shutdown RPC then kill + join reader.
    pub fn shutdown(mut self) {
        let id = self.alloc_id();
        let req =
            JsonRpcRequest::<serde_json::Value>::new(id, METHOD_SHUTDOWN, serde_json::json!({}));
        if let Ok(line) = serde_json::to_string(&req) {
            let rx = self.register_pending(id);
            let _ = self.write_line(&line);
            let _ = self.wait_line(id, rx, Duration::from_secs(2));
        }
        self.force_cleanup();
    }

    /// Kill child, drain pending, join reader. Safe after crash.
    pub fn force_cleanup(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        self.reader_alive.store(false, Ordering::SeqCst);
        if let Some(handle) = self.reader.take() {
            let _ = handle.join();
        }
        let mut map = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        map.clear();
    }

    pub fn is_alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }
}

impl Drop for HostProcessSession {
    fn drop(&mut self) {
        self.force_cleanup();
    }
}

fn map_rpc_error(code: i64, message: String) -> HostProcessError {
    match code {
        c if c == error_codes::DENIED => HostProcessError::Denied(message),
        c if c == error_codes::TIMEOUT => HostProcessError::Timeout,
        c if c == error_codes::CANCELLED => HostProcessError::Cancelled,
        c if c == error_codes::UNSUPPORTED_OP => HostProcessError::UnsupportedOp(message),
        c if c == error_codes::PAYLOAD_TOO_LARGE => HostProcessError::PayloadTooLarge(message),
        c if c == error_codes::CRASHED => HostProcessError::Crashed,
        c if c == error_codes::SECRETS_FORBIDDEN => HostProcessError::SecretsForbidden(message),
        _ => HostProcessError::Rpc { code, message },
    }
}

fn validate_spawn_argv(command: &str, args: &[String]) -> Result<(), HostProcessError> {
    let cmd = command.trim();
    if cmd.is_empty() {
        return Err(HostProcessError::Denied("empty command".into()));
    }
    if crate::risk_gate::is_opaque_shell(Some(args), cmd) {
        return Err(HostProcessError::Denied(
            "host_process command must not be an opaque shell line".into(),
        ));
    }
    let base = Path::new(cmd)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(cmd)
        .to_ascii_lowercase();
    if matches!(
        base.as_str(),
        "sudo" | "doas" | "su" | "pkexec" | "login" | "su-l"
    ) {
        return Err(HostProcessError::Denied(format!(
            "host_process refuses escalation binary `{base}`"
        )));
    }
    Ok(())
}

fn resolve_command(package_path: &Path, command: &str) -> Result<PathBuf, HostProcessError> {
    let path = Path::new(command);
    if path.is_absolute() {
        return Err(HostProcessError::Denied(
            "host_process command must be a PATH binary name or package-relative path (no absolute)"
                .into(),
        ));
    }
    if command.contains('/') || command.contains('\\') {
        if path.components().any(|c| matches!(c, Component::ParentDir)) {
            return Err(HostProcessError::Denied(
                "host_process relative command must not contain `..`".into(),
            ));
        }
        let candidate = package_path.join(path);
        if !candidate.starts_with(package_path) {
            return Err(HostProcessError::Denied(
                "host_process command escapes package directory".into(),
            ));
        }
        return Ok(candidate);
    }
    Ok(PathBuf::from(command))
}

#[cfg(test)]
mod tests {
    use super::*;
    use impetus_extension_sdk::ExtensionPermission;
    use impetus_extension_sdk::host_protocol::ops;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::time::Duration;

    fn write_echo_fixture(dir: &Path) {
        let script = dir.join("ext.sh");
        fs::write(
            &script,
            r#"#!/bin/sh
while IFS= read -r line; do
  case "$line" in
    *extension/initialize*)
      id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p' | head -1)
      [ -z "$id" ] && id=1
      printf '%s\n' '{"jsonrpc":"2.0","id":'"$id"',"result":{"protocol_version":1,"name":"fixture","extension_api_version":1,"supported_ops":["echo","invoke"]}}'
      ;;
    *extension/operate*)
      id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p' | head -1)
      rid=$(printf '%s' "$line" | sed -n 's/.*"request_id":"\([^"]*\)".*/\1/p' | head -1)
      [ -z "$id" ] && id=1
      [ -z "$rid" ] && rid=unknown
      case "$line" in
        *'"op":"echo"'*)
          printf '%s\n' '{"jsonrpc":"2.0","id":'"$id"',"result":{"request_id":"'"$rid"'","op":"echo","data":{"ok":true}}}'
          ;;
        *)
          printf '%s\n' '{"jsonrpc":"2.0","id":'"$id"',"error":{"code":-32013,"message":"unsupported op"}}'
          ;;
      esac
      ;;
    *extension/cancel*)
      id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p' | head -1)
      [ -z "$id" ] && id=1
      printf '%s\n' '{"jsonrpc":"2.0","id":'"$id"',"result":null}'
      ;;
    *extension/shutdown*)
      id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p' | head -1)
      [ -z "$id" ] && id=1
      printf '%s\n' '{"jsonrpc":"2.0","id":'"$id"',"result":null}'
      exit 0
      ;;
  esac
done
"#,
        )
        .unwrap();
        let mut perms = fs::metadata(&script).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&script, perms).unwrap();
    }

    fn write_crash_fixture(dir: &Path) {
        let script = dir.join("crash.sh");
        fs::write(
            &script,
            r#"#!/bin/sh
while IFS= read -r line; do
  case "$line" in
    *extension/initialize*)
      id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p' | head -1)
      [ -z "$id" ] && id=1
      printf '%s\n' '{"jsonrpc":"2.0","id":'"$id"',"result":{"protocol_version":1,"name":"crash","supported_ops":["echo"]}}'
      ;;
    *extension/operate*)
      exit 1
      ;;
  esac
done
"#,
        )
        .unwrap();
        let mut perms = fs::metadata(&script).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&script, perms).unwrap();
    }

    #[test]
    fn spawn_initialize_operate_shutdown() {
        let tmp = tempfile::tempdir().unwrap();
        write_echo_fixture(tmp.path());
        let mut session =
            spawn_and_initialize(tmp.path(), "demo", 1, "./ext.sh", &[]).expect("spawn");
        assert!(session.pid() > 0);
        assert!(session.supported_ops.iter().any(|o| o == "echo"));
        let result = session
            .operate(
                "req-1",
                ops::ECHO,
                serde_json::json!({"message": "hi"}),
                None,
                Some(Duration::from_secs(5)),
            )
            .expect("operate");
        assert_eq!(result.request_id, "req-1");
        assert_eq!(result.op, "echo");
        assert_eq!(result.data["ok"], true);
        session.shutdown();
    }

    #[test]
    fn operate_timeout_when_supported() {
        // Fixture that advertises hang.
        let tmp = tempfile::tempdir().unwrap();
        let script = tmp.path().join("ext.sh");
        fs::write(
            &script,
            r#"#!/bin/sh
while IFS= read -r line; do
  case "$line" in
    *extension/initialize*)
      id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p' | head -1)
      printf '%s\n' '{"jsonrpc":"2.0","id":'"$id"',"result":{"protocol_version":1,"supported_ops":["hang"]}}'
      ;;
    *extension/operate*)
      sleep 30
      ;;
    *extension/cancel*)
      id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p' | head -1)
      printf '%s\n' '{"jsonrpc":"2.0","id":'"$id"',"result":null}'
      ;;
    *extension/shutdown*)
      exit 0
      ;;
  esac
done
"#,
        )
        .unwrap();
        let mut perms = fs::metadata(&script).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&script, perms).unwrap();

        let mut session =
            spawn_and_initialize(tmp.path(), "demo", 1, "./ext.sh", &[]).expect("spawn");
        let err = session
            .operate(
                "hang-1",
                "hang",
                serde_json::json!({}),
                Some(ExtensionPermission::ProcessSpawn),
                Some(Duration::from_millis(300)),
            )
            .unwrap_err();
        assert!(matches!(err, HostProcessError::Timeout), "got {err}");
        session.force_cleanup();
    }

    #[test]
    fn crash_on_operate_is_cleanup() {
        let tmp = tempfile::tempdir().unwrap();
        write_crash_fixture(tmp.path());
        let mut session =
            spawn_and_initialize(tmp.path(), "crash", 1, "./crash.sh", &[]).expect("spawn");
        let err = session
            .operate(
                "c1",
                ops::ECHO,
                serde_json::json!({}),
                None,
                Some(Duration::from_secs(3)),
            )
            .unwrap_err();
        assert!(
            err.is_crash() || matches!(err, HostProcessError::Crashed),
            "got {err}"
        );
        session.force_cleanup();
        assert!(!session.is_alive());
    }

    #[test]
    fn rejects_secrets_in_params() {
        let tmp = tempfile::tempdir().unwrap();
        write_echo_fixture(tmp.path());
        let mut session =
            spawn_and_initialize(tmp.path(), "demo", 1, "./ext.sh", &[]).expect("spawn");
        let err = session
            .operate(
                "s1",
                ops::ECHO,
                serde_json::json!({"api_key": "nope"}),
                None,
                Some(Duration::from_secs(2)),
            )
            .unwrap_err();
        assert!(matches!(err, HostProcessError::SecretsForbidden(_)));
        session.shutdown();
    }

    #[test]
    fn rejects_sudo() {
        let err = spawn_and_initialize(
            Path::new("/tmp"),
            "x",
            1,
            "sudo",
            &["-n".into(), "true".into()],
        )
        .unwrap_err();
        assert!(err.to_string().contains("escalation") || err.to_string().contains("sudo"));
    }

    #[test]
    fn rejects_absolute_command() {
        let err = spawn_and_initialize(Path::new("/tmp"), "x", 1, "/bin/echo", &[]).unwrap_err();
        assert!(err.to_string().contains("absolute"));
    }
}
