//! Out-of-process `host_process` spawn + newline-delimited JSON-RPC.
//!
//! Crash-isolated: child death does not panic the daemon. Commands never go
//! through a shell. Basename deny-list mirrors RiskGate sudo/escalation tools.

use std::io::{BufRead, BufReader, Write};
use std::path::{Component, Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use impetus_extension_sdk::host_protocol::{
    HOST_PROTOCOL_VERSION, InitializeParams, InitializeResult, JsonRpcRequest, JsonRpcResponse,
    METHOD_INITIALIZE, METHOD_SHUTDOWN,
};
use thiserror::Error;

/// Live child for an Active `host_process` package.
pub struct HostProcessSession {
    pub extension_id: String,
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
}

impl std::fmt::Debug for HostProcessSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HostProcessSession")
            .field("extension_id", &self.extension_id)
            .field("pid", &self.child.id())
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

    let mut session = HostProcessSession {
        extension_id: extension_id.to_string(),
        child,
        stdin,
        stdout: BufReader::new(stdout),
        next_id: 1,
    };

    match session.initialize(extension_api_version) {
        Ok(()) => Ok(session),
        Err(err) => {
            let _ = session.child.kill();
            let _ = session.child.wait();
            Err(err)
        }
    }
}

impl HostProcessSession {
    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    fn initialize(&mut self, extension_api_version: u32) -> Result<(), HostProcessError> {
        let id = self.next_id;
        self.next_id += 1;
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
        writeln!(self.stdin, "{line}")?;
        self.stdin.flush()?;

        let mut response_line = String::new();
        let n = self.stdout.read_line(&mut response_line)?;
        if n == 0 {
            return Err(HostProcessError::Handshake(
                "child closed stdout during initialize".into(),
            ));
        }
        let response_line = response_line.trim_end();
        let resp: JsonRpcResponse<InitializeResult> = serde_json::from_str(response_line)
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
        Ok(())
    }

    /// Best-effort shutdown RPC then kill.
    pub fn shutdown(mut self) {
        let id = self.next_id;
        let req =
            JsonRpcRequest::<serde_json::Value>::new(id, METHOD_SHUTDOWN, serde_json::json!({}));
        if let Ok(line) = serde_json::to_string(&req) {
            let _ = writeln!(self.stdin, "{line}");
            let _ = self.stdin.flush();
            let mut sink = String::new();
            let _ = self.stdout.read_line(&mut sink);
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }

    pub fn is_alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
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
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    fn write_fixture(dir: &Path) {
        let script = dir.join("ext.sh");
        fs::write(
            &script,
            r#"#!/bin/sh
while IFS= read -r line; do
  case "$line" in
    *extension/initialize*)
      echo '{"jsonrpc":"2.0","id":1,"result":{"protocol_version":1,"name":"fixture"}}'
      ;;
    *extension/shutdown*)
      echo '{"jsonrpc":"2.0","id":2,"result":null}'
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

    #[test]
    fn spawn_initialize_shutdown() {
        let tmp = tempfile::tempdir().unwrap();
        write_fixture(tmp.path());
        let session = spawn_and_initialize(tmp.path(), "demo", 1, "./ext.sh", &[]).expect("spawn");
        assert!(session.pid() > 0);
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
