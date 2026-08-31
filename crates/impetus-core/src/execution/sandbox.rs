//! Replaceable OS sandbox backend for agent-controlled child processes.

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use thiserror::Error;
use tokio::process::Command;
use uuid::Uuid;

const SANDBOX_EXEC: &str = "/usr/bin/sandbox-exec";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SandboxDecisionState {
    Prepared,
    Denied,
}

/// Secret-free execution evidence suitable for the durable event log.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SandboxDecision {
    pub backend: String,
    pub state: SandboxDecisionState,
    pub network_allowed: bool,
    pub writable_root_count: u32,
    pub reason_code: Option<String>,
}

impl SandboxDecision {
    fn prepared(backend: &str, request: &SandboxCommandRequest<'_>) -> Self {
        Self {
            backend: backend.into(),
            state: SandboxDecisionState::Prepared,
            network_allowed: request.allow_network,
            writable_root_count: 2,
            reason_code: None,
        }
    }

    pub(crate) fn denied(
        backend: &str,
        request: &SandboxCommandRequest<'_>,
        error: &SandboxError,
    ) -> Self {
        Self {
            backend: backend.into(),
            state: SandboxDecisionState::Denied,
            network_allowed: request.allow_network,
            writable_root_count: 0,
            reason_code: Some(error.reason_code().into()),
        }
    }
}

#[derive(Debug, Error)]
pub enum SandboxError {
    #[error("sandbox backend is unavailable")]
    Unavailable,
    #[error("sandbox configuration is invalid")]
    InvalidConfiguration,
    #[error("sandbox profile could not be constructed")]
    ProfileConstruction,
    #[error("sandbox session directory could not be created")]
    SessionDirectory,
}

impl SandboxError {
    pub fn reason_code(&self) -> &'static str {
        match self {
            Self::Unavailable => "backend_unavailable",
            Self::InvalidConfiguration => "invalid_configuration",
            Self::ProfileConstruction => "profile_construction_failed",
            Self::SessionDirectory => "session_directory_failed",
        }
    }
}

pub struct SandboxCommandRequest<'a> {
    pub executable: &'a str,
    pub args: &'a [String],
    pub workspace_root: &'a Path,
    pub working_dir: &'a Path,
    pub explicit_env: &'a [(String, String)],
    pub allow_network: bool,
}

/// A provider owns the platform-specific confinement mechanism. Policy and the
/// tool orchestrator only provide a normalized execution request.
pub trait SandboxProvider: Send + Sync {
    fn backend_name(&self) -> &'static str;
    fn probe(&self) -> Result<(), SandboxError>;
    fn prepare(
        &self,
        request: &SandboxCommandRequest<'_>,
    ) -> Result<PreparedSandboxCommand, SandboxError>;
}

pub struct PreparedSandboxCommand {
    command: Command,
    decision: SandboxDecision,
    _session_temp: SessionTempDirectory,
}

impl PreparedSandboxCommand {
    pub fn command_mut(&mut self) -> &mut Command {
        &mut self.command
    }

    pub fn decision(&self) -> &SandboxDecision {
        &self.decision
    }
}

pub fn production_sandbox_provider() -> Arc<dyn SandboxProvider> {
    #[cfg(target_os = "macos")]
    {
        Arc::new(MacosSeatbeltSandbox)
    }
    #[cfg(not(target_os = "macos"))]
    {
        Arc::new(UnavailableSandboxProvider)
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct MacosSeatbeltSandbox;

impl SandboxProvider for MacosSeatbeltSandbox {
    fn backend_name(&self) -> &'static str {
        "macos_seatbelt"
    }

    fn probe(&self) -> Result<(), SandboxError> {
        #[cfg(target_os = "macos")]
        {
            use std::os::unix::fs::PermissionsExt;
            let metadata = fs::metadata(SANDBOX_EXEC).map_err(|_| SandboxError::Unavailable)?;
            if metadata.is_file() && metadata.permissions().mode() & 0o111 != 0 {
                Ok(())
            } else {
                Err(SandboxError::Unavailable)
            }
        }
        #[cfg(not(target_os = "macos"))]
        {
            Err(SandboxError::Unavailable)
        }
    }

    fn prepare(
        &self,
        request: &SandboxCommandRequest<'_>,
    ) -> Result<PreparedSandboxCommand, SandboxError> {
        self.probe()?;
        let paths = CanonicalSandboxPaths::new(request)?;
        let session_temp = SessionTempDirectory::create()?;
        let profile = seatbelt_profile(request, &paths, session_temp.path())?;

        let mut command = Command::new(SANDBOX_EXEC);
        command
            .arg("-p")
            .arg(profile)
            .arg(request.executable)
            .args(request.args)
            .current_dir(&paths.working_dir)
            .env_clear()
            .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
            .env("HOME", session_temp.home())
            .env("TMPDIR", session_temp.tmp())
            .env("LANG", "C")
            .env("LC_ALL", "C")
            .env("TERM", "dumb")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        for (key, value) in request.explicit_env {
            if !safe_environment_key(key) {
                return Err(SandboxError::InvalidConfiguration);
            }
            command.env(key, value);
        }

        #[cfg(unix)]
        command.process_group(0);

        Ok(PreparedSandboxCommand {
            command,
            decision: SandboxDecision::prepared(self.backend_name(), request),
            _session_temp: session_temp,
        })
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct UnavailableSandboxProvider;

impl UnavailableSandboxProvider {
    pub fn new(_reason: impl Into<String>) -> Self {
        Self
    }
}

impl SandboxProvider for UnavailableSandboxProvider {
    fn backend_name(&self) -> &'static str {
        "unavailable"
    }

    fn probe(&self) -> Result<(), SandboxError> {
        Err(SandboxError::Unavailable)
    }

    fn prepare(
        &self,
        _request: &SandboxCommandRequest<'_>,
    ) -> Result<PreparedSandboxCommand, SandboxError> {
        Err(SandboxError::Unavailable)
    }
}

struct CanonicalSandboxPaths {
    workspace_root: PathBuf,
    working_dir: PathBuf,
    sensitive_roots: Vec<PathBuf>,
}

impl CanonicalSandboxPaths {
    fn new(request: &SandboxCommandRequest<'_>) -> Result<Self, SandboxError> {
        let workspace_root = request
            .workspace_root
            .canonicalize()
            .map_err(|_| SandboxError::InvalidConfiguration)?;
        let working_dir = request
            .working_dir
            .canonicalize()
            .map_err(|_| SandboxError::InvalidConfiguration)?;
        if !workspace_root.is_dir() || !working_dir.starts_with(&workspace_root) {
            return Err(SandboxError::InvalidConfiguration);
        }

        let sensitive_roots = sensitive_home_roots();
        if sensitive_roots
            .iter()
            .any(|root| workspace_root.starts_with(root))
        {
            return Err(SandboxError::InvalidConfiguration);
        }

        Ok(Self {
            workspace_root,
            working_dir,
            sensitive_roots,
        })
    }
}

struct SessionTempDirectory {
    root: PathBuf,
    home: PathBuf,
    tmp: PathBuf,
}

impl SessionTempDirectory {
    fn create() -> Result<Self, SandboxError> {
        use std::os::unix::fs::PermissionsExt;

        let parent = std::env::temp_dir()
            .canonicalize()
            .map_err(|_| SandboxError::SessionDirectory)?;
        let root = parent.join(format!("impetus-sandbox-{}", Uuid::new_v4()));
        fs::create_dir(&root).map_err(|_| SandboxError::SessionDirectory)?;
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700))
            .map_err(|_| SandboxError::SessionDirectory)?;
        let home = root.join("home");
        let tmp = root.join("tmp");
        fs::create_dir(&home).map_err(|_| SandboxError::SessionDirectory)?;
        fs::create_dir(&tmp).map_err(|_| SandboxError::SessionDirectory)?;
        Ok(Self { root, home, tmp })
    }

    fn path(&self) -> &Path {
        &self.root
    }

    fn home(&self) -> &Path {
        &self.home
    }

    fn tmp(&self) -> &Path {
        &self.tmp
    }
}

impl Drop for SessionTempDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn safe_environment_key(key: &str) -> bool {
    matches!(
        key,
        "LANG" | "LC_ALL" | "TERM" | "NO_COLOR" | "RUST_BACKTRACE"
    )
}

fn sensitive_home_roots() -> Vec<PathBuf> {
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
        return Vec::new();
    };
    let home = home.canonicalize().unwrap_or(home);
    [
        ".ssh",
        ".aws",
        ".docker",
        ".gnupg",
        ".kube",
        ".netrc",
        ".git-credentials",
        ".npmrc",
        "Library",
    ]
    .into_iter()
    .map(|relative| {
        let path = home.join(relative);
        path.canonicalize().unwrap_or(path)
    })
    .collect()
}

fn seatbelt_profile(
    request: &SandboxCommandRequest<'_>,
    paths: &CanonicalSandboxPaths,
    session_temp: &Path,
) -> Result<String, SandboxError> {
    let workspace = sbpl_path(&paths.workspace_root)?;
    let session_temp = sbpl_path(session_temp)?;
    let mut profile = format!(
        "(version 1)\n\
         (deny default)\n\
         (allow process*)\n\
         (allow signal (target same-sandbox))\n\
         (allow sysctl-read)\n\
         (allow file-read-metadata)\n\
         (allow file-read*\n\
           (subpath \"/System\")\n\
           (subpath \"/usr/lib\")\n\
           (subpath \"/usr/bin\")\n\
           (subpath \"/bin\")\n\
           (subpath \"/usr/share\")\n\
           (subpath \"/private/etc\")\n\
           (literal \"/dev/null\")\n\
           (literal \"/dev/urandom\")\n\
           (subpath \"{workspace}\")\n\
           (subpath \"{session_temp}\"))\n\
         (allow file-write*\n\
           (literal \"/dev/null\")\n\
           (subpath \"{workspace}\")\n\
           (subpath \"{session_temp}\"))\n"
    );
    for sensitive in &paths.sensitive_roots {
        let sensitive = sbpl_path(sensitive)?;
        profile.push_str(&format!(
            "(deny file-read* file-write* (subpath \"{sensitive}\"))\n"
        ));
    }
    if request.allow_network {
        profile.push_str("(allow network*)\n");
    }
    Ok(profile)
}

fn sbpl_path(path: &Path) -> Result<String, SandboxError> {
    let path = path.to_str().ok_or(SandboxError::ProfileConstruction)?;
    if path.chars().any(char::is_control) {
        return Err(SandboxError::ProfileConstruction);
    }
    Ok(path.replace('\\', "\\\\").replace('"', "\\\""))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_environment_is_allowlisted() {
        assert!(safe_environment_key("TERM"));
        assert!(!safe_environment_key("AWS_SECRET_ACCESS_KEY"));
        assert!(!safe_environment_key("PATH"));
    }

    #[test]
    fn sbpl_paths_escape_quotes_and_backslashes() {
        assert_eq!(
            sbpl_path(Path::new("/tmp/a\\b\"c")).expect("escaped"),
            "/tmp/a\\\\b\\\"c"
        );
    }
}
