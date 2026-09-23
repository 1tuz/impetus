//! Shared local `impetusd` lifecycle: probe, spawn lock, stale recovery, readiness.
//!
//! Canonical home for flock/RAII/kill-on-timeout/`Incompatible` safety used by
//! CLI, Desktop, and other hosts. No privilege escalation; userspace paths only.

mod ensure;
mod error;
mod lock;
mod paths;
mod probe;
mod spawn;

pub use ensure::{DaemonOptions, ensure_daemon_running, ensure_daemon_running_with};
pub use error::DaemonError;
pub use paths::{default_data_root, discover_socket_path};
pub use probe::{is_daemon_running, socket_listening};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lock::{LockAcquire, SpawnLock};
    use std::io;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::thread;
    use std::time::Duration;
    use tempfile::tempdir;
    use tokio::time::sleep;

    fn test_opts(data_dir: &std::path::Path) -> DaemonOptions {
        let mut opts = DaemonOptions::new(
            data_dir.join("harness.sock"),
            data_dir.to_path_buf(),
            PathBuf::from("/nonexistent/impetusd-for-tests"),
        );
        // Keep injectable tests snappy; production defaults remain 3s / 5s.
        opts.ready_timeout = Duration::from_millis(800);
        opts.lock_wait_timeout = Duration::from_secs(5);
        opts
    }

    #[test]
    fn ensure_daemon_spawn_has_no_privilege_escalation() {
        let ensure_src = include_str!("ensure.rs");
        let spawn_src = include_str!("spawn.rs");
        let combined = format!("{ensure_src}\n{spawn_src}");

        let start = ensure_src
            .find("pub async fn ensure_daemon_running")
            .expect("ensure_daemon_running present");
        let end = ensure_src[start..]
            .find("\nasync fn wait_until_ready_holding_child")
            .map(|i| start + i)
            .unwrap_or(ensure_src.len());
        let ensure_path = &ensure_src[start..end];

        for needle in [
            "sudo",
            "osascript",
            "withAdministratorPrivileges",
            "AuthorizationCreate",
            "SMJobBless",
            "PrivilegedHelperTools",
        ] {
            assert!(
                !combined.contains(needle),
                "daemon spawn must not escalate via `{needle}`"
            );
        }
        assert!(
            spawn_src.contains("Command::new(daemon_binary)")
                || ensure_path.contains("spawn_daemon_process"),
            "must spawn impetusd directly as the current user"
        );
        assert!(
            spawn_src.contains("IMPETUS_SOCKET"),
            "lazy-start must pass IMPETUS_SOCKET so client and daemon agree"
        );
        assert!(
            spawn_src.contains("IMPETUS_DATA_DIR"),
            "lazy-start must pass IMPETUS_DATA_DIR for durable state colocation"
        );
        assert!(
            ensure_path.contains("daemon.spawn.lock"),
            "lazy-start must serialize concurrent spawn via lock file"
        );
        assert!(
            include_str!("lock.rs").contains("flock")
                || include_str!("lock.rs").contains("LOCK_EX"),
            "lazy-start must use flock so crash cannot permanently block autostart"
        );
        assert!(
            ensure_path.contains("incompatible") || ensure_path.contains("Incompatible"),
            "lazy-start must refuse unlink/respawn on IPC Incompatible"
        );
    }

    #[test]
    fn discover_socket_defaults_under_user_data_dir() {
        let src = include_str!("paths.rs");
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
        let src = include_str!("paths.rs");
        let start = src
            .find("pub fn default_data_root")
            .expect("default_data_root present");
        let body = &src[start..];
        assert!(
            body.contains("Library/Application Support/Impetus") || body.contains("XDG_DATA_HOME")
        );
        assert!(!body.contains("/var/lib"));
        assert!(!body.contains("PrivilegedHelperTools"));
    }

    #[test]
    fn socket_listening_is_raw_connect_not_hello() {
        let src = include_str!("probe.rs");
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
        let opts = test_opts(dir.path());
        let lock_path = opts.data_dir.join("daemon.spawn.lock");
        let spawn_count = Arc::new(AtomicUsize::new(0));
        let spawn_count_cb = Arc::clone(&spawn_count);
        let socket = opts.socket_path.clone();

        let result = ensure_daemon_running_with(&opts, {
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
        assert!(is_daemon_running(&opts.socket_path).await);
        // Flock released: fresh acquire must succeed.
        match SpawnLock::try_acquire(&lock_path).expect("post") {
            LockAcquire::Acquired(_) => {}
            LockAcquire::Busy => panic!("lock must be released after successful startup"),
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn concurrent_startup_exactly_one_spawn() {
        let dir = tempdir().expect("tempdir");
        let opts = Arc::new(test_opts(dir.path()));
        let spawn_count = Arc::new(AtomicUsize::new(0));
        let started = Arc::new(tokio::sync::Barrier::new(4));

        let mut tasks = Vec::new();
        for _ in 0..4 {
            let opts = Arc::clone(&opts);
            let spawn_count = Arc::clone(&spawn_count);
            let started = Arc::clone(&started);
            tasks.push(tokio::spawn(async move {
                started.wait().await;
                let socket = opts.socket_path.clone();
                ensure_daemon_running_with(&opts, {
                    let spawn_count = Arc::clone(&spawn_count);
                    move |_sock, _data| {
                        if spawn_count.fetch_add(1, Ordering::SeqCst) == 0 {
                            let (handle, stop) = spawn_hello_stub(socket);
                            std::mem::forget(handle);
                            std::mem::forget(stop);
                        } else {
                            // Loser must not start a second authoritative listener.
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

    /// Two+ independent consumers (CLI-like + Desktop-like) share options →
    /// exactly one spawn, all succeed.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cross_consumer_exactly_one_daemon() {
        let dir = tempdir().expect("tempdir");
        let base = test_opts(dir.path());
        let spawn_count = Arc::new(AtomicUsize::new(0));
        let started = Arc::new(tokio::sync::Barrier::new(3));

        let mut tasks = Vec::new();
        for _ in 0..3 {
            let opts = base.clone();
            let spawn_count = Arc::clone(&spawn_count);
            let started = Arc::clone(&started);
            tasks.push(tokio::spawn(async move {
                started.wait().await;
                let socket = opts.socket_path.clone();
                ensure_daemon_running_with(&opts, {
                    let spawn_count = Arc::clone(&spawn_count);
                    move |_sock, _data| {
                        if spawn_count.fetch_add(1, Ordering::SeqCst) == 0 {
                            let (handle, stop) = spawn_hello_stub(socket);
                            std::mem::forget(handle);
                            std::mem::forget(stop);
                        } else {
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
        assert_eq!(oks, 3, "all independent consumers Ok");
        assert!(is_daemon_running(&base.socket_path).await);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn stale_lock_file_is_reclaimed() {
        let dir = tempdir().expect("tempdir");
        let opts = test_opts(dir.path());
        let lock_path = opts.data_dir.join("daemon.spawn.lock");
        // Crash residue: file present, no flock holder.
        std::fs::write(&lock_path, b"pid=999999\nacquired_unix_ms=1\n").expect("stale");
        let socket = opts.socket_path.clone();

        let result = ensure_daemon_running_with(&opts, {
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
        let opts = test_opts(dir.path());
        let lock_path = opts.data_dir.join("daemon.spawn.lock");
        let holder = match SpawnLock::try_acquire(&lock_path).expect("hold") {
            LockAcquire::Acquired(lock) => lock,
            LockAcquire::Busy => panic!("expected hold"),
        };

        let stolen = Arc::new(AtomicUsize::new(0));
        let stolen_cb = Arc::clone(&stolen);
        let socket = opts.socket_path.clone();
        let opts_for_task = opts.clone();

        match SpawnLock::try_acquire(&lock_path).expect("probe") {
            LockAcquire::Busy => {}
            LockAcquire::Acquired(_) => panic!("must not steal active lock"),
        }

        let ensure = tokio::spawn(async move {
            ensure_daemon_running_with(&opts_for_task, move |_s, _d| {
                stolen_cb.fetch_add(1, Ordering::SeqCst);
                let (handle, stop) = spawn_hello_stub(socket);
                std::mem::forget(handle);
                std::mem::forget(stop);
                Ok(None)
            })
            .await
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
        assert!(is_daemon_running(&opts.socket_path).await);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn busy_waiter_recovers_when_holder_releases() {
        let dir = tempdir().expect("tempdir");
        let opts = test_opts(dir.path());
        let lock_path = opts.data_dir.join("daemon.spawn.lock");

        let holder = match SpawnLock::try_acquire(&lock_path).expect("hold") {
            LockAcquire::Acquired(lock) => lock,
            LockAcquire::Busy => panic!("expected hold"),
        };

        let socket = opts.socket_path.clone();
        let opts_for_task = opts.clone();
        let waiter = tokio::spawn(async move {
            ensure_daemon_running_with(&opts_for_task, move |_s, _d| {
                let (handle, stop) = spawn_hello_stub(socket);
                std::mem::forget(handle);
                std::mem::forget(stop);
                Ok(None)
            })
            .await
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
        let opts = test_opts(dir.path());
        let lock_path = opts.data_dir.join("daemon.spawn.lock");

        let err = ensure_daemon_running_with(&opts, |_s, _d| {
            Err(DaemonError::Other("simulated spawn failure".into()))
        })
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
        let opts = test_opts(dir.path());
        let lock_path = opts.data_dir.join("daemon.spawn.lock");

        std::fs::write(&opts.socket_path, b"").expect("stale socket file");
        std::fs::write(&lock_path, b"pid=1\nacquired_unix_ms=0\n").expect("stale lock");
        let socket = opts.socket_path.clone();

        let result = ensure_daemon_running_with(&opts, {
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
            "must recover from stale socket+lock: {result:?}"
        );
        assert!(is_daemon_running(&opts.socket_path).await);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn readiness_timeout_releases_lock() {
        let dir = tempdir().expect("tempdir");
        let mut opts = test_opts(dir.path());
        opts.ready_timeout = Duration::from_millis(400);
        let lock_path = opts.data_dir.join("daemon.spawn.lock");

        // Spawn "succeeds" but never binds → wait timeout → lock released.
        let err = ensure_daemon_running_with(&opts, |_s, _d| Ok(None)).await;
        assert!(err.is_err());
        match SpawnLock::try_acquire(&lock_path).expect("after timeout") {
            LockAcquire::Acquired(_) => {}
            LockAcquire::Busy => panic!("lock must release after readiness timeout"),
        }
    }
}
