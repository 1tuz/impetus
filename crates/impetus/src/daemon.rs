use anyhow::{Context, Result, bail};
use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::time::sleep;

/// True when something accepts TCP-style connections on the Unix socket
/// (bind is live). Does **not** run Hello — use before deciding to unlink.
pub fn socket_listening(socket_path: &str) -> bool {
    std::os::unix::net::UnixStream::connect(socket_path).is_ok()
}

/// True when daemon answers harness Hello (negotiated transport ready).
pub async fn is_daemon_running(socket_path: &str) -> bool {
    impetus_client::UnixSocketTransport::connect(socket_path)
        .await
        .is_ok()
}

/// Discover socket path from environment or default data-dir location.
///
/// Order: `IMPETUS_SOCKET` → `$IMPETUS_DATA_DIR/harness.sock` → platform
/// default under `$HOME` (same rules as `impetusd`).
pub fn discover_socket_path() -> String {
    if let Ok(socket) = std::env::var("IMPETUS_SOCKET") {
        return socket;
    }
    default_data_root()
        .join("harness.sock")
        .to_string_lossy()
        .into_owned()
}

/// Platform default data root (userspace only — never `/var` / PrivilegedHelper).
pub fn default_data_root() -> PathBuf {
    if let Ok(dir) = std::env::var("IMPETUS_DATA_DIR") {
        return PathBuf::from(dir);
    }
    let home = PathBuf::from(std::env::var("HOME").expect("HOME not set"));
    if cfg!(target_os = "macos") {
        home.join("Library/Application Support/Impetus")
    } else {
        std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".local/share"))
            .join("impetus")
    }
}

/// Attempt to spawn `impetusd` if not already accepting on `socket_path`.
///
/// Product UX: ordinary `impetus` commands lazy-start the daemon. Manual
/// `impetusd` is for development / debugging / advanced administration.
///
/// Safety:
/// - live socket + protocol `Incompatible` → error (never unlink / respawn)
/// - spawn serialized via exclusive `flock` on `daemon.spawn.lock`
///   (kernel releases flock on holder crash — stale file cannot permanently
///   block autostart; PID metadata is diagnostic only)
pub async fn ensure_daemon_running(socket_path: &str) -> Result<()> {
    ensure_daemon_running_with(socket_path, spawn_impetusd_process).await
}

/// Injectable spawn path for tests. Production uses [`ensure_daemon_running`].
///
/// Returns a live [`std::process::Child`] when the caller must keep the spawn
/// lock until that child exits or becomes ready. Tests that start an in-process
/// Hello stub return `None`.
pub async fn ensure_daemon_running_with<F>(socket_path: &str, spawn_daemon: F) -> Result<()>
where
    F: FnOnce(&str, &Path) -> Result<Option<std::process::Child>>,
{
    if is_daemon_running(socket_path).await {
        return Ok(());
    }

    // Listener up but Hello failed — usually version mismatch. Do not unlink.
    if socket_listening(socket_path) {
        match impetus_client::UnixSocketTransport::connect(socket_path).await {
            Ok(_) => return Ok(()),
            Err(err) => {
                let msg = err.to_string();
                if msg.to_ascii_lowercase().contains("incompatible") {
                    bail!(
                        "impetusd is running at {socket_path} but IPC is incompatible ({msg}). \
                         Upgrade/reinstall matching `impetus` and `impetusd` — refusing to respawn."
                    );
                }
                bail!(
                    "socket {socket_path} accepts connections but harness Hello failed: {msg}. \
                     Inspect with `impetus doctor` or run `impetusd` in the foreground."
                );
            }
        }
    }

    let socket = Path::new(socket_path);
    let data_dir = std::env::var_os("IMPETUS_DATA_DIR")
        .map(PathBuf::from)
        .or_else(|| socket.parent().map(Path::to_path_buf))
        .unwrap_or_else(default_data_root);

    if let Some(parent) = data_dir.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::create_dir_all(&data_dir);

    let lock_path = data_dir.join("daemon.spawn.lock");
    let readiness_budget = Duration::from_secs(5);
    let started = Instant::now();

    // Flock is released when the holder process dies, so a crash after lock
    // create cannot permanently block later CLIs. While a peer holds the lock
    // we poll readiness; if the peer dies mid-spawn we acquire and continue.
    loop {
        if is_daemon_running(socket_path).await {
            return Ok(());
        }

        match SpawnLock::try_acquire(&lock_path)? {
            LockAcquire::Acquired(lock) => {
                // Stale socket only when nothing listens (safe under spawn lock).
                if socket.exists() && !socket_listening(socket_path) {
                    std::fs::remove_file(socket).context("failed to remove stale socket")?;
                }

                // Recheck after lock — peer may have finished.
                if is_daemon_running(socket_path).await {
                    drop(lock);
                    return Ok(());
                }

                let mut child = match spawn_daemon(socket_path, &data_dir) {
                    Ok(child) => child,
                    Err(err) => {
                        drop(lock);
                        return Err(err).context("failed to spawn impetusd");
                    }
                };

                let wait_budget = Duration::from_secs(3);
                let result =
                    wait_until_ready_holding_child(socket_path, wait_budget, &mut child).await;
                // Hold flock until ready or child is gone so a peer cannot start
                // a second authoritative daemon while ours is still binding.
                drop(lock);
                return result.with_context(|| {
                    format!(
                        "impetusd did not become ready at {socket_path}. \
                         For debugging run manually: impetusd"
                    )
                });
            }
            LockAcquire::Busy => {
                if started.elapsed() >= readiness_budget {
                    bail!(
                        "another impetus process holds daemon.spawn.lock but \
                         impetusd is not ready at {socket_path} within {readiness_budget:?}"
                    );
                }
                sleep(Duration::from_millis(300)).await;
            }
        }
    }
}

fn spawn_impetusd_process(
    socket_path: &str,
    data_dir: &Path,
) -> Result<Option<std::process::Child>> {
    let impetusd_path = find_impetusd_binary()?;
    let child = Command::new(&impetusd_path)
        .env("IMPETUS_SOCKET", socket_path)
        .env("IMPETUS_DATA_DIR", data_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("failed to spawn impetusd at {impetusd_path}"))?;
    Ok(Some(child))
}

async fn wait_until_ready_holding_child(
    socket_path: &str,
    budget: Duration,
    child: &mut Option<std::process::Child>,
) -> Result<()> {
    let deadline = Instant::now() + budget;
    loop {
        if is_daemon_running(socket_path).await {
            return Ok(());
        }
        if let Some(proc) = child.as_mut() {
            match proc.try_wait() {
                Ok(Some(status)) => {
                    bail!("impetusd exited before ready ({status})");
                }
                Ok(None) => {}
                Err(err) => bail!("failed to poll impetusd child: {err}"),
            }
        }
        if Instant::now() >= deadline {
            if let Some(proc) = child.as_mut() {
                // Still binding past budget — kill so a peer retry cannot race
                // a second authoritative process against a half-started daemon.
                let _ = proc.kill();
                let _ = proc.wait();
            }
            bail!("daemon not ready within {budget:?}");
        }
        sleep(Duration::from_millis(300)).await;
    }
}

/// Find impetusd binary in PATH or next to impetus binary.
fn find_impetusd_binary() -> Result<String> {
    if let Ok(override_path) = std::env::var("IMPETUS_IMPETUSD_PATH")
        && !override_path.is_empty()
    {
        return Ok(override_path);
    }

    if cfg!(debug_assertions)
        && let Ok(current_exe) = std::env::current_exe()
        && let Some(parent) = current_exe.parent()
    {
        let impetusd = parent.join("impetusd");
        if impetusd.exists() {
            return Ok(impetusd.to_string_lossy().to_string());
        }
    }

    if let Ok(output) = Command::new("which").arg("impetusd").output()
        && output.status.success()
    {
        let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !path.is_empty() {
            return Ok(path);
        }
    }

    if let Ok(current_exe) = std::env::current_exe()
        && let Some(parent) = current_exe.parent()
    {
        let impetusd = parent.join("impetusd");
        if impetusd.exists() {
            return Ok(impetusd.to_string_lossy().to_string());
        }
    }

    bail!(
        "impetusd not found in PATH or next to impetus binary. Install it or ensure it's in PATH."
    )
}

/// Outcome of a non-blocking exclusive flock attempt.
enum LockAcquire {
    Acquired(SpawnLock),
    Busy,
}

/// RAII exclusive spawn lock. Kernel drops flock when the holding process
/// exits (including crash), so a leftover lock *file* cannot permanently
/// block autostart.
struct SpawnLock {
    file: File,
}

impl SpawnLock {
    /// Open/create `path` and take exclusive non-blocking flock.
    ///
    /// Writes owner metadata (pid + timestamp) for diagnostics. Liveness of
    /// the lock is the flock itself — not the pid field (avoids PID-reuse
    /// false ownership).
    fn try_acquire(path: &Path) -> Result<LockAcquire> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)
            .with_context(|| format!("failed to open {}", path.display()))?;

        match flock_exclusive_nb(&file) {
            Ok(()) => {
                let mut lock = SpawnLock { file };
                lock.write_owner_metadata()
                    .context("failed to write daemon.spawn.lock owner metadata")?;
                Ok(LockAcquire::Acquired(lock))
            }
            Err(err) if err.kind() == io::ErrorKind::WouldBlock => Ok(LockAcquire::Busy),
            Err(err) => Err(err).context("failed to flock daemon.spawn.lock"),
        }
    }

    fn write_owner_metadata(&mut self) -> io::Result<()> {
        let pid = std::process::id();
        let acquired_unix_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        self.file.set_len(0)?;
        write!(
            self.file,
            "pid={pid}\nacquired_unix_ms={acquired_unix_ms}\n"
        )?;
        self.file.flush()?;
        Ok(())
    }
}

impl Drop for SpawnLock {
    fn drop(&mut self) {
        // Truncate owner metadata while we still hold the exclusive flock so a
        // peer cannot acquire, write its pid, then lose that metadata to us.
        let _ = self.file.set_len(0);
        let _ = self.file.flush();
        let _ = flock_unlock(&self.file);
    }
}

fn flock_exclusive_nb(file: &File) -> io::Result<()> {
    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if rc == 0 {
        return Ok(());
    }
    let err = io::Error::last_os_error();
    // Normalize EAGAIN/EWOULDBLOCK to WouldBlock for callers.
    if err.raw_os_error() == Some(libc::EWOULDBLOCK) || err.raw_os_error() == Some(libc::EAGAIN) {
        return Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            "daemon.spawn.lock held by another process",
        ));
    }
    Err(err)
}

fn flock_unlock(file: &File) -> io::Result<()> {
    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_UN) };
    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::thread;
    use tempfile::tempdir;

    #[test]
    fn ensure_daemon_spawn_has_no_privilege_escalation() {
        let src = include_str!("daemon.rs");
        let start = src
            .find("pub async fn ensure_daemon_running")
            .expect("ensure_daemon_running present");
        let end = src[start..]
            .find("\nasync fn wait_until_ready_holding_child")
            .map(|i| start + i)
            .unwrap_or(src.len());
        let spawn_path = &src[start..end];
        for needle in [
            "sudo",
            "osascript",
            "withAdministratorPrivileges",
            "AuthorizationCreate",
            "SMJobBless",
            "PrivilegedHelperTools",
        ] {
            assert!(
                !spawn_path.contains(needle),
                "CLI daemon spawn must not escalate via `{needle}`"
            );
        }
        assert!(
            spawn_path.contains("spawn_impetusd_process")
                || spawn_path.contains("Command::new(&impetusd_path)"),
            "CLI must spawn impetusd directly as the current user"
        );
        assert!(
            src.contains("IMPETUS_SOCKET"),
            "lazy-start must pass IMPETUS_SOCKET so client and daemon agree"
        );
        assert!(
            src.contains("IMPETUS_DATA_DIR"),
            "lazy-start must pass IMPETUS_DATA_DIR for durable state colocation"
        );
        assert!(
            spawn_path.contains("daemon.spawn.lock"),
            "lazy-start must serialize concurrent spawn via lock file"
        );
        assert!(
            src.contains("flock") || src.contains("LOCK_EX"),
            "lazy-start must use flock so crash cannot permanently block autostart"
        );
        assert!(
            spawn_path.contains("incompatible"),
            "lazy-start must refuse unlink/respawn on IPC Incompatible"
        );
    }

    #[test]
    fn discover_socket_defaults_under_user_data_dir() {
        let src = include_str!("daemon.rs");
        let start = src
            .find("pub fn discover_socket_path")
            .expect("discover_socket_path present");
        let end = src[start..]
            .find("\npub fn default_data_root")
            .map(|i| start + i)
            .unwrap_or(src.len());
        let discover = &src[start..end];
        assert!(discover.contains("IMPETUS_SOCKET"));
        assert!(discover.contains("harness.sock"));
        assert!(!discover.contains("/var/lib"));
        assert!(!discover.contains("PrivilegedHelperTools"));
    }

    #[test]
    fn default_data_root_is_userspace_only() {
        let src = include_str!("daemon.rs");
        let start = src
            .find("pub fn default_data_root")
            .expect("default_data_root present");
        let end = src[start..]
            .find("\npub async fn ensure_daemon_running")
            .map(|i| start + i)
            .unwrap_or(src.len());
        let body = &src[start..end];
        assert!(
            body.contains("Library/Application Support/Impetus") || body.contains("XDG_DATA_HOME")
        );
        assert!(!body.contains("/var/lib"));
        assert!(!body.contains("PrivilegedHelperTools"));
    }

    #[test]
    fn socket_listening_is_raw_connect_not_hello() {
        let src = include_str!("daemon.rs");
        let start = src
            .find("pub fn socket_listening")
            .expect("socket_listening present");
        let end = src[start..]
            .find("\npub async fn is_daemon_running")
            .map(|i| start + i)
            .unwrap_or(src.len());
        let body = &src[start..end];
        assert!(body.contains("UnixStream::connect"));
        assert!(!body.contains("UnixSocketTransport"));
    }

    #[test]
    fn spawn_lock_acquire_release_and_stale_file() {
        let dir = tempdir().expect("tempdir");
        let lock_path = dir.path().join("daemon.spawn.lock");

        // Stale file with no holder must be reclaimable.
        std::fs::write(&lock_path, b"pid=1\nacquired_unix_ms=0\n").expect("stale write");
        let first = match SpawnLock::try_acquire(&lock_path).expect("acquire") {
            LockAcquire::Acquired(lock) => lock,
            LockAcquire::Busy => panic!("stale lock file must be reclaimable"),
        };
        assert!(lock_path.exists());
        let meta = std::fs::read_to_string(&lock_path).expect("read owner");
        assert!(meta.contains(&format!("pid={}", std::process::id())));

        // Active holder: peer must not steal the lock.
        match SpawnLock::try_acquire(&lock_path).expect("second") {
            LockAcquire::Busy => {}
            LockAcquire::Acquired(_) => panic!("must not reclaim active lock"),
        }

        drop(first);

        // After release, another process can acquire.
        match SpawnLock::try_acquire(&lock_path).expect("reacquire") {
            LockAcquire::Acquired(_lock) => {}
            LockAcquire::Busy => panic!("lock must be free after Drop"),
        }
    }

    #[test]
    fn spawn_lock_raii_releases_on_drop_even_without_explicit_remove() {
        let dir = tempdir().expect("tempdir");
        let lock_path = dir.path().join("daemon.spawn.lock");
        {
            let _lock = match SpawnLock::try_acquire(&lock_path).expect("acquire") {
                LockAcquire::Acquired(lock) => lock,
                LockAcquire::Busy => panic!("expected acquire"),
            };
            // Panic-unwind path also runs Drop; simulate by scope exit.
            assert!(lock_path.exists());
        }
        match SpawnLock::try_acquire(&lock_path).expect("after drop") {
            LockAcquire::Acquired(_) => {}
            LockAcquire::Busy => panic!("RAII drop must unlock"),
        }
    }

    #[test]
    fn concurrent_lock_exactly_one_owner() {
        let dir = tempdir().expect("tempdir");
        let lock_path = Arc::new(dir.path().join("daemon.spawn.lock"));
        let winners = Arc::new(AtomicUsize::new(0));
        let mut handles = Vec::new();
        for _ in 0..8 {
            let path = Arc::clone(&lock_path);
            let winners = Arc::clone(&winners);
            handles.push(thread::spawn(move || {
                match SpawnLock::try_acquire(&path).expect("try") {
                    LockAcquire::Acquired(lock) => {
                        winners.fetch_add(1, Ordering::SeqCst);
                        thread::sleep(Duration::from_millis(50));
                        drop(lock);
                    }
                    LockAcquire::Busy => {}
                }
            }));
        }
        for h in handles {
            h.join().expect("join");
        }
        assert_eq!(winners.load(Ordering::SeqCst), 1);
    }

    /// Minimal Hello responder so `UnixSocketTransport::connect` succeeds.
    /// Uses std threads + std UnixListener so it can start from a sync spawn
    /// callback without blocking the tokio runtime.
    fn spawn_hello_stub(socket: PathBuf) -> (thread::JoinHandle<()>, std::sync::mpsc::Sender<()>) {
        let (stop_tx, stop_rx) = std::sync::mpsc::channel::<()>();
        let handle = thread::spawn(move || {
            let _ = std::fs::remove_file(&socket);
            let listener = match std::os::unix::net::UnixListener::bind(&socket) {
                Ok(l) => l,
                Err(err) => panic!("bind hello stub: {err}"),
            };
            listener
                .set_nonblocking(true)
                .expect("nonblocking listener");
            loop {
                if stop_rx.try_recv().is_ok() {
                    break;
                }
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        use std::io::{BufRead, BufReader as StdBufReader, Write as IoWrite};
                        let mut reader = StdBufReader::new(stream.try_clone().expect("clone"));
                        let mut line = String::new();
                        if reader.read_line(&mut line).is_ok() {
                            let response = serde_json::json!({
                                "result": "hello",
                                "data": {
                                    "version": 14,
                                    "capabilities": []
                                }
                            });
                            let mut payload = response.to_string();
                            payload.push('\n');
                            let _ = stream.write_all(payload.as_bytes());
                            let _ = stream.flush();
                        }
                    }
                    Err(err) if err.kind() == io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(_) => break,
                }
            }
        });
        (handle, stop_tx)
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn normal_startup_lock_spawn_readiness_release() {
        let dir = tempdir().expect("tempdir");
        let data_dir = dir.path().to_path_buf();
        let socket = data_dir.join("harness.sock");
        let socket_str = socket.to_string_lossy().to_string();
        let lock_path = data_dir.join("daemon.spawn.lock");
        let spawn_count = Arc::new(AtomicUsize::new(0));
        let spawn_count_cb = Arc::clone(&spawn_count);

        let result = ensure_daemon_running_with(&socket_str, {
            let socket = socket.clone();
            move |_sock, _data| {
                spawn_count_cb.fetch_add(1, Ordering::SeqCst);
                let (handle, stop) = spawn_hello_stub(socket);
                // Detach stub for test lifetime; keep stop alive via leak.
                std::mem::forget(handle);
                std::mem::forget(stop);
                Ok(None)
            }
        })
        .await;

        assert!(result.is_ok(), "ensure failed: {result:?}");
        assert_eq!(spawn_count.load(Ordering::SeqCst), 1);
        assert!(is_daemon_running(&socket_str).await);
        // Flock released: fresh acquire must succeed.
        match SpawnLock::try_acquire(&lock_path).expect("post") {
            LockAcquire::Acquired(_) => {}
            LockAcquire::Busy => panic!("lock must be released after successful startup"),
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn concurrent_startup_exactly_one_spawn() {
        let dir = tempdir().expect("tempdir");
        let data_dir = dir.path().to_path_buf();
        let socket = data_dir.join("harness.sock");
        let socket_str = socket.to_string_lossy().to_string();
        let spawn_count = Arc::new(AtomicUsize::new(0));
        let started = Arc::new(tokio::sync::Barrier::new(4));

        let mut tasks = Vec::new();
        for _ in 0..4 {
            let socket_str = socket_str.clone();
            let socket = socket.clone();
            let spawn_count = Arc::clone(&spawn_count);
            let started = Arc::clone(&started);
            tasks.push(tokio::spawn(async move {
                started.wait().await;
                ensure_daemon_running_with(&socket_str, {
                    let socket = socket.clone();
                    let spawn_count = Arc::clone(&spawn_count);
                    move |_sock, _data| {
                        if spawn_count.fetch_add(1, Ordering::SeqCst) == 0 {
                            let (handle, stop) = spawn_hello_stub(socket);
                            std::mem::forget(handle);
                            std::mem::forget(stop);
                        } else {
                            // Loser must not start a second authoritative listener.
                            // Brief delay so winner can bind first; readiness wait covers rest.
                            thread::sleep(Duration::from_millis(20));
                        }
                        Ok(None)
                    }
                })
                .await
            }));
        }

        let mut oks = 0;
        for t in tasks {
            if t.await.expect("join").is_ok() {
                oks += 1;
            }
        }
        assert_eq!(spawn_count.load(Ordering::SeqCst), 1, "exactly one spawn");
        assert_eq!(oks, 4, "all clients connect");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn stale_lock_file_is_reclaimed() {
        let dir = tempdir().expect("tempdir");
        let data_dir = dir.path().to_path_buf();
        let socket = data_dir.join("harness.sock");
        let socket_str = socket.to_string_lossy().to_string();
        let lock_path = data_dir.join("daemon.spawn.lock");
        // Crash residue: file present, no flock holder.
        std::fs::write(&lock_path, b"pid=999999\nacquired_unix_ms=1\n").expect("stale");

        let result = ensure_daemon_running_with(&socket_str, {
            let socket = socket.clone();
            move |_sock, _data| {
                let (handle, stop) = spawn_hello_stub(socket);
                std::mem::forget(handle);
                std::mem::forget(stop);
                Ok(None)
            }
        })
        .await;
        assert!(result.is_ok(), "stale lock must not block: {result:?}");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn active_lock_not_stolen_while_peer_spawns() {
        let dir = tempdir().expect("tempdir");
        let data_dir = dir.path().to_path_buf();
        let lock_path = data_dir.join("daemon.spawn.lock");
        let holder = match SpawnLock::try_acquire(&lock_path).expect("hold") {
            LockAcquire::Acquired(lock) => lock,
            LockAcquire::Busy => panic!("expected hold"),
        };

        let socket = data_dir.join("harness.sock");
        let socket_str = socket.to_string_lossy().to_string();
        let stolen = Arc::new(AtomicUsize::new(0));
        let stolen_cb = Arc::clone(&stolen);

        match SpawnLock::try_acquire(&lock_path).expect("probe") {
            LockAcquire::Busy => {}
            LockAcquire::Acquired(_) => panic!("must not steal active lock"),
        }

        let ensure = tokio::spawn({
            let socket_str = socket_str.clone();
            let socket = socket.clone();
            async move {
                ensure_daemon_running_with(&socket_str, move |_s, _d| {
                    stolen_cb.fetch_add(1, Ordering::SeqCst);
                    let (handle, stop) = spawn_hello_stub(socket);
                    std::mem::forget(handle);
                    std::mem::forget(stop);
                    Ok(None)
                })
                .await
            }
        });

        // Let ensure observe Busy a few times — must not spawn yet.
        sleep(Duration::from_millis(700)).await;
        assert_eq!(
            stolen.load(Ordering::SeqCst),
            0,
            "must not steal active lock"
        );
        drop(holder);

        let result = ensure.await.expect("join");
        assert!(
            result.is_ok(),
            "after holder drop ensure must succeed: {result:?}"
        );
        assert_eq!(stolen.load(Ordering::SeqCst), 1);
        assert!(is_daemon_running(&socket_str).await);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn busy_waiter_recovers_when_holder_releases() {
        // Same as active_lock scenario focused on Busy→acquire after Drop.
        let dir = tempdir().expect("tempdir");
        let data_dir = dir.path().to_path_buf();
        let lock_path = data_dir.join("daemon.spawn.lock");
        let socket = data_dir.join("harness.sock");
        let socket_str = socket.to_string_lossy().to_string();

        let holder = match SpawnLock::try_acquire(&lock_path).expect("hold") {
            LockAcquire::Acquired(lock) => lock,
            LockAcquire::Busy => panic!("expected hold"),
        };

        let waiter = tokio::spawn({
            let socket_str = socket_str.clone();
            let socket = socket.clone();
            async move {
                ensure_daemon_running_with(&socket_str, move |_s, _d| {
                    let (handle, stop) = spawn_hello_stub(socket);
                    std::mem::forget(handle);
                    std::mem::forget(stop);
                    Ok(None)
                })
                .await
            }
        });

        sleep(Duration::from_millis(400)).await;
        drop(holder);
        assert!(
            waiter.await.expect("join").is_ok(),
            "waiter must reclaim after holder death/release"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn spawn_failure_releases_lock() {
        let dir = tempdir().expect("tempdir");
        let data_dir = dir.path().to_path_buf();
        let socket = data_dir.join("harness.sock");
        let socket_str = socket.to_string_lossy().to_string();
        let lock_path = data_dir.join("daemon.spawn.lock");

        let err =
            ensure_daemon_running_with(&socket_str, |_s, _d| bail!("simulated spawn failure"))
                .await;
        assert!(err.is_err());

        match SpawnLock::try_acquire(&lock_path).expect("after fail") {
            LockAcquire::Acquired(_) => {}
            LockAcquire::Busy => panic!("lock must not stick after spawn failure"),
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn stale_socket_and_stale_lock_recover() {
        let dir = tempdir().expect("tempdir");
        let data_dir = dir.path().to_path_buf();
        let socket = data_dir.join("harness.sock");
        let socket_str = socket.to_string_lossy().to_string();
        let lock_path = data_dir.join("daemon.spawn.lock");

        std::fs::write(&socket, b"").expect("stale socket file");
        std::fs::write(&lock_path, b"pid=1\nacquired_unix_ms=0\n").expect("stale lock");

        let result = ensure_daemon_running_with(&socket_str, {
            let socket = socket.clone();
            move |_sock, _data| {
                let (handle, stop) = spawn_hello_stub(socket);
                std::mem::forget(handle);
                std::mem::forget(stop);
                Ok(None)
            }
        })
        .await;
        assert!(
            result.is_ok(),
            "CLI must recover from stale socket+lock: {result:?}"
        );
        assert!(is_daemon_running(&socket_str).await);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn readiness_timeout_releases_lock() {
        let dir = tempdir().expect("tempdir");
        let data_dir = dir.path().to_path_buf();
        let socket = data_dir.join("harness.sock");
        let socket_str = socket.to_string_lossy().to_string();
        let lock_path = data_dir.join("daemon.spawn.lock");

        // Spawn "succeeds" but never binds → wait timeout → lock released.
        let err = ensure_daemon_running_with(&socket_str, |_s, _d| Ok(None)).await;
        assert!(err.is_err());
        match SpawnLock::try_acquire(&lock_path).expect("after timeout") {
            LockAcquire::Acquired(_) => {}
            LockAcquire::Busy => panic!("lock must release after readiness timeout"),
        }
    }
}
