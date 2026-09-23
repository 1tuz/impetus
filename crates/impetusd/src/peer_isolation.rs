//! Same-uid Unix peer isolation for the harness control-plane socket.
//!
//! - Restrictive umask around bind (socket created 0600 even before chmod).
//! - Peer credentials: reject foreign uid (fail closed if credentials unavailable).
//! - `IMPETUS_ACP_CHILD=1` peers are denied unless they also carry
//!   [`ACP_CHILD_CONTROL_OK_ENV`]=`1` (operator escape hatch; ACP profile blanks it).

use std::io;
use std::os::fd::AsRawFd;

use tokio::net::UnixStream;

/// Marker forced onto ACP agent children ([`impetus_acp_gateway::ACP_CHILD_ENV`]).
pub const ACP_CHILD_ENV: &str = "IMPETUS_ACP_CHILD";

/// Explicit operator authorization to use the control socket while marked as an
/// ACP child. Not grantable via ACP profile (control-plane name); SDK overlay
/// blanks inherited `IMPETUS_*` values.
pub const ACP_CHILD_CONTROL_OK_ENV: &str = "IMPETUS_ACP_CHILD_CONTROL_OK";

/// umask that yields mode 0600 for sockets created as 0666 (`0666 & !0177`).
const SOCKET_UMASK: libc::mode_t = 0o177;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PeerCredentials {
    pub uid: u32,
    pub gid: u32,
    pub pid: Option<u32>,
}

#[derive(Debug, thiserror::Error)]
pub enum AdmitError {
    #[error("peer credentials unavailable: {0}")]
    Credentials(io::Error),
    #[error("peer uid {peer} does not match daemon euid {daemon}")]
    UidMismatch { peer: u32, daemon: u32 },
    #[error(
        "ACP child process blocked from control-plane socket (set {ACP_CHILD_CONTROL_OK_ENV}=1 to authorize)"
    )]
    AcpChildBlocked,
    #[error("peer environment inspection failed: {0}")]
    Environ(io::Error),
}

/// Set restrictive umask for socket bind; returns previous umask to restore.
pub fn push_socket_umask() -> libc::mode_t {
    // SAFETY: umask is process-global and returns the previous mask.
    unsafe { libc::umask(SOCKET_UMASK) }
}

/// Restore umask after [`push_socket_umask`].
pub fn pop_socket_umask(previous: libc::mode_t) {
    unsafe {
        libc::umask(previous);
    }
}

/// Admit a newly accepted control-plane peer, or fail closed.
pub fn admit_control_plane_peer(stream: &UnixStream) -> Result<(), AdmitError> {
    let peer = peer_credentials(stream).map_err(AdmitError::Credentials)?;
    admit_peer_credentials(peer, read_peer_environ)
}

/// Pure admission decision (testable without a live socket).
pub fn admit_peer_credentials(
    peer: PeerCredentials,
    environ: impl FnOnce(u32) -> io::Result<Vec<u8>>,
) -> Result<(), AdmitError> {
    let daemon_uid = current_euid();
    if peer.uid != daemon_uid {
        return Err(AdmitError::UidMismatch {
            peer: peer.uid,
            daemon: daemon_uid,
        });
    }

    let Some(pid) = peer.pid else {
        // Platform lacks peer pid — uid gate still applied; ACP marker check
        // skipped where APIs do not permit (see issue acceptance).
        return Ok(());
    };

    let env_bytes = environ(pid).map_err(AdmitError::Environ)?;
    let vars = parse_environ_bytes(&env_bytes);
    if env_flag_is_set(&vars, ACP_CHILD_ENV) && !env_flag_is_set(&vars, ACP_CHILD_CONTROL_OK_ENV) {
        return Err(AdmitError::AcpChildBlocked);
    }
    Ok(())
}

pub fn current_euid() -> u32 {
    unsafe { libc::geteuid() }
}

pub fn peer_credentials(stream: &UnixStream) -> io::Result<PeerCredentials> {
    peer_credentials_fd(stream.as_raw_fd())
}

fn peer_credentials_fd(fd: libc::c_int) -> io::Result<PeerCredentials> {
    #[cfg(any(
        target_os = "macos",
        target_os = "ios",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "netbsd",
        target_os = "dragonfly"
    ))]
    {
        let mut uid: libc::uid_t = 0;
        let mut gid: libc::gid_t = 0;
        let rc = unsafe { libc::getpeereid(fd, &mut uid, &mut gid) };
        if rc != 0 {
            return Err(io::Error::last_os_error());
        }
        let pid = peer_pid_darwin(fd).ok();
        Ok(PeerCredentials {
            uid: uid as u32,
            gid: gid as u32,
            pid,
        })
    }

    #[cfg(target_os = "linux")]
    {
        let mut cred = libc::ucred {
            pid: 0,
            uid: 0,
            gid: 0,
        };
        let mut len = std::mem::size_of_val(&cred) as libc::socklen_t;
        let rc = unsafe {
            libc::getsockopt(
                fd,
                libc::SOL_SOCKET,
                libc::SO_PEERCRED,
                (&raw mut cred).cast(),
                &mut len,
            )
        };
        if rc != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(PeerCredentials {
            uid: cred.uid,
            gid: cred.gid,
            pid: Some(cred.pid as u32),
        })
    }

    #[cfg(not(any(
        target_os = "linux",
        target_os = "macos",
        target_os = "ios",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "netbsd",
        target_os = "dragonfly"
    )))]
    {
        let _ = fd;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "peer credentials unsupported on this platform",
        ))
    }
}

#[cfg(any(target_os = "macos", target_os = "ios"))]
fn peer_pid_darwin(fd: libc::c_int) -> io::Result<u32> {
    let mut pid: libc::pid_t = 0;
    let mut len = std::mem::size_of_val(&pid) as libc::socklen_t;
    let rc = unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_LOCAL,
            libc::LOCAL_PEERPID,
            (&raw mut pid).cast(),
            &mut len,
        )
    };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    if pid <= 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "peer pid unavailable",
        ));
    }
    Ok(pid as u32)
}

#[cfg(any(
    target_os = "freebsd",
    target_os = "openbsd",
    target_os = "netbsd",
    target_os = "dragonfly"
))]
fn peer_pid_darwin(_fd: libc::c_int) -> io::Result<u32> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "peer pid not wired on this BSD",
    ))
}

/// Read NUL-separated `KEY=VALUE` environ for `pid` (exec-time image).
pub fn read_peer_environ(pid: u32) -> io::Result<Vec<u8>> {
    #[cfg(target_os = "linux")]
    {
        std::fs::read(format!("/proc/{pid}/environ"))
    }

    #[cfg(any(target_os = "macos", target_os = "ios"))]
    {
        read_peer_environ_darwin(pid)
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "ios")))]
    {
        let _ = pid;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "peer environ inspection unsupported on this platform",
        ))
    }
}

#[cfg(any(target_os = "macos", target_os = "ios"))]
fn read_peer_environ_darwin(pid: u32) -> io::Result<Vec<u8>> {
    let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid as libc::c_int];
    let mut size: libc::size_t = 0;
    let rc = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            mib.len() as libc::c_uint,
            std::ptr::null_mut(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    let mut buf = vec![0u8; size];
    let rc = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            mib.len() as libc::c_uint,
            buf.as_mut_ptr().cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    buf.truncate(size);
    extract_environ_from_procargs2(&buf)
}

/// Parse Darwin `KERN_PROCARGS2` buffer into a Linux-style environ blob.
#[cfg(any(target_os = "macos", target_os = "ios"))]
fn extract_environ_from_procargs2(buf: &[u8]) -> io::Result<Vec<u8>> {
    if buf.len() < 4 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "KERN_PROCARGS2 buffer too short",
        ));
    }
    let argc = i32::from_ne_bytes([buf[0], buf[1], buf[2], buf[3]]);
    if argc < 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "KERN_PROCARGS2 argc negative",
        ));
    }
    let mut i = 4usize;
    // Skip optional padding NULs before executable path.
    while i < buf.len() && buf[i] == 0 {
        i += 1;
    }
    // Executable path.
    while i < buf.len() && buf[i] != 0 {
        i += 1;
    }
    i += 1;
    while i < buf.len() && buf[i] == 0 {
        i += 1;
    }
    let mut strings: Vec<&[u8]> = Vec::new();
    while i < buf.len() {
        if buf[i] == 0 {
            i += 1;
            continue;
        }
        let start = i;
        while i < buf.len() && buf[i] != 0 {
            i += 1;
        }
        strings.push(&buf[start..i]);
        i += 1;
    }
    let argc = argc as usize;
    if strings.len() < argc {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "KERN_PROCARGS2 fewer strings than argc",
        ));
    }
    let mut out = Vec::new();
    for entry in &strings[argc..] {
        out.extend_from_slice(entry);
        out.push(0);
    }
    Ok(out)
}

fn parse_environ_bytes(bytes: &[u8]) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for entry in bytes.split(|b| *b == 0) {
        if entry.is_empty() {
            continue;
        }
        let Ok(text) = std::str::from_utf8(entry) else {
            continue;
        };
        let Some((key, value)) = text.split_once('=') else {
            continue;
        };
        out.push((key.to_string(), value.to_string()));
    }
    out
}

fn env_flag_is_set(vars: &[(String, String)], name: &str) -> bool {
    vars.iter()
        .find(|(k, _)| k == name)
        .is_some_and(|(_, v)| v == "1")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn admit_same_uid_without_acp_marker() {
        let peer = PeerCredentials {
            uid: current_euid(),
            gid: 0,
            pid: Some(1),
        };
        admit_peer_credentials(peer, |_| Ok(b"PATH=/bin\0HOME=/tmp\0".to_vec())).unwrap();
    }

    #[test]
    fn admit_rejects_foreign_uid() {
        let peer = PeerCredentials {
            uid: current_euid().wrapping_add(1),
            gid: 0,
            pid: Some(1),
        };
        let err = admit_peer_credentials(peer, |_| Ok(Vec::new())).unwrap_err();
        assert!(matches!(err, AdmitError::UidMismatch { .. }));
    }

    #[test]
    fn admit_blocks_acp_child_without_control_ok() {
        let peer = PeerCredentials {
            uid: current_euid(),
            gid: 0,
            pid: Some(42),
        };
        let err =
            admit_peer_credentials(peer, |_| Ok(b"IMPETUS_ACP_CHILD=1\0PATH=/bin\0".to_vec()))
                .unwrap_err();
        assert!(matches!(err, AdmitError::AcpChildBlocked));
    }

    #[test]
    fn admit_allows_acp_child_with_control_ok() {
        let peer = PeerCredentials {
            uid: current_euid(),
            gid: 0,
            pid: Some(42),
        };
        admit_peer_credentials(peer, |_| {
            Ok(b"IMPETUS_ACP_CHILD=1\0IMPETUS_ACP_CHILD_CONTROL_OK=1\0".to_vec())
        })
        .unwrap();
    }

    #[test]
    fn admit_fails_closed_when_environ_unreadable() {
        let peer = PeerCredentials {
            uid: current_euid(),
            gid: 0,
            pid: Some(42),
        };
        let err = admit_peer_credentials(peer, |_| {
            Err(io::Error::new(io::ErrorKind::PermissionDenied, "nope"))
        })
        .unwrap_err();
        assert!(matches!(err, AdmitError::Environ(_)));
    }

    #[test]
    fn admit_skips_acp_check_without_pid() {
        let peer = PeerCredentials {
            uid: current_euid(),
            gid: 0,
            pid: None,
        };
        admit_peer_credentials(peer, |_| panic!("environ must not be read")).unwrap();
    }

    #[test]
    fn umask_push_pop_restores() {
        let before = unsafe { libc::umask(0o022) };
        let pushed = push_socket_umask();
        assert_eq!(pushed, 0o022);
        let during = unsafe { libc::umask(0o022) };
        assert_eq!(during, SOCKET_UMASK);
        pop_socket_umask(before);
        let after = unsafe { libc::umask(before) };
        assert_eq!(after, before);
        unsafe {
            libc::umask(before);
        }
    }

    #[test]
    fn parse_environ_bytes_splits_nul_entries() {
        let vars = parse_environ_bytes(b"A=1\0IMPETUS_ACP_CHILD=1\0B=\0");
        assert!(env_flag_is_set(&vars, ACP_CHILD_ENV));
        assert!(!env_flag_is_set(&vars, ACP_CHILD_CONTROL_OK_ENV));
    }

    #[cfg(any(target_os = "macos", target_os = "ios"))]
    #[test]
    fn read_peer_environ_sees_exec_time_marker() {
        use std::io::{BufRead, BufReader};
        use std::process::{Command, Stdio};

        let mut child = Command::new("python3")
            .args(["-c", "import os,sys,time; sys.stdout.write(str(os.getpid())+'\\n'); sys.stdout.flush(); time.sleep(30)"])
            .env(ACP_CHILD_ENV, "1")
            .stdout(Stdio::piped())
            .spawn()
            .expect("spawn marker child");
        let mut line = String::new();
        BufReader::new(child.stdout.take().expect("stdout"))
            .read_line(&mut line)
            .expect("pid line");
        let pid: u32 = line.trim().parse().expect("pid");
        let bytes = read_peer_environ(pid).expect("environ");
        let vars = parse_environ_bytes(&bytes);
        assert!(
            env_flag_is_set(&vars, ACP_CHILD_ENV),
            "expected {ACP_CHILD_ENV}=1 in peer environ"
        );
        let _ = child.kill();
        let _ = child.wait();
    }
}
