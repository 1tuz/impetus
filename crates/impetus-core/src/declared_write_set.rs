//! DeclaredWriteSet + writer lease / handoff fence (#417 / parent #397).
//!
//! Logical conflict avoidance for concurrent writer children. Distinct from
//! WorktreeManager physical isolation and from capability leases (TTL/Policy).
//! Unknown (empty) scope is treated conservatively as conflicting with any
//! other active writer. Does **not** spawn processes or touch secrets.

use std::collections::HashMap;
use std::path::{Component, Path};

use thiserror::Error;

/// Anticipated write scope under a workspace (paths and/or globs).
///
/// Empty paths **and** globs = unknown scope → overlaps everything.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DeclaredWriteSet {
    /// Relative file/dir paths (posix-style after normalize).
    pub paths: Vec<String>,
    /// Simple globs: `*` = one segment, `**` = any depth. Relative only.
    pub globs: Vec<String>,
}

impl DeclaredWriteSet {
    pub fn new() -> Self {
        Self::default()
    }

    /// Unknown / undeclared scope — conservative conflict with any peer.
    pub fn unknown() -> Self {
        Self::default()
    }

    pub fn from_paths(
        paths: impl IntoIterator<Item = impl Into<String>>,
    ) -> Result<Self, WriteSetError> {
        let mut set = Self::new();
        for p in paths {
            set.add_path(p)?;
        }
        Ok(set)
    }

    pub fn from_globs(
        globs: impl IntoIterator<Item = impl Into<String>>,
    ) -> Result<Self, WriteSetError> {
        let mut set = Self::new();
        for g in globs {
            set.add_glob(g)?;
        }
        Ok(set)
    }

    pub fn is_unknown(&self) -> bool {
        self.paths.is_empty() && self.globs.is_empty()
    }

    pub fn add_path(&mut self, path: impl Into<String>) -> Result<(), WriteSetError> {
        let n = normalize_rel(&path.into())?;
        if !self.paths.iter().any(|p| p == &n) {
            self.paths.push(n);
        }
        Ok(())
    }

    pub fn add_glob(&mut self, glob: impl Into<String>) -> Result<(), WriteSetError> {
        let n = normalize_rel(&glob.into())?;
        if !self.globs.iter().any(|g| g == &n) {
            self.globs.push(n);
        }
        Ok(())
    }

    /// True when scopes may touch the same workspace path.
    pub fn overlaps(&self, other: &Self) -> bool {
        if self.is_unknown() || other.is_unknown() {
            return true;
        }
        for a in &self.paths {
            if other.covers(a) {
                return true;
            }
        }
        for b in &other.paths {
            if self.covers(b) {
                return true;
            }
        }
        for ag in &self.globs {
            for bg in &other.globs {
                if globs_may_overlap(ag, bg) {
                    return true;
                }
            }
            for bp in &other.paths {
                if glob_matches(ag, bp) {
                    return true;
                }
            }
        }
        for bg in &other.globs {
            for ap in &self.paths {
                if glob_matches(bg, ap) {
                    return true;
                }
            }
        }
        false
    }

    fn covers(&self, candidate: &str) -> bool {
        for p in &self.paths {
            if path_covers(p, candidate) || path_covers(candidate, p) {
                return true;
            }
        }
        self.globs.iter().any(|g| glob_matches(g, candidate))
    }
}

/// Child handoff fence: owner id + generation + declared write scope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriterHandoffFence {
    pub child_id: String,
    pub generation: u64,
    pub write_set: DeclaredWriteSet,
}

impl WriterHandoffFence {
    pub fn new(
        child_id: impl Into<String>,
        generation: u64,
        write_set: DeclaredWriteSet,
    ) -> Result<Self, WriteSetError> {
        let child_id = child_id.into();
        if child_id.trim().is_empty() {
            return Err(WriteSetError::EmptyChildId);
        }
        Ok(Self {
            child_id,
            generation,
            write_set,
        })
    }
}

/// Failures from write-set normalize / lease admission.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum WriteSetError {
    #[error("child_id must be non-empty")]
    EmptyChildId,
    #[error("write path must be non-empty")]
    EmptyPath,
    #[error("write path escapes workspace: {0}")]
    PathEscape(String),
    #[error("absolute write path forbidden: {0}")]
    AbsolutePath(String),
    #[error("overlapping DeclaredWriteSet with active writer {holder}")]
    Conflict { holder: String },
    #[error("stale writer fence for {child_id}: offered gen {offered}, live gen {live}")]
    Stale {
        child_id: String,
        offered: u64,
        live: u64,
    },
    #[error("unknown writer lease {0}")]
    UnknownChild(String),
    #[error("writer {0} already holds a lease")]
    AlreadyAdmitted(String),
}

/// In-memory active writer leases keyed by child id.
#[derive(Debug, Clone, Default)]
pub struct WriterLeaseTable {
    active: HashMap<String, WriterHandoffFence>,
}

impl WriterLeaseTable {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn active_count(&self) -> usize {
        self.active.len()
    }

    pub fn get(&self, child_id: &str) -> Option<&WriterHandoffFence> {
        self.active.get(child_id)
    }

    /// Admit a writer fence when generation is live and write set is disjoint.
    pub fn admit(&mut self, fence: WriterHandoffFence) -> Result<(), WriteSetError> {
        if fence.child_id.trim().is_empty() {
            return Err(WriteSetError::EmptyChildId);
        }
        if let Some(existing) = self.active.get(&fence.child_id) {
            if fence.generation < existing.generation {
                return Err(WriteSetError::Stale {
                    child_id: fence.child_id,
                    offered: fence.generation,
                    live: existing.generation,
                });
            }
            if fence.generation == existing.generation {
                return Err(WriteSetError::AlreadyAdmitted(fence.child_id));
            }
            // Higher generation = handoff replace; still check peers.
            for (id, peer) in &self.active {
                if id == &fence.child_id {
                    continue;
                }
                if fence.write_set.overlaps(&peer.write_set) {
                    return Err(WriteSetError::Conflict { holder: id.clone() });
                }
            }
            self.active.insert(fence.child_id.clone(), fence);
            return Ok(());
        }

        for (id, peer) in &self.active {
            if fence.write_set.overlaps(&peer.write_set) {
                return Err(WriteSetError::Conflict { holder: id.clone() });
            }
        }
        self.active.insert(fence.child_id.clone(), fence);
        Ok(())
    }

    /// Fail-closed check before a late commit / result handoff.
    pub fn assert_live(&self, child_id: &str, generation: u64) -> Result<(), WriteSetError> {
        let Some(live) = self.active.get(child_id) else {
            return Err(WriteSetError::UnknownChild(child_id.to_string()));
        };
        if generation != live.generation {
            return Err(WriteSetError::Stale {
                child_id: child_id.to_string(),
                offered: generation,
                live: live.generation,
            });
        }
        Ok(())
    }

    /// Bump generation and keep write set — invalidates stale handoffs.
    pub fn bump(&mut self, child_id: &str) -> Result<&WriterHandoffFence, WriteSetError> {
        let entry = self
            .active
            .get_mut(child_id)
            .ok_or_else(|| WriteSetError::UnknownChild(child_id.to_string()))?;
        entry.generation = entry.generation.saturating_add(1);
        Ok(entry)
    }

    pub fn release(&mut self, child_id: &str) -> Result<WriterHandoffFence, WriteSetError> {
        self.active
            .remove(child_id)
            .ok_or_else(|| WriteSetError::UnknownChild(child_id.to_string()))
    }
}

fn normalize_rel(raw: &str) -> Result<String, WriteSetError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(WriteSetError::EmptyPath);
    }
    let path = Path::new(trimmed);
    if path.is_absolute() {
        return Err(WriteSetError::AbsolutePath(trimmed.to_string()));
    }
    let mut parts: Vec<String> = Vec::new();
    for comp in path.components() {
        match comp {
            Component::CurDir => {}
            Component::Normal(s) => parts.push(s.to_string_lossy().into_owned()),
            Component::ParentDir => {
                if parts.pop().is_none() {
                    return Err(WriteSetError::PathEscape(trimmed.to_string()));
                }
            }
            Component::RootDir | Component::Prefix(_) => {
                return Err(WriteSetError::AbsolutePath(trimmed.to_string()));
            }
        }
    }
    if parts.is_empty() {
        return Err(WriteSetError::EmptyPath);
    }
    Ok(parts.join("/"))
}

fn path_covers(owner: &str, candidate: &str) -> bool {
    if owner == candidate {
        return true;
    }
    candidate.starts_with(owner) && candidate.as_bytes().get(owner.len()) == Some(&b'/')
}

fn glob_matches(pattern: &str, path: &str) -> bool {
    let pat: Vec<&str> = pattern.split('/').collect();
    let segs: Vec<&str> = path.split('/').collect();
    match_glob(&pat, &segs)
}

fn match_glob(pat: &[&str], segs: &[&str]) -> bool {
    match (pat.first(), segs.first()) {
        (None, None) => true,
        (Some(&"**"), rest_pat) => {
            let rest = &pat[1..];
            if rest.is_empty() {
                return true;
            }
            if match_glob(rest, segs) {
                return true;
            }
            if segs.is_empty() {
                return false;
            }
            match_glob(pat, &segs[1..])
        }
        (Some(p), Some(s)) => {
            if *p == "*" || *p == *s {
                match_glob(&pat[1..], &segs[1..])
            } else {
                false
            }
        }
        (Some(_), None) | (None, Some(_)) => false,
    }
}

fn globs_may_overlap(a: &str, b: &str) -> bool {
    // Conservative: identical, or either is a prefix pattern under the other.
    a == b
        || glob_matches(a, b)
        || glob_matches(b, a)
        || path_covers(a.trim_end_matches("/**"), b.trim_end_matches("/**"))
        || path_covers(b.trim_end_matches("/**"), a.trim_end_matches("/**"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(ps: &[&str]) -> DeclaredWriteSet {
        DeclaredWriteSet::from_paths(ps.iter().copied()).expect("paths")
    }

    #[test]
    fn disjoint_writers_admit() {
        let mut table = WriterLeaseTable::new();
        table
            .admit(WriterHandoffFence::new("a", 1, paths(&["src/a.rs"])).unwrap())
            .unwrap();
        table
            .admit(WriterHandoffFence::new("b", 1, paths(&["src/b.rs"])).unwrap())
            .unwrap();
        assert_eq!(table.active_count(), 2);
    }

    #[test]
    fn overlapping_writers_denied() {
        let mut table = WriterLeaseTable::new();
        table
            .admit(WriterHandoffFence::new("a", 1, paths(&["src/shared.rs"])).unwrap())
            .unwrap();
        let err = table
            .admit(WriterHandoffFence::new("b", 1, paths(&["src/shared.rs"])).unwrap())
            .unwrap_err();
        assert!(matches!(err, WriteSetError::Conflict { holder } if holder == "a"));
    }

    #[test]
    fn prefix_and_glob_conflict() {
        let dir = paths(&["src/mod"]);
        let file = paths(&["src/mod/foo.rs"]);
        assert!(dir.overlaps(&file));

        let mut glob_set = DeclaredWriteSet::new();
        glob_set.add_glob("src/**").unwrap();
        assert!(glob_set.overlaps(&paths(&["src/x.rs"])));
    }

    #[test]
    fn unknown_scope_conflicts_conservatively() {
        let unknown = DeclaredWriteSet::unknown();
        assert!(unknown.overlaps(&paths(&["a.rs"])));
        assert!(unknown.overlaps(&DeclaredWriteSet::unknown()));
    }

    #[test]
    fn stale_generation_denied_on_admit_and_assert() {
        let mut table = WriterLeaseTable::new();
        table
            .admit(WriterHandoffFence::new("w", 2, paths(&["a.rs"])).unwrap())
            .unwrap();
        let stale = table
            .admit(WriterHandoffFence::new("w", 1, paths(&["a.rs"])).unwrap())
            .unwrap_err();
        assert!(matches!(
            stale,
            WriteSetError::Stale {
                child_id,
                offered: 1,
                live: 2
            } if child_id == "w"
        ));

        table.bump("w").unwrap();
        let err = table.assert_live("w", 2).unwrap_err();
        assert!(matches!(
            err,
            WriteSetError::Stale {
                offered: 2,
                live: 3,
                ..
            }
        ));
        table.assert_live("w", 3).unwrap();
    }

    #[test]
    fn handoff_higher_generation_replaces() {
        let mut table = WriterLeaseTable::new();
        table
            .admit(WriterHandoffFence::new("w", 1, paths(&["a.rs"])).unwrap())
            .unwrap();
        table
            .admit(WriterHandoffFence::new("w", 2, paths(&["b.rs"])).unwrap())
            .unwrap();
        assert_eq!(table.get("w").unwrap().generation, 2);
        assert_eq!(table.get("w").unwrap().write_set, paths(&["b.rs"]));
    }

    #[test]
    fn reject_absolute_and_escape() {
        assert!(matches!(
            DeclaredWriteSet::from_paths(["/etc/passwd"]),
            Err(WriteSetError::AbsolutePath(_))
        ));
        assert!(matches!(
            DeclaredWriteSet::from_paths(["../outside"]),
            Err(WriteSetError::PathEscape(_))
        ));
    }
}
