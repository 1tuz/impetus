use std::path::PathBuf;
use std::time::Duration;

/// Typed failures for local impetusd lifecycle (probe / flock / spawn / ready).
#[derive(Debug, thiserror::Error)]
pub enum DaemonError {
    #[error("impetusd binary missing at {}", path.display())]
    BinaryMissing { path: PathBuf },

    #[error("failed to spawn impetusd at {}", path.display())]
    SpawnFailed {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error(
        "impetusd did not become ready at {} within {:?}",
        socket.display(),
        budget
    )]
    StartupTimeout { socket: PathBuf, budget: Duration },

    #[error("impetusd exited before ready ({status})")]
    DaemonExited { status: String },

    #[error(
        "another impetus process holds daemon.spawn.lock but impetusd is not ready at {} within {:?}",
        socket.display(),
        budget
    )]
    LockBusyTimeout { socket: PathBuf, budget: Duration },

    #[error("daemon.spawn.lock I/O at {}", path.display())]
    LockIo {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("failed to remove stale socket {}", path.display())]
    StaleSocketRemove {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error(
        "impetusd is running at {} but IPC is incompatible ({detail}). \
         Upgrade/reinstall matching `impetus` and `impetusd` — refusing to respawn.",
        socket.display()
    )]
    Incompatible { socket: PathBuf, detail: String },

    #[error(
        "socket {} accepts connections but harness Hello failed: {detail}. \
         Inspect with `impetus doctor` or run `impetusd` in the foreground.",
        socket.display()
    )]
    HelloFailed { socket: PathBuf, detail: String },

    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error("{0}")]
    Other(String),
}
