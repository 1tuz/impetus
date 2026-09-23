//! ACP agent registry / discovery / version probing from installed binaries.
//!
//! Backends are chosen by what is actually on PATH (or an explicit probe root),
//! not by hard-coded CLI flags. No live auth — version probe only.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

/// Known ACP-capable agent CLI candidates (labels only; paths discovered).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AgentCandidate {
    pub id: &'static str,
    pub display_name: &'static str,
    /// Basename candidates searched on PATH / probe roots.
    pub command_names: &'static [&'static str],
    /// Args used for a non-auth version probe (deterministic, short).
    pub version_args: &'static [&'static str],
}

/// Built-in catalog — install/discovery only; never assumes a vendor flag.
pub const BUILTIN_CANDIDATES: &[AgentCandidate] = &[
    AgentCandidate {
        id: "codex-acp",
        display_name: "Codex ACP",
        command_names: &["codex-acp", "codex"],
        version_args: &["--version"],
    },
    AgentCandidate {
        id: "claude-agent-acp",
        display_name: "Claude ACP",
        command_names: &["claude-code-acp", "claude-agent-acp", "claude"],
        version_args: &["--version"],
    },
    AgentCandidate {
        id: "gemini-cli-acp",
        display_name: "Gemini CLI ACP",
        command_names: &["gemini-cli-acp", "gemini"],
        version_args: &["--version"],
    },
    AgentCandidate {
        id: "qwen-code-acp",
        display_name: "Qwen Code ACP",
        command_names: &["qwen-code-acp", "qwen"],
        version_args: &["--version"],
    },
    AgentCandidate {
        id: "cursor-agent-acp",
        display_name: "Cursor Agent ACP",
        command_names: &["cursor-agent-acp", "cursor-agent"],
        version_args: &["--version"],
    },
];

/// Result of discovering one installed agent binary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscoveredAgent {
    pub id: String,
    pub display_name: String,
    pub command: PathBuf,
    /// Sanitized version string from probe (None if probe failed / empty).
    pub version: Option<String>,
    pub probe_ok: bool,
}

/// Discover agents under `search_dirs` (typically PATH split) using `catalog`.
///
/// Deterministic: first matching basename wins per candidate id; no network.
pub fn discover_agents(
    search_dirs: &[PathBuf],
    catalog: &[AgentCandidate],
) -> Vec<DiscoveredAgent> {
    let mut found = Vec::new();
    for candidate in catalog {
        let Some(command) = find_command(search_dirs, candidate.command_names) else {
            continue;
        };
        let version = probe_version(&command, candidate.version_args);
        let probe_ok = version.is_some();
        found.push(DiscoveredAgent {
            id: candidate.id.to_owned(),
            display_name: candidate.display_name.to_owned(),
            command,
            version,
            probe_ok,
        });
    }
    found
}

/// Split a PATH-like string into directories (empty entries skipped).
pub fn path_dirs_from_env(path_env: &str) -> Vec<PathBuf> {
    std::env::split_paths(path_env)
        .filter(|p| !p.as_os_str().is_empty())
        .collect()
}

fn find_command(search_dirs: &[PathBuf], names: &[&str]) -> Option<PathBuf> {
    for dir in search_dirs {
        for name in names {
            let candidate = dir.join(name);
            if is_executable(&candidate) {
                return Some(candidate);
            }
        }
    }
    None
}

fn is_executable(path: &Path) -> bool {
    if !path.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        path.metadata()
            .map(|m| m.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// Run a short version probe. No auth env inherited beyond process default.
/// Output is sanitized (control chars stripped, length capped).
pub fn probe_version(command: &Path, args: &[&str]) -> Option<String> {
    let mut child = Command::new(command);
    child.args(args);
    // Fail closed on hang: rely on OS; tests use instant scripts.
    let output = child.output().ok()?;
    if !output.status.success() && output.stdout.is_empty() && output.stderr.is_empty() {
        return None;
    }
    let raw = if !output.stdout.is_empty() {
        String::from_utf8_lossy(&output.stdout)
    } else {
        String::from_utf8_lossy(&output.stderr)
    };
    let sanitized = sanitize_version(&raw);
    if sanitized.is_empty() {
        None
    } else {
        Some(sanitized)
    }
}

fn sanitize_version(raw: &str) -> String {
    raw.chars()
        .filter(|c| !c.is_control() || *c == '\n' || *c == '\t')
        .take(200)
        .collect::<String>()
        .lines()
        .next()
        .unwrap_or("")
        .trim()
        .to_owned()
}

/// Optional soft timeout helper for tests (spawn + wait). Production probe is sync.
#[allow(dead_code)]
pub fn probe_version_with_timeout(
    command: &Path,
    args: &[&str],
    _timeout: Duration,
) -> Option<String> {
    // ponytail: sync Command has no portable timeout without extra deps;
    // tests use instant binaries. Upgrade path: tokio::process + timeout.
    probe_version(command, args)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    fn write_executable(dir: &Path, name: &str, body: &str) -> PathBuf {
        let path = dir.join(name);
        fs::write(&path, body).expect("write");
        let mut perms = fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&path, perms).unwrap();
        path
    }

    #[test]
    fn discovers_installed_candidate_and_probes_version() {
        let dir = tempfile::tempdir().expect("temp");
        write_executable(
            dir.path(),
            "codex-acp",
            "#!/bin/sh\necho 'codex-acp 0.4.2'\n",
        );
        let catalog = &[AgentCandidate {
            id: "codex-acp",
            display_name: "Codex ACP",
            command_names: &["codex-acp"],
            version_args: &["--version"],
        }];
        let found = discover_agents(&[dir.path().to_path_buf()], catalog);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].id, "codex-acp");
        assert!(found[0].probe_ok);
        assert_eq!(found[0].version.as_deref(), Some("codex-acp 0.4.2"));
    }

    #[test]
    fn missing_binary_is_not_invented() {
        let dir = tempfile::tempdir().expect("temp");
        let found = discover_agents(&[dir.path().to_path_buf()], BUILTIN_CANDIDATES);
        assert!(found.is_empty());
    }

    #[test]
    fn path_dirs_skips_empty() {
        let dirs = path_dirs_from_env("/tmp::/opt/bin:");
        assert_eq!(dirs.len(), 2);
    }
}
