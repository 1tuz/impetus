//! Daemon-owned Git read/branch helpers via the system `git` CLI.
//!
//! Session cwd prefers an open [`WorktreeManager`] binding when present;
//! otherwise the session workspace root. Clients must not shell out to git.

use std::path::{Path, PathBuf};
use std::process::Command;

use thiserror::Error;
use uuid::Uuid;

use crate::diff_observation::from_unified;
use crate::worktree_manager::{
    WorktreeBinding, WorktreeDiffSummary, WorktreeLifecycleState, WorktreeManager,
};

pub use impetus_protocol::{
    DiffObservation, DiffSource, GitBranchInfo, GitChangeKind, GitChangedFile, GitCurrentBranch,
    GitDiffPayload, GitRepositoryState, GitStatusSnapshot,
};

/// Soft cap on patch text returned over IPC (bytes).
pub const GIT_DIFF_MAX_BYTES: usize = 256 * 1024;

#[derive(Debug, Error)]
pub enum GitOpsError {
    #[error("not a git repository: {0}")]
    NotARepo(String),
    #[error("invalid branch name: {0}")]
    InvalidBranchName(String),
    #[error("branch already exists: {0}")]
    BranchExists(String),
    #[error("unknown branch: {0}")]
    UnknownBranch(String),
    #[error("working tree has uncommitted changes")]
    Dirty,
    #[error("repository has an in-progress merge/rebase/cherry-pick or conflicts")]
    ConflictInProgress,
    #[error("session worktree binding is stale")]
    StaleWorktree,
    #[error("worktree path missing on disk: {0}")]
    PathMissing(String),
    #[error("git failed: {0}")]
    Git(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

/// Resolved git working directory for a session.
#[derive(Debug, Clone)]
pub struct GitSessionCwd {
    pub path: PathBuf,
    pub binding: Option<WorktreeBinding>,
}

/// Prefer active/stopped managed worktree path; fall back to workspace root.
pub fn resolve_session_git_cwd(
    worktrees: Option<&WorktreeManager>,
    session_id: Uuid,
    bound_worktree_id: Option<&str>,
    workspace_root: &Path,
) -> Result<GitSessionCwd, GitOpsError> {
    if let Some(mgr) = worktrees {
        if let Some(id) = bound_worktree_id
            && let Some(binding) = mgr
                .get_by_worktree_id(id)
                .map_err(|e| GitOpsError::Git(e.to_string()))?
        {
            return cwd_from_binding(binding);
        }
        if let Some(binding) = mgr
            .get_by_session(session_id)
            .map_err(|e| GitOpsError::Git(e.to_string()))?
            && binding.state != WorktreeLifecycleState::Closed
        {
            return cwd_from_binding(binding);
        }
    }
    Ok(GitSessionCwd {
        path: workspace_root.to_path_buf(),
        binding: None,
    })
}

fn cwd_from_binding(binding: WorktreeBinding) -> Result<GitSessionCwd, GitOpsError> {
    if binding.state == WorktreeLifecycleState::Stale {
        return Err(GitOpsError::StaleWorktree);
    }
    if !binding.path.exists() {
        return Err(GitOpsError::PathMissing(
            binding.path.to_string_lossy().into_owned(),
        ));
    }
    Ok(GitSessionCwd {
        path: binding.path.clone(),
        binding: Some(binding),
    })
}

pub fn get_repository_state(cwd: &GitSessionCwd) -> Result<GitRepositoryState, GitOpsError> {
    let repo_root = rev_parse_show_toplevel(&cwd.path)?;
    let head_sha = symbolic_or_none(&cwd.path, "HEAD")?;
    let (current_branch, detached) = current_branch_info(&cwd.path)?;
    let dirty = is_dirty(&cwd.path)?;
    let conflict_in_progress = has_conflict_in_progress(&cwd.path)?;
    Ok(GitRepositoryState {
        repo_root,
        worktree_path: cwd.path.clone(),
        head_sha,
        current_branch,
        detached,
        dirty,
        conflict_in_progress,
        worktree_id: cwd.binding.as_ref().map(|b| b.worktree_id.clone()),
        worktree_lifecycle: cwd.binding.as_ref().map(|b| b.state),
    })
}

pub fn list_branches(cwd: &Path) -> Result<Vec<GitBranchInfo>, GitOpsError> {
    ensure_repo(cwd)?;
    // format: refname|upstream|HEAD marker (*)
    let out = git(
        cwd,
        &[
            "for-each-ref",
            "--format=%(refname:short)%09%(upstream:short)%09%(HEAD)",
            "refs/heads",
        ],
    )?;
    let mut branches = Vec::new();
    for line in out.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let mut parts = line.splitn(3, '\t');
        let name = parts.next().unwrap_or("").to_string();
        if name.is_empty() {
            continue;
        }
        let upstream = parts.next().filter(|s| !s.is_empty()).map(str::to_string);
        let head_mark = parts.next().unwrap_or("");
        branches.push(GitBranchInfo {
            name,
            current: head_mark == "*",
            upstream,
        });
    }
    Ok(branches)
}

pub fn get_current_branch(cwd: &Path) -> Result<GitCurrentBranch, GitOpsError> {
    ensure_repo(cwd)?;
    let (name, detached) = current_branch_info(cwd)?;
    let head_sha = symbolic_or_none(cwd, "HEAD")?;
    Ok(GitCurrentBranch {
        name,
        detached,
        head_sha,
    })
}

pub fn create_branch(
    cwd: &Path,
    name: &str,
    checkout: bool,
) -> Result<GitCurrentBranch, GitOpsError> {
    ensure_repo(cwd)?;
    validate_branch_name(name)?;
    refuse_conflict(cwd)?;
    if branch_exists(cwd, name)? {
        return Err(GitOpsError::BranchExists(name.to_string()));
    }
    if checkout {
        refuse_dirty(cwd)?;
        git(cwd, &["checkout", "-b", name])?;
    } else {
        git(cwd, &["branch", name])?;
    }
    get_current_branch(cwd)
}

pub fn switch_branch(cwd: &Path, name: &str) -> Result<GitCurrentBranch, GitOpsError> {
    ensure_repo(cwd)?;
    validate_branch_name(name)?;
    refuse_conflict(cwd)?;
    refuse_dirty(cwd)?;
    if !branch_exists(cwd, name)? {
        return Err(GitOpsError::UnknownBranch(name.to_string()));
    }
    git(cwd, &["checkout", "--quiet", name])?;
    get_current_branch(cwd)
}

pub fn git_status(cwd: &Path) -> Result<GitStatusSnapshot, GitOpsError> {
    ensure_repo(cwd)?;
    let branch = get_current_branch(cwd)?;
    let files = list_changed_files(cwd)?;
    let dirty = !files.is_empty();
    let conflict_in_progress = has_conflict_in_progress(cwd)?;
    Ok(GitStatusSnapshot {
        branch,
        dirty,
        conflict_in_progress,
        files,
    })
}

pub fn list_changed_files(cwd: &Path) -> Result<Vec<GitChangedFile>, GitOpsError> {
    ensure_repo(cwd)?;
    // NUL-separated porcelain v1: robust for spaces / unicode / " -> " in names.
    let out = git(cwd, &["status", "--porcelain=v1", "-z", "-uall"])?;
    Ok(parse_porcelain_z(&out))
}

/// Parse `git status --porcelain=v1 -z` output into changed files.
fn parse_porcelain_z(raw: &str) -> Vec<GitChangedFile> {
    let bytes = raw.as_bytes();
    let mut files = Vec::new();
    let mut i = 0usize;
    while i < bytes.len() {
        // Skip empty records (leading/trailing NULs).
        if bytes[i] == 0 {
            i += 1;
            continue;
        }
        if i + 3 > bytes.len() {
            break;
        }
        let code = std::str::from_utf8(&bytes[i..i + 2]).unwrap_or("??");
        // XY<space>path\0  OR  for rename/copy: XY<space>path\0orig\0 — actually
        // with -z: "XY path\0" for normal; "XY\0old\0new\0" is wrong.
        // Spec: entry is `XY PATH\0` or for rename/copy `XY ORIG_PATH\0PATH\0`
        // where XY may be followed by space then first path until NUL.
        let after_xy = i + 2;
        let path_start = if after_xy < bytes.len() && bytes[after_xy] == b' ' {
            after_xy + 1
        } else {
            after_xy
        };
        let Some(first_nul) = bytes[path_start..].iter().position(|&b| b == 0) else {
            break;
        };
        let first_end = path_start + first_nul;
        let first = String::from_utf8_lossy(&bytes[path_start..first_end]).into_owned();
        i = first_end + 1;

        let kind = classify_porcelain(code);
        let is_rename_or_copy = code
            .as_bytes()
            .first()
            .is_some_and(|c| *c == b'R' || *c == b'C')
            || code
                .as_bytes()
                .get(1)
                .is_some_and(|c| *c == b'R' || *c == b'C');
        let path = if is_rename_or_copy {
            match bytes[i..].iter().position(|&b| b == 0) {
                Some(second_nul) => {
                    let second_end = i + second_nul;
                    let second = String::from_utf8_lossy(&bytes[i..second_end]).into_owned();
                    i = second_end + 1;
                    let _ = first;
                    PathBuf::from(second)
                }
                None => PathBuf::from(first),
            }
        } else {
            PathBuf::from(first)
        };
        files.push(GitChangedFile {
            path,
            kind,
            status_code: Some(code.to_string()),
        });
    }
    files
}

pub fn get_diff(cwd: &Path, base_ref: Option<&str>) -> Result<GitDiffPayload, GitOpsError> {
    ensure_repo(cwd)?;
    let mut args: Vec<&str> = vec!["diff", "--no-ext-diff"];
    if let Some(base) = base_ref {
        ensure_commit_ref(cwd, base)?;
        args.push(base);
    }
    let raw = git(cwd, &args)?;
    Ok(truncate_diff(raw, base_ref.map(str::to_string), None))
}

pub fn get_file_diff(
    cwd: &Path,
    path: &Path,
    base_ref: Option<&str>,
) -> Result<GitDiffPayload, GitOpsError> {
    ensure_repo(cwd)?;
    let path_str = path
        .to_str()
        .ok_or_else(|| GitOpsError::Git("file path is not valid UTF-8".into()))?;
    let mut cmd_args = vec!["diff", "--no-ext-diff"];
    if let Some(base) = base_ref {
        ensure_commit_ref(cwd, base)?;
        cmd_args.push(base);
    }
    cmd_args.push("--");
    cmd_args.push(path_str);
    let raw = git(cwd, &cmd_args)?;
    Ok(truncate_diff(
        raw,
        base_ref.map(str::to_string),
        Some(path.to_path_buf()),
    ))
}

fn truncate_diff(raw: String, base_ref: Option<String>, path: Option<PathBuf>) -> GitDiffPayload {
    let files_changed = raw.lines().filter(|l| l.starts_with("diff --git ")).count();
    let truncated = raw.len() > GIT_DIFF_MAX_BYTES;
    let patch = if truncated {
        let mut cut = raw;
        cut.truncate(GIT_DIFF_MAX_BYTES);
        cut.push_str("\n… truncated …\n");
        cut
    } else {
        raw
    };
    let observation = observation_from_patch(base_ref.as_deref(), &patch);
    GitDiffPayload {
        base_ref,
        path,
        patch,
        truncated,
        files_changed,
        observation,
    }
}

fn observation_from_patch(base_ref: Option<&str>, patch: &str) -> Option<DiffObservation> {
    if patch.trim().is_empty() {
        return None;
    }
    Some(from_unified(
        DiffSource::Git {
            commit_range: base_ref.map(str::to_string),
        },
        patch,
    ))
}

/// Overlay [`WorktreeManager::diff_summary`] numstat counts onto structured observation.
///
/// Patch → hunks still come from git unified text; counts prefer worktree summary when present.
pub fn apply_worktree_diff_counts(payload: &mut GitDiffPayload, summary: &WorktreeDiffSummary) {
    let Some(obs) = payload.observation.as_mut() else {
        return;
    };
    obs.files_changed = summary.files_changed as usize;
    obs.insertions = summary.insertions as usize;
    obs.deletions = summary.deletions as usize;
    obs.summary = format!(
        "{} files changed, {} insertions(+), {} deletions(-)",
        obs.files_changed, obs.insertions, obs.deletions
    );
}

fn classify_porcelain(code: &str) -> GitChangeKind {
    let chars: Vec<char> = code.chars().collect();
    let (x, y) = match chars.as_slice() {
        [a, b] => (*a, *b),
        _ => return GitChangeKind::Unknown,
    };
    if x == '?' || y == '?' {
        return GitChangeKind::Untracked;
    }
    if x == '!' || y == '!' {
        return GitChangeKind::Ignored;
    }
    if x == 'U' || y == 'U' || (x == 'A' && y == 'A') || (x == 'D' && y == 'D') {
        return GitChangeKind::Unmerged;
    }
    let primary = if y != ' ' { y } else { x };
    match primary {
        'M' => GitChangeKind::Modified,
        'A' => GitChangeKind::Added,
        'D' => GitChangeKind::Deleted,
        'R' => GitChangeKind::Renamed,
        'C' => GitChangeKind::Copied,
        _ => GitChangeKind::Unknown,
    }
}

fn validate_branch_name(name: &str) -> Result<(), GitOpsError> {
    let name = name.trim();
    if name.is_empty()
        || name.starts_with('-')
        || name.contains("..")
        || name.contains([' ', '~', '^', ':', '?', '*', '[', '\\'])
        || name.ends_with('/')
        || name.ends_with(".lock")
    {
        return Err(GitOpsError::InvalidBranchName(name.to_string()));
    }
    Ok(())
}

fn refuse_dirty(cwd: &Path) -> Result<(), GitOpsError> {
    if is_dirty(cwd)? {
        Err(GitOpsError::Dirty)
    } else {
        Ok(())
    }
}

fn refuse_conflict(cwd: &Path) -> Result<(), GitOpsError> {
    if has_conflict_in_progress(cwd)? {
        Err(GitOpsError::ConflictInProgress)
    } else {
        Ok(())
    }
}

fn is_dirty(cwd: &Path) -> Result<bool, GitOpsError> {
    let status = git(cwd, &["status", "--porcelain"])?;
    Ok(!status.trim().is_empty())
}

fn has_conflict_in_progress(cwd: &Path) -> Result<bool, GitOpsError> {
    // gitdir markers for in-progress operations
    let git_dir = PathBuf::from(git(cwd, &["rev-parse", "--git-dir"])?.trim());
    let git_dir = if git_dir.is_absolute() {
        git_dir
    } else {
        cwd.join(git_dir)
    };
    for marker in [
        "MERGE_HEAD",
        "REBASE_HEAD",
        "CHERRY_PICK_HEAD",
        "REVERT_HEAD",
        "sequencer/todo",
    ] {
        if git_dir.join(marker).exists() {
            return Ok(true);
        }
    }
    // Unmerged index entries
    let unmerged = git(cwd, &["diff", "--name-only", "--diff-filter=U"])?;
    Ok(!unmerged.trim().is_empty())
}

fn branch_exists(cwd: &Path, name: &str) -> Result<bool, GitOpsError> {
    let spec = format!("refs/heads/{name}");
    match git(cwd, &["show-ref", "--verify", "--quiet", &spec]) {
        Ok(_) => Ok(true),
        Err(GitOpsError::Git(_)) => Ok(false),
        Err(other) => Err(other),
    }
}

fn current_branch_info(cwd: &Path) -> Result<(Option<String>, bool), GitOpsError> {
    match git(cwd, &["symbolic-ref", "--quiet", "--short", "HEAD"]) {
        Ok(name) => Ok((Some(name.trim().to_string()), false)),
        Err(_) => {
            // Detached or empty
            let detached = git(cwd, &["rev-parse", "--verify", "HEAD"]).is_ok();
            Ok((None, detached))
        }
    }
}

fn symbolic_or_none(cwd: &Path, rev: &str) -> Result<Option<String>, GitOpsError> {
    match git(cwd, &["rev-parse", "--verify", rev]) {
        Ok(sha) => Ok(Some(sha.trim().to_string())),
        Err(GitOpsError::Git(_)) => Ok(None),
        Err(other) => Err(other),
    }
}

fn rev_parse_show_toplevel(cwd: &Path) -> Result<PathBuf, GitOpsError> {
    let out = git(cwd, &["rev-parse", "--show-toplevel"])?;
    Ok(PathBuf::from(out.trim()))
}

fn ensure_repo(cwd: &Path) -> Result<(), GitOpsError> {
    match git(cwd, &["rev-parse", "--is-inside-work-tree"]) {
        Ok(v) if v.trim() == "true" => Ok(()),
        Ok(_) | Err(GitOpsError::Git(_)) => {
            Err(GitOpsError::NotARepo(cwd.to_string_lossy().into_owned()))
        }
        Err(other) => Err(other),
    }
}

fn ensure_commit_ref(cwd: &Path, rev: &str) -> Result<(), GitOpsError> {
    let spec = format!("{rev}^{{commit}}");
    git(cwd, &["rev-parse", "--verify", &spec]).map(|_| ())
}

fn git(cwd: &Path, args: &[&str]) -> Result<String, GitOpsError> {
    let output = Command::new("git").args(args).current_dir(cwd).output()?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let detail = if stderr.is_empty() { stdout } else { stderr };
        return Err(GitOpsError::Git(if detail.is_empty() {
            format!("git {} failed", args.first().unwrap_or(&""))
        } else {
            detail
        }));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

#[cfg(test)]
mod sentinel_git {
    //! PR-safe Git ops lib suite (TODO P2 CI / #315).
    //!
    //! Filter: `cargo test -p impetus-core --lib sentinel_git`
    //! All named sentinels: `cargo test -p impetus-core --lib -- sentinel`
    //!
    //! Temp-repo status/branch/diff via system `git`. No Seatbelt / IPC socket.

    use super::*;
    use tempfile::TempDir;

    fn init_repo() -> TempDir {
        let dir = TempDir::new().expect("temp");
        git(dir.path(), &["init", "-b", "main"]).expect("init");
        git(dir.path(), &["config", "user.email", "test@example.com"]).expect("email");
        git(dir.path(), &["config", "user.name", "Test"]).expect("name");
        std::fs::write(dir.path().join("README"), b"seed").expect("write");
        git(dir.path(), &["add", "README"]).expect("add");
        git(dir.path(), &["commit", "-m", "seed"]).expect("commit");
        dir
    }

    #[test]
    fn status_and_list_branches_on_temp_repo() {
        let repo = init_repo();
        let cwd = repo.path();

        let branches = list_branches(cwd).expect("branches");
        assert!(
            branches.iter().any(|b| b.name == "main" && b.current),
            "expected current main: {branches:?}"
        );

        let status = git_status(cwd).expect("status");
        assert!(!status.dirty);
        assert_eq!(status.branch.name.as_deref(), Some("main"));
        assert!(status.files.is_empty());

        std::fs::write(cwd.join("dirty.txt"), b"x").expect("dirty");
        let status = git_status(cwd).expect("dirty status");
        assert!(status.dirty);
        assert!(
            status
                .files
                .iter()
                .any(|f| f.path.ends_with("dirty.txt") && f.kind == GitChangeKind::Untracked)
        );

        let changed = list_changed_files(cwd).expect("changed");
        assert_eq!(changed.len(), 1);
    }

    #[test]
    fn porcelain_z_handles_spaces_unicode_and_arrow_in_name() {
        let raw = "?? weird name with spaces.txt\0?? file with -> arrow.txt\0?? юникод.txt\0R  old.txt\0new name.txt\0";
        let files = parse_porcelain_z(raw);
        let names: Vec<String> = files
            .iter()
            .map(|f| f.path.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert!(names.iter().any(|n| n == "weird name with spaces.txt"));
        assert!(names.iter().any(|n| n == "file with -> arrow.txt"));
        assert!(names.iter().any(|n| n == "юникод.txt"));
        assert!(names.iter().any(|n| n == "new name.txt"));
        assert!(!names.iter().any(|n| n == "old.txt"));
    }

    #[test]
    fn create_and_switch_branch_with_dirty_guard() {
        let repo = init_repo();
        let cwd = repo.path();

        create_branch(cwd, "feature/x", false).expect("create");
        let branches = list_branches(cwd).expect("list");
        assert!(branches.iter().any(|b| b.name == "feature/x" && !b.current));

        let after = switch_branch(cwd, "feature/x").expect("switch");
        assert_eq!(after.name.as_deref(), Some("feature/x"));

        std::fs::write(cwd.join("wip"), b"1").expect("wip");
        let err = switch_branch(cwd, "main").expect_err("dirty switch");
        assert!(matches!(err, GitOpsError::Dirty));

        // create without checkout still ok while dirty
        create_branch(cwd, "feature/y", false).expect("create dirty");
        let err = create_branch(cwd, "feature/z", true).expect_err("checkout dirty");
        assert!(matches!(err, GitOpsError::Dirty));
    }

    #[test]
    fn repository_state_and_diff() {
        let repo = init_repo();
        let cwd = GitSessionCwd {
            path: repo.path().to_path_buf(),
            binding: None,
        };
        let state = get_repository_state(&cwd).expect("state");
        assert_eq!(state.current_branch.as_deref(), Some("main"));
        assert!(!state.detached);
        assert!(!state.dirty);

        std::fs::write(repo.path().join("README"), b"seed\nedit").expect("edit");
        let diff = get_diff(repo.path(), None).expect("diff");
        assert!(diff.patch.contains("README") || diff.files_changed >= 1 || !diff.patch.is_empty());
        let obs = diff.observation.expect("observation from patch");
        assert!(obs.files_changed >= 1);
        assert!(!obs.hunks.is_empty());
        let file_diff = get_file_diff(repo.path(), Path::new("README"), None).expect("file diff");
        assert!(!file_diff.patch.is_empty() || file_diff.files_changed <= 1);
        assert!(file_diff.observation.is_some());
    }

    #[test]
    fn resolve_cwd_prefers_active_worktree() {
        let repo = init_repo();
        let store = TempDir::new().expect("store");
        let wts = TempDir::new().expect("wts");
        let mgr = WorktreeManager::open(store.path().join("wt.db"), wts.path()).expect("mgr");
        let session = Uuid::new_v4();
        let binding = mgr.create(session, repo.path()).expect("create wt");
        assert_eq!(binding.state, WorktreeLifecycleState::Active);

        let resolved =
            resolve_session_git_cwd(Some(&mgr), session, Some(&binding.worktree_id), repo.path())
                .expect("resolve");
        assert_eq!(resolved.path, binding.path);
        assert_eq!(
            resolved.binding.as_ref().map(|b| b.worktree_id.as_str()),
            Some(binding.worktree_id.as_str())
        );

        let status = git_status(&resolved.path).expect("wt status");
        assert_eq!(status.branch.name.as_deref(), Some(binding.branch.as_str()));
    }
}
