//! Structured workspace Files API (list / stat / read / search).
//!
//! Path resolution matches [`crate::memory_store::resolve_index_path`] rigor:
//! refuse absolute relatives, `..` components, and symlink escape outside the
//! workspace root. Binary and oversized files are rejected, not silently
//! mangled. Search skips common build/VCS directories.

use std::io::Read;
use std::path::{Component, Path, PathBuf};
use thiserror::Error;

pub use impetus_protocol::MAX_WORKSPACE_FILE_BYTES;
/// Bytes inspected for NUL / UTF-8 before accepting a text read.
pub const BINARY_PROBE_BYTES: usize = 8 * 1024;
/// Max files scanned per search call.
pub const MAX_WORKSPACE_SEARCH_FILES: usize = 200;
/// Max search hit lines collected.
pub const MAX_WORKSPACE_SEARCH_HITS: usize = 200;

/// Directory names skipped when listing children and when descending for search.
pub const IGNORED_DIR_NAMES: &[&str] = &[".git", "target", "node_modules"];

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum WorkspaceFilesError {
    #[error("workspace path escapes root or follows unsafe symlink: {0}")]
    UnsafePath(String),
    #[error("workspace path not found: {0}")]
    NotFound(String),
    #[error("workspace path is not a directory: {0}")]
    NotDirectory(String),
    #[error("workspace path is not a file: {0}")]
    NotFile(String),
    #[error("workspace file too large: {size} bytes (limit {limit})")]
    TooLarge { size: u64, limit: usize },
    #[error("workspace file looks binary: {0}")]
    Binary(String),
    #[error("workspace files io error: {0}")]
    Io(String),
}

pub use impetus_protocol::{
    WorkspaceDirEntry, WorkspaceDirListing, WorkspaceFileContent, WorkspaceFileMetadata,
    WorkspaceSearchHit, WorkspaceSearchResult,
};

/// Resolve `relative` under `workspace_root`.
///
/// Refuses absolute `relative`, `..` / prefix components, and any path whose
/// canonical form (following symlinks) lies outside `workspace_root`.
pub fn resolve_workspace_path(
    workspace_root: &Path,
    relative: &Path,
) -> Result<PathBuf, WorkspaceFilesError> {
    let display = relative.display().to_string();
    if relative.is_absolute() {
        return Err(WorkspaceFilesError::UnsafePath(display));
    }
    for component in relative.components() {
        match component {
            Component::Normal(_) | Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(WorkspaceFilesError::UnsafePath(display));
            }
        }
    }

    let root = workspace_root
        .canonicalize()
        .map_err(|err| WorkspaceFilesError::Io(format!("canonicalize workspace root: {err}")))?;

    // Empty / "." → workspace root itself.
    if relative.as_os_str().is_empty() || relative == Path::new(".") {
        return Ok(root);
    }

    let candidate = root.join(relative);
    if let Ok(resolved) = candidate.canonicalize() {
        if !resolved.starts_with(&root) {
            return Err(WorkspaceFilesError::UnsafePath(display));
        }
        return Ok(resolved);
    }

    // Missing leaf: prove parent stays inside root (covers symlink parents).
    let parent = candidate.parent().unwrap_or(root.as_path());
    let file_name = candidate
        .file_name()
        .ok_or_else(|| WorkspaceFilesError::UnsafePath(display.clone()))?;
    let parent_canon = parent.canonicalize().map_err(|err| {
        WorkspaceFilesError::Io(format!("canonicalize parent {}: {err}", parent.display()))
    })?;
    if !parent_canon.starts_with(&root) {
        return Err(WorkspaceFilesError::UnsafePath(display));
    }
    Ok(parent_canon.join(file_name))
}

fn relative_display(workspace_root: &Path, absolute: &Path) -> String {
    let Ok(root) = workspace_root.canonicalize() else {
        return absolute.display().to_string();
    };
    absolute
        .strip_prefix(&root)
        .map(|p| {
            if p.as_os_str().is_empty() {
                ".".to_string()
            } else {
                p.display().to_string()
            }
        })
        .unwrap_or_else(|_| absolute.display().to_string())
}

fn is_ignored_name(name: &str) -> bool {
    IGNORED_DIR_NAMES.contains(&name)
}

fn looks_binary(bytes: &[u8]) -> bool {
    if bytes.contains(&0) {
        return true;
    }
    std::str::from_utf8(bytes).is_err()
}

/// List directory entries under a workspace-relative path.
pub fn list_directory(
    workspace_root: &Path,
    relative: &Path,
) -> Result<WorkspaceDirListing, WorkspaceFilesError> {
    let resolved = resolve_workspace_path(workspace_root, relative)?;
    let meta = std::fs::symlink_metadata(&resolved).map_err(|err| {
        if err.kind() == std::io::ErrorKind::NotFound {
            WorkspaceFilesError::NotFound(relative.display().to_string())
        } else {
            WorkspaceFilesError::Io(err.to_string())
        }
    })?;
    if !meta.is_dir() {
        return Err(WorkspaceFilesError::NotDirectory(
            relative.display().to_string(),
        ));
    }

    let mut entries = Vec::new();
    let read =
        std::fs::read_dir(&resolved).map_err(|err| WorkspaceFilesError::Io(err.to_string()))?;
    for entry in read {
        let entry = entry.map_err(|err| WorkspaceFilesError::Io(err.to_string()))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if is_ignored_name(&name) {
            continue;
        }
        let ft = entry
            .file_type()
            .map_err(|err| WorkspaceFilesError::Io(err.to_string()))?;
        let child_abs = entry.path();
        let path = relative_display(workspace_root, &child_abs);
        entries.push(WorkspaceDirEntry {
            name,
            path,
            is_dir: ft.is_dir(),
            is_symlink: ft.is_symlink(),
            is_file: ft.is_file(),
        });
    }
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(WorkspaceDirListing {
        path: relative_display(workspace_root, &resolved),
        entries,
    })
}

/// Stat a workspace-relative path (symlink escape refused).
pub fn stat_path(
    workspace_root: &Path,
    relative: &Path,
) -> Result<WorkspaceFileMetadata, WorkspaceFilesError> {
    let resolved = resolve_workspace_path(workspace_root, relative)?;
    let candidate = {
        let root = workspace_root
            .canonicalize()
            .map_err(|err| WorkspaceFilesError::Io(err.to_string()))?;
        if relative.as_os_str().is_empty() || relative == Path::new(".") {
            root
        } else {
            root.join(relative)
        }
    };
    let link_meta = std::fs::symlink_metadata(&candidate).map_err(|err| {
        if err.kind() == std::io::ErrorKind::NotFound {
            WorkspaceFilesError::NotFound(relative.display().to_string())
        } else {
            WorkspaceFilesError::Io(err.to_string())
        }
    })?;
    let is_symlink = link_meta.file_type().is_symlink();
    // Follow for size / type of target; resolve already proved containment.
    let meta = std::fs::metadata(&resolved).map_err(|err| {
        if err.kind() == std::io::ErrorKind::NotFound {
            WorkspaceFilesError::NotFound(relative.display().to_string())
        } else {
            WorkspaceFilesError::Io(err.to_string())
        }
    })?;
    Ok(WorkspaceFileMetadata {
        path: relative_display(workspace_root, &resolved),
        is_dir: meta.is_dir(),
        is_symlink,
        is_file: meta.is_file(),
        size: meta.len(),
    })
}

/// Read a text file; rejects oversized and binary content.
pub fn read_text_file(
    workspace_root: &Path,
    relative: &Path,
) -> Result<WorkspaceFileContent, WorkspaceFilesError> {
    read_text_file_limited(workspace_root, relative, MAX_WORKSPACE_FILE_BYTES)
}

/// Read a text file with an explicit byte cap (still hard-capped by
/// [`MAX_WORKSPACE_FILE_BYTES`]).
pub fn read_text_file_limited(
    workspace_root: &Path,
    relative: &Path,
    max_bytes: usize,
) -> Result<WorkspaceFileContent, WorkspaceFilesError> {
    let limit = max_bytes.min(MAX_WORKSPACE_FILE_BYTES);
    let resolved = resolve_workspace_path(workspace_root, relative)?;
    let meta = std::fs::metadata(&resolved).map_err(|err| {
        if err.kind() == std::io::ErrorKind::NotFound {
            WorkspaceFilesError::NotFound(relative.display().to_string())
        } else {
            WorkspaceFilesError::Io(err.to_string())
        }
    })?;
    if meta.is_dir() {
        return Err(WorkspaceFilesError::NotFile(relative.display().to_string()));
    }
    if meta.len() > limit as u64 {
        return Err(WorkspaceFilesError::TooLarge {
            size: meta.len(),
            limit,
        });
    }

    let mut file =
        std::fs::File::open(&resolved).map_err(|err| WorkspaceFilesError::Io(err.to_string()))?;
    let mut probe = vec![0u8; BINARY_PROBE_BYTES.min(limit)];
    let probe_n = file
        .read(&mut probe)
        .map_err(|err| WorkspaceFilesError::Io(err.to_string()))?;
    probe.truncate(probe_n);
    if looks_binary(&probe) {
        return Err(WorkspaceFilesError::Binary(relative.display().to_string()));
    }

    let mut bytes = probe;
    file.take(limit.saturating_sub(bytes.len()) as u64)
        .read_to_end(&mut bytes)
        .map_err(|err| WorkspaceFilesError::Io(err.to_string()))?;
    if looks_binary(&bytes) {
        return Err(WorkspaceFilesError::Binary(relative.display().to_string()));
    }
    let content = String::from_utf8(bytes)
        .map_err(|_| WorkspaceFilesError::Binary(relative.display().to_string()))?;
    let byte_count = content.len();
    Ok(WorkspaceFileContent {
        path: relative_display(workspace_root, &resolved),
        content,
        byte_count,
    })
}

/// Case-insensitive substring search under a workspace path.
pub fn search_text(
    workspace_root: &Path,
    relative: &Path,
    pattern: &str,
) -> Result<WorkspaceSearchResult, WorkspaceFilesError> {
    if pattern.is_empty() {
        return Ok(WorkspaceSearchResult {
            path: relative_display(
                workspace_root,
                &resolve_workspace_path(workspace_root, relative)?,
            ),
            pattern: pattern.to_string(),
            hits: Vec::new(),
            truncated: false,
        });
    }

    let start = resolve_workspace_path(workspace_root, relative)?;
    let needle = pattern.to_lowercase();
    let mut hits = Vec::new();
    let mut truncated = false;
    let mut scanned = 0usize;
    let mut stack = vec![start.clone()];

    while let Some(dir) = stack.pop() {
        if scanned >= MAX_WORKSPACE_SEARCH_FILES || hits.len() >= MAX_WORKSPACE_SEARCH_HITS {
            truncated = true;
            break;
        }
        let Ok(read) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in read.flatten() {
            if scanned >= MAX_WORKSPACE_SEARCH_FILES || hits.len() >= MAX_WORKSPACE_SEARCH_HITS {
                truncated = true;
                break;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            if is_ignored_name(&name) {
                continue;
            }
            let Ok(ft) = entry.file_type() else {
                continue;
            };
            let path = entry.path();
            if ft.is_symlink() {
                // Only follow if resolve keeps us inside root.
                if let Ok(resolved) = path.canonicalize() {
                    let Ok(root) = workspace_root.canonicalize() else {
                        continue;
                    };
                    if !resolved.starts_with(&root) {
                        continue;
                    }
                    if resolved.is_dir() {
                        stack.push(resolved);
                    } else if resolved.is_file() {
                        scanned += 1;
                        if search_file(&resolved, workspace_root, &needle, &mut hits)? {
                            truncated = true;
                            break;
                        }
                    }
                }
                continue;
            }
            if ft.is_dir() {
                stack.push(path);
            } else if ft.is_file() {
                scanned += 1;
                if search_file(&path, workspace_root, &needle, &mut hits)? {
                    truncated = true;
                    break;
                }
            }
        }
    }

    Ok(WorkspaceSearchResult {
        path: relative_display(workspace_root, &start),
        pattern: pattern.to_string(),
        hits,
        truncated,
    })
}

fn search_file(
    path: &Path,
    workspace_root: &Path,
    needle: &str,
    hits: &mut Vec<WorkspaceSearchHit>,
) -> Result<bool, WorkspaceFilesError> {
    let Ok(meta) = std::fs::metadata(path) else {
        return Ok(false);
    };
    if meta.len() > MAX_WORKSPACE_FILE_BYTES as u64 {
        return Ok(false);
    }
    let mut bytes = Vec::with_capacity(meta.len() as usize);
    let Ok(file) = std::fs::File::open(path) else {
        return Ok(false);
    };
    if file
        .take(MAX_WORKSPACE_FILE_BYTES as u64)
        .read_to_end(&mut bytes)
        .is_err()
    {
        return Ok(false);
    }
    if looks_binary(&bytes) {
        return Ok(false);
    }
    let Ok(text) = String::from_utf8(bytes) else {
        return Ok(false);
    };
    let rel = relative_display(workspace_root, path);
    for (idx, line) in text.lines().enumerate() {
        if line.to_lowercase().contains(needle) {
            hits.push(WorkspaceSearchHit {
                path: rel.clone(),
                line: (idx + 1) as u32,
                text: line.to_string(),
            });
            if hits.len() >= MAX_WORKSPACE_SEARCH_HITS {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

#[cfg(test)]
mod sentinel_files {
    //! PR-safe workspace Files lib suite (TODO P2 CI / #315).
    //!
    //! Filter: `cargo test -p impetus-core --lib sentinel_files`
    //! All named sentinels: `cargo test -p impetus-core --lib -- sentinel`
    //!
    //! Path-safe list/read/stat/search. No daemon socket / Seatbelt.

    use super::*;
    use std::fs;
    use std::os::unix::fs::symlink;
    use tempfile::tempdir;

    fn write(root: &Path, rel: &str, body: &str) {
        let path = root.join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("mkdir");
        }
        fs::write(path, body).expect("write");
    }

    #[test]
    fn list_and_read_text_in_temp_workspace() {
        let dir = tempdir().expect("temp");
        let root = dir.path();
        write(root, "src/main.rs", "fn main() {}\n");
        write(root, "README.md", "hello\n");
        fs::create_dir_all(root.join(".git")).expect("git");
        fs::create_dir_all(root.join("target")).expect("target");
        fs::create_dir_all(root.join("node_modules")).expect("nm");

        let listing = list_directory(root, Path::new(".")).expect("list");
        let names: Vec<_> = listing.entries.iter().map(|e| e.name.as_str()).collect();
        assert!(names.contains(&"src"));
        assert!(names.contains(&"README.md"));
        assert!(!names.contains(&".git"));
        assert!(!names.contains(&"target"));
        assert!(!names.contains(&"node_modules"));

        let content = read_text_file(root, Path::new("README.md")).expect("read");
        assert_eq!(content.content, "hello\n");
        assert_eq!(content.path, "README.md");

        let stat = stat_path(root, Path::new("src/main.rs")).expect("stat");
        assert!(stat.is_file);
        assert!(!stat.is_dir);
        assert_eq!(stat.size, b"fn main() {}\n".len() as u64);
    }

    #[test]
    fn deny_parent_escape_and_absolute() {
        let dir = tempdir().expect("temp");
        let root = dir.path();
        write(root, "ok.txt", "x");

        let err = resolve_workspace_path(root, Path::new("../escape")).expect_err("parent");
        assert!(matches!(err, WorkspaceFilesError::UnsafePath(_)));

        let abs = root.join("ok.txt");
        let err = resolve_workspace_path(root, &abs).expect_err("absolute");
        assert!(matches!(err, WorkspaceFilesError::UnsafePath(_)));

        let err = list_directory(root, Path::new("../escape")).expect_err("list escape");
        assert!(matches!(err, WorkspaceFilesError::UnsafePath(_)));
    }

    #[test]
    fn deny_symlink_escape_outside_root() {
        let workspace = tempdir().expect("ws");
        let outside = tempdir().expect("out");
        fs::write(outside.path().join("secret.txt"), "leak").expect("secret");
        let link = workspace.path().join("escape");
        symlink(outside.path(), &link).expect("symlink");

        let err = resolve_workspace_path(workspace.path(), Path::new("escape"))
            .expect_err("symlink escape");
        assert!(matches!(err, WorkspaceFilesError::UnsafePath(_)));

        let err = read_text_file(workspace.path(), Path::new("escape/secret.txt"))
            .expect_err("read via symlink");
        assert!(matches!(err, WorkspaceFilesError::UnsafePath(_)));

        let err = stat_path(workspace.path(), Path::new("escape")).expect_err("stat symlink");
        assert!(matches!(err, WorkspaceFilesError::UnsafePath(_)));
    }

    #[test]
    fn reject_huge_and_binary() {
        let dir = tempdir().expect("temp");
        let root = dir.path();
        let huge = vec![b'a'; MAX_WORKSPACE_FILE_BYTES + 1];
        fs::write(root.join("huge.txt"), &huge).expect("huge");
        let err = read_text_file(root, Path::new("huge.txt")).expect_err("huge");
        assert!(matches!(
            err,
            WorkspaceFilesError::TooLarge { size, limit }
            if size == (MAX_WORKSPACE_FILE_BYTES as u64 + 1) && limit == MAX_WORKSPACE_FILE_BYTES
        ));

        fs::write(root.join("bin.dat"), [0u8, 1, 2, 3, b'x']).expect("bin");
        let err = read_text_file(root, Path::new("bin.dat")).expect_err("binary");
        assert!(matches!(err, WorkspaceFilesError::Binary(_)));
    }

    #[test]
    fn search_skips_ignored_and_finds_text() {
        let dir = tempdir().expect("temp");
        let root = dir.path();
        write(root, "src/a.rs", "needle here\n");
        write(root, "target/hidden.rs", "needle hidden\n");
        write(root, "node_modules/pkg/index.js", "needle nm\n");

        let result = search_text(root, Path::new("."), "needle").expect("search");
        assert_eq!(result.hits.len(), 1);
        assert_eq!(result.hits[0].path, "src/a.rs");
        assert_eq!(result.hits[0].line, 1);
    }
}
