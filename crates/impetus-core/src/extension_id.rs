//! Canonical extension identifier validation and install-path containment.
//!
//! IDs must match `[a-z0-9][a-z0-9_-]{0,63}` (length 1..=64). Generated
//! destinations must stay under `target_root/.impetus/<skills|mcp>/`.

use std::path::{Component, Path, PathBuf};

use thiserror::Error;

/// Relative type directory under `.impetus/`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExtensionTypeDir {
    Skills,
    Mcp,
}

impl ExtensionTypeDir {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Skills => "skills",
            Self::Mcp => "mcp",
        }
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ExtensionIdError {
    #[error("extension id must be non-empty")]
    Empty,
    #[error(
        "extension id `{id}` is invalid: must match [a-z0-9][a-z0-9_-]{{0,63}} (length 1..=64)"
    )]
    InvalidFormat { id: String },
    #[error("extension path escapes type root: {0}")]
    PathEscape(String),
}

/// Trim, lowercase, spaces → `-`, then require restrictive allowlist.
///
/// Replacement alone is not enough: path separators, `.`, `..`, and other
/// characters are rejected after normalization.
pub fn normalize_extension_id(raw: &str) -> Result<String, ExtensionIdError> {
    let normalized = raw.trim().to_lowercase().replace(' ', "-");
    if normalized.is_empty() {
        return Err(ExtensionIdError::Empty);
    }
    if !is_valid_extension_id(&normalized) {
        return Err(ExtensionIdError::InvalidFormat { id: normalized });
    }
    Ok(normalized)
}

/// True iff `id` already matches `[a-z0-9][a-z0-9_-]{0,63}` (len 1..=64).
pub fn is_valid_extension_id(id: &str) -> bool {
    let bytes = id.as_bytes();
    if !(1..=64).contains(&bytes.len()) {
        return false;
    }
    let first = bytes[0];
    if !first.is_ascii_lowercase() && !first.is_ascii_digit() {
        return false;
    }
    bytes[1..]
        .iter()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'_' || *b == b'-')
}

/// `{target_root}/.impetus/{skills|mcp}` (not yet containment-checked).
pub fn extension_type_root(target_root: &Path, type_dir: ExtensionTypeDir) -> PathBuf {
    target_root.join(".impetus").join(type_dir.as_str())
}

/// Skill dest: `{target_root}/.impetus/skills/{id}/SKILL.md`.
pub fn skill_install_path(target_root: &Path, skill_id: &str) -> Result<PathBuf, ExtensionIdError> {
    let id = normalize_extension_id(skill_id)?;
    let type_root = extension_type_root(target_root, ExtensionTypeDir::Skills);
    join_under_extension_root(&type_root, Path::new(&id).join("SKILL.md"))
}

/// MCP dest: `{target_root}/.impetus/mcp/{id}.json`.
pub fn mcp_install_path(target_root: &Path, module_id: &str) -> Result<PathBuf, ExtensionIdError> {
    let id = normalize_extension_id(module_id)?;
    let type_root = extension_type_root(target_root, ExtensionTypeDir::Mcp);
    join_under_extension_root(&type_root, Path::new(&format!("{id}.json")))
}

fn reject_unsafe_relative(relative: &Path) -> Result<(), ExtensionIdError> {
    if relative.as_os_str().is_empty() {
        return Err(ExtensionIdError::PathEscape(relative.display().to_string()));
    }
    for component in relative.components() {
        match component {
            Component::Normal(_) => {}
            Component::CurDir
            | Component::ParentDir
            | Component::RootDir
            | Component::Prefix(_) => {
                return Err(ExtensionIdError::PathEscape(relative.display().to_string()));
            }
        }
    }
    Ok(())
}

/// Join `relative` under `type_root` and prove the result stays inside it.
///
/// Rejects absolute `relative`, empty, `.`, `..`, and any parent-dir component.
/// When `type_root` (or parents) already exist, canonicalize and check
/// `starts_with`. Does **not** create directories (safe for dry-run plan).
pub fn join_under_extension_root(
    type_root: &Path,
    relative: impl AsRef<Path>,
) -> Result<PathBuf, ExtensionIdError> {
    let relative = relative.as_ref();
    reject_unsafe_relative(relative)?;

    let candidate = type_root.join(relative);

    if let Ok(root) = type_root.canonicalize() {
        if let Ok(resolved) = candidate.canonicalize() {
            if !resolved.starts_with(&root) {
                return Err(ExtensionIdError::PathEscape(relative.display().to_string()));
            }
            return Ok(resolved);
        }
        if let (Some(parent), Some(name)) = (candidate.parent(), candidate.file_name())
            && let Ok(parent_canon) = parent.canonicalize()
        {
            if !parent_canon.starts_with(&root) {
                return Err(ExtensionIdError::PathEscape(relative.display().to_string()));
            }
            return Ok(parent_canon.join(name));
        }
        // Type root exists; relative has no `..` — lexical join under canonical root.
        return Ok(root.join(relative));
    }

    // Type root missing (dry-run): relative already proven free of traversal.
    Ok(candidate)
}

/// Fail closed if `path` is not the expected install leaf for `module_id`.
///
/// Used by uninstall/repair so ownership rows cannot point outside
/// `.impetus/<type>/{id}/…`.
pub fn ensure_owned_extension_path(
    path: &Path,
    type_dir: ExtensionTypeDir,
    module_id: &str,
) -> Result<(), ExtensionIdError> {
    let id = normalize_extension_id(module_id)?;
    if path.components().any(|c| matches!(c, Component::ParentDir)) {
        return Err(ExtensionIdError::PathEscape(path.display().to_string()));
    }
    let expected_tail = match type_dir {
        ExtensionTypeDir::Skills => PathBuf::from(".impetus")
            .join("skills")
            .join(&id)
            .join("SKILL.md"),
        ExtensionTypeDir::Mcp => PathBuf::from(".impetus")
            .join("mcp")
            .join(format!("{id}.json")),
    };
    if !path.ends_with(&expected_tail) {
        return Err(ExtensionIdError::PathEscape(path.display().to_string()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_accepts_valid_ids_with_dash_underscore() {
        assert_eq!(normalize_extension_id("demo-skill").unwrap(), "demo-skill");
        assert_eq!(normalize_extension_id("demo_skill").unwrap(), "demo_skill");
        assert_eq!(normalize_extension_id("a").unwrap(), "a");
        assert_eq!(normalize_extension_id("0x").unwrap(), "0x");
        assert_eq!(normalize_extension_id("My Skill").unwrap(), "my-skill");
    }

    #[test]
    fn normalize_rejects_traversal_and_separators() {
        for bad in [
            "../../escape",
            "../foo",
            "foo/bar",
            "foo\\bar",
            ".",
            "..",
            "/tmp/foo",
            "",
            " ",
            &"a".repeat(65),
        ] {
            assert!(
                normalize_extension_id(bad).is_err(),
                "expected reject for {bad:?}"
            );
        }
    }

    #[test]
    fn skill_and_mcp_paths_stay_under_type_root() {
        let target = tempfile::tempdir().expect("target");
        // Pre-create so canonicalize path is exercised.
        std::fs::create_dir_all(extension_type_root(target.path(), ExtensionTypeDir::Skills))
            .unwrap();
        std::fs::create_dir_all(extension_type_root(target.path(), ExtensionTypeDir::Mcp)).unwrap();

        let skill = skill_install_path(target.path(), "demo-skill").expect("skill");
        let mcp = mcp_install_path(target.path(), "filesystem").expect("mcp");

        let skills_root = extension_type_root(target.path(), ExtensionTypeDir::Skills)
            .canonicalize()
            .unwrap();
        let mcp_root = extension_type_root(target.path(), ExtensionTypeDir::Mcp)
            .canonicalize()
            .unwrap();

        assert!(skill.starts_with(&skills_root));
        assert!(skill.ends_with(Path::new("demo-skill/SKILL.md")));
        assert!(mcp.starts_with(&mcp_root));
        assert!(mcp.ends_with("filesystem.json"));
    }

    #[test]
    fn skill_install_path_dry_run_does_not_create_dirs() {
        let target = tempfile::tempdir().expect("target");
        let before = std::fs::read_dir(target.path()).unwrap().count();
        let dest = skill_install_path(target.path(), "demo-skill").expect("skill");
        assert!(!dest.exists());
        assert_eq!(std::fs::read_dir(target.path()).unwrap().count(), before);
    }

    #[test]
    fn join_rejects_parent_dir_relative() {
        let target = tempfile::tempdir().expect("target");
        let type_root = extension_type_root(target.path(), ExtensionTypeDir::Skills);
        let err = join_under_extension_root(&type_root, Path::new("../escape")).unwrap_err();
        assert!(matches!(err, ExtensionIdError::PathEscape(_)));
    }

    #[test]
    fn ensure_owned_path_accepts_install_layout_rejects_escape() {
        let target = tempfile::tempdir().expect("target");
        let skill = skill_install_path(target.path(), "demo-skill").expect("skill");
        ensure_owned_extension_path(&skill, ExtensionTypeDir::Skills, "demo-skill")
            .expect("owned skill ok");

        let outside = target.path().join("escape.txt");
        assert!(matches!(
            ensure_owned_extension_path(&outside, ExtensionTypeDir::Skills, "demo-skill"),
            Err(ExtensionIdError::PathEscape(_))
        ));

        let wrong_id = extension_type_root(target.path(), ExtensionTypeDir::Skills)
            .join("other")
            .join("SKILL.md");
        assert!(matches!(
            ensure_owned_extension_path(&wrong_id, ExtensionTypeDir::Skills, "demo-skill"),
            Err(ExtensionIdError::PathEscape(_))
        ));
    }
}
