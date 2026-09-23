use crate::error::DaemonError;
use crate::lock::{LockAcquire, SpawnLock};
use crate::probe::{is_daemon_running, socket_listening};
use crate::spawn::spawn_daemon_process;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tokio::time::sleep;

/// Options for lazy-start / ensure of a local `impetusd`.
#[derive(Debug, Clone)]
pub struct DaemonOptions {
    pub socket_path: PathBuf,
    pub data_dir: PathBuf,
    /// Required for real spawn; Desktop passes bundled path.
    pub daemon_binary: PathBuf,
    /// Default 3s — wait for Hello after spawn while holding flock.
    pub ready_timeout: Duration,
    /// Default 5s — wait while a peer holds `daemon.spawn.lock`.
    pub lock_wait_timeout: Duration,
}

impl DaemonOptions {
    pub fn new(socket_path: PathBuf, data_dir: PathBuf, daemon_binary: PathBuf) -> Self {
        Self {
            socket_path,
            data_dir,
            daemon_binary,
            ready_timeout: Duration::from_secs(3),
            lock_wait_timeout: Duration::from_secs(5),
        }
    }
}

/// Attempt to spawn `impetusd` if not already accepting on `opts.socket_path`.
///
/// Product UX: ordinary clients lazy-start the daemon. Manual `impetusd` is for
/// development / debugging / advanced administration.
///
/// Safety:
/// - live socket + protocol `Incompatible` → error (never unlink / respawn)
/// - spawn serialized via exclusive `flock` on `daemon.spawn.lock`
///   (kernel releases flock on holder crash — stale file cannot permanently
///   block autostart; PID metadata is diagnostic only)
pub async fn ensure_daemon_running(opts: &DaemonOptions) -> Result<(), DaemonError> {
    let binary = opts.daemon_binary.clone();
    ensure_daemon_running_with(opts, move |socket, data_dir| {
        spawn_daemon_process(&binary, socket, data_dir)
    })
    .await
}

/// Injectable spawn path for tests / advanced hosts.
///
/// Returns a live [`std::process::Child`] when the caller must keep the spawn
/// lock until that child exits or becomes ready. Tests that start an in-process
/// Hello stub return `None`.
pub async fn ensure_daemon_running_with<F>(
    opts: &DaemonOptions,
    spawn_daemon: F,
) -> Result<(), DaemonError>
where
    F: FnOnce(&Path, &Path) -> Result<Option<std::process::Child>, DaemonError>,
{
    let socket_path = opts.socket_path.as_path();

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
                    return Err(DaemonError::Incompatible {
                        socket: opts.socket_path.clone(),
                        detail: msg,
                    });
                }
                return Err(DaemonError::HelloFailed {
                    socket: opts.socket_path.clone(),
                    detail: msg,
                });
            }
        }
    }

    let data_dir = &opts.data_dir;

    if let Some(parent) = data_dir.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::create_dir_all(data_dir);

    let lock_path = data_dir.join("daemon.spawn.lock");
    let readiness_budget = opts.lock_wait_timeout;
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
                if opts.socket_path.exists() && !socket_listening(socket_path) {
                    std::fs::remove_file(&opts.socket_path).map_err(|source| {
                        DaemonError::StaleSocketRemove {
                            path: opts.socket_path.clone(),
                            source,
                        }
                    })?;
                }

                // Recheck after lock — peer may have finished.
                if is_daemon_running(socket_path).await {
                    drop(lock);
                    return Ok(());
                }

                let mut child = match spawn_daemon(socket_path, data_dir) {
                    Ok(child) => child,
                    Err(err) => {
                        drop(lock);
                        return Err(err);
                    }
                };

                let wait_budget = opts.ready_timeout;
                let result =
                    wait_until_ready_holding_child(socket_path, wait_budget, &mut child).await;
                // Hold flock until ready or child is gone so a peer cannot start
                // a second authoritative daemon while ours is still binding.
                drop(lock);
                return result;
            }
            LockAcquire::Busy => {
                if started.elapsed() >= readiness_budget {
                    return Err(DaemonError::LockBusyTimeout {
                        socket: opts.socket_path.clone(),
                        budget: readiness_budget,
                    });
                }
                sleep(Duration::from_millis(300)).await;
            }
        }
    }
}

async fn wait_until_ready_holding_child(
    socket_path: &Path,
    budget: Duration,
    child: &mut Option<std::process::Child>,
) -> Result<(), DaemonError> {
    let deadline = Instant::now() + budget;
    loop {
        if is_daemon_running(socket_path).await {
            return Ok(());
        }
        if let Some(proc) = child.as_mut() {
            match proc.try_wait() {
                Ok(Some(status)) => {
                    return Err(DaemonError::DaemonExited {
                        status: status.to_string(),
                    });
                }
                Ok(None) => {}
                Err(err) => {
                    return Err(DaemonError::Other(format!(
                        "failed to poll impetusd child: {err}"
                    )));
                }
            }
        }
        if Instant::now() >= deadline {
            if let Some(proc) = child.as_mut() {
                // Still binding past budget — kill so a peer retry cannot race
                // a second authoritative process against a half-started daemon.
                let _ = proc.kill();
                let _ = proc.wait();
            }
            return Err(DaemonError::StartupTimeout {
                socket: socket_path.to_path_buf(),
                budget,
            });
        }
        sleep(Duration::from_millis(300)).await;
    }
}
