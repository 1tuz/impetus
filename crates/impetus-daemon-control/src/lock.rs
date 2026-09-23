use crate::error::DaemonError;
use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::io::AsRawFd;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

/// Outcome of a non-blocking exclusive flock attempt.
pub(crate) enum LockAcquire {
    Acquired(SpawnLock),
    Busy,
}

/// RAII exclusive spawn lock. Kernel drops flock when the holding process
/// exits (including crash), so a leftover lock *file* cannot permanently
/// block autostart.
pub(crate) struct SpawnLock {
    file: File,
}

impl SpawnLock {
    /// Open/create `path` and take exclusive non-blocking flock.
    ///
    /// Writes owner metadata (pid + timestamp) for diagnostics. Liveness of
    /// the lock is the flock itself — not the pid field (avoids PID-reuse
    /// false ownership).
    pub(crate) fn try_acquire(path: &Path) -> Result<LockAcquire, DaemonError> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)
            .map_err(|source| DaemonError::LockIo {
                path: path.to_path_buf(),
                source,
            })?;

        match flock_exclusive_nb(&file) {
            Ok(()) => {
                let mut lock = SpawnLock { file };
                lock.write_owner_metadata()
                    .map_err(|source| DaemonError::LockIo {
                        path: path.to_path_buf(),
                        source,
                    })?;
                Ok(LockAcquire::Acquired(lock))
            }
            Err(err) if err.kind() == io::ErrorKind::WouldBlock => Ok(LockAcquire::Busy),
            Err(source) => Err(DaemonError::LockIo {
                path: path.to_path_buf(),
                source,
            }),
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
