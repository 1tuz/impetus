//! Shared helpers for real-`impetusd` Unix-socket integration tests.

#![allow(dead_code)] // helpers used by sibling test binaries selectively

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use tempfile::TempDir;

/// Owned daemon process + temp data dir / socket. Kills child on drop.
pub struct DaemonFixture {
    pub data_dir: TempDir,
    pub socket: PathBuf,
    child: Child,
}

impl DaemonFixture {
    /// Spawn real `impetusd` binary (Cargo sets `CARGO_BIN_EXE_impetusd`).
    pub fn spawn() -> Self {
        Self::spawn_with_args(&[] as &[&str], &[])
    }

    /// Like [`Self::spawn`], with CLI args (e.g. `--acp-profile PATH`) and env.
    /// When `keep_stderr` is true, child stderr stays piped (caller must drain).
    pub fn spawn_with_args(args: &[&str], extra_env: &[(&str, &str)]) -> Self {
        Self::spawn_with_args_stderr(args, extra_env, false)
    }

    pub fn spawn_with_args_stderr(
        args: &[&str],
        extra_env: &[(&str, &str)],
        keep_stderr: bool,
    ) -> Self {
        let data_dir = tempfile::tempdir().expect("temp data dir");
        let socket = data_dir.path().join("harness.sock");
        let bin = impetusd_bin();
        let mut cmd = Command::new(&bin);
        cmd.args(args)
            .env("IMPETUS_DATA_DIR", data_dir.path())
            .env("IMPETUS_SOCKET", &socket)
            .env("IMPETUS_NONINTERACTIVE", "1")
            .env("CI", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        for (key, value) in extra_env {
            cmd.env(key, value);
        }
        let mut child = cmd.spawn().unwrap_or_else(|e| panic!("spawn {bin}: {e}"));
        wait_for_socket_or_child(&socket, &mut child, Duration::from_secs(15));
        if !keep_stderr {
            // Drop stderr pipe after ready so Drop kill does not block on a full pipe.
            let _ = child.stderr.take();
        }
        Self {
            data_dir,
            socket,
            child,
        }
    }

    pub fn kill_in_place(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }

    /// Restart daemon on same data dir after unlink of stale socket.
    pub fn restart_after_kill(&mut self) {
        self.kill_in_place();
        if self.socket.exists() {
            let _ = std::fs::remove_file(&self.socket);
        }
        let bin = impetusd_bin();
        let mut child = Command::new(&bin)
            .env("IMPETUS_DATA_DIR", self.data_dir.path())
            .env("IMPETUS_SOCKET", &self.socket)
            .env("IMPETUS_NONINTERACTIVE", "1")
            .env("CI", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap_or_else(|e| panic!("respawn {bin}: {e}"));
        wait_for_socket_or_child(&self.socket, &mut child, Duration::from_secs(10));
        self.child = child;
    }
}

impl Drop for DaemonFixture {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

pub fn impetusd_bin() -> String {
    if let Ok(path) = std::env::var("CARGO_BIN_EXE_impetusd") {
        return path;
    }
    // Fallback when invoked outside cargo-test (manual debug).
    let mut candidates = Vec::new();
    if let Ok(manifest) = std::env::var("CARGO_MANIFEST_DIR") {
        let root = Path::new(&manifest).join("../..");
        candidates.push(root.join("target/debug/impetusd"));
        candidates.push(root.join("target/release/impetusd"));
    }
    for path in candidates {
        if path.is_file() {
            return path.display().to_string();
        }
    }
    panic!("impetusd binary not found; run via `cargo test -p impetusd`");
}

pub fn wait_for_socket(socket: &Path, timeout: Duration) {
    let start = Instant::now();
    loop {
        if socket.exists() && std::os::unix::net::UnixStream::connect(socket).is_ok() {
            return;
        }
        if start.elapsed() > timeout {
            panic!(
                "daemon socket not ready within {:?}: {}",
                timeout,
                socket.display()
            );
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// Like [`wait_for_socket`], but fails fast if the child exits before accept.
pub fn wait_for_socket_or_child(socket: &Path, child: &mut Child, timeout: Duration) {
    let start = Instant::now();
    loop {
        if socket.exists() && std::os::unix::net::UnixStream::connect(socket).is_ok() {
            return;
        }
        if let Ok(Some(status)) = child.try_wait() {
            let stderr = child
                .stderr
                .take()
                .and_then(|mut pipe| {
                    let mut buf = String::new();
                    std::io::Read::read_to_string(&mut pipe, &mut buf).ok()?;
                    Some(buf)
                })
                .unwrap_or_default();
            panic!(
                "impetusd exited before socket ready ({status}): {}\nstderr:\n{stderr}",
                socket.display()
            );
        }
        if start.elapsed() > timeout {
            let _ = child.kill();
            panic!(
                "daemon socket not ready within {:?}: {}",
                timeout,
                socket.display()
            );
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

pub fn workspace_root() -> PathBuf {
    std::env::current_dir()
        .expect("cwd")
        .canonicalize()
        .expect("canonicalize cwd")
}
