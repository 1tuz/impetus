//! Review pane helpers: changed-file list stats from daemon patch text.
//!
//! No local `git` — callers load `GitStatus` / `ListChangedFiles` / `GetDiff` /
//! `GetFileDiff` via `UiBackend` and feed results here for presentation.

use std::collections::HashMap;

use impetus_client::protocol::{GitChangeKind, GitChangedFile};

use crate::model::ReviewFileRow;

/// Compact status letter for the changed-files column.
pub fn change_kind_label(kind: GitChangeKind) -> &'static str {
    match kind {
        GitChangeKind::Modified => "M",
        GitChangeKind::Added => "A",
        GitChangeKind::Deleted => "D",
        GitChangeKind::Renamed => "R",
        GitChangeKind::Copied => "C",
        GitChangeKind::Unmerged => "U",
        GitChangeKind::Untracked => "?",
        GitChangeKind::Ignored => "!",
        GitChangeKind::Unknown => " ",
    }
}

/// Count insertions/deletions per path from a unified patch (daemon `GetDiff`).
pub fn file_line_stats(patch: &str) -> HashMap<String, (usize, usize)> {
    let mut map = HashMap::new();
    let mut current: Option<String> = None;
    let mut insertions = 0usize;
    let mut deletions = 0usize;

    let flush = |current: &mut Option<String>,
                 insertions: &mut usize,
                 deletions: &mut usize,
                 map: &mut HashMap<String, (usize, usize)>| {
        if let Some(path) = current.take() {
            let entry = map.entry(path).or_insert((0, 0));
            entry.0 = entry.0.saturating_add(*insertions);
            entry.1 = entry.1.saturating_add(*deletions);
            *insertions = 0;
            *deletions = 0;
        }
    };

    for line in patch.lines() {
        if let Some(path) = path_from_diff_header(line) {
            flush(&mut current, &mut insertions, &mut deletions, &mut map);
            current = Some(path);
            continue;
        }
        if current.is_none() {
            continue;
        }
        if line.starts_with('+') && !line.starts_with("+++") {
            insertions = insertions.saturating_add(1);
        } else if line.starts_with('-') && !line.starts_with("---") {
            deletions = deletions.saturating_add(1);
        }
    }
    flush(&mut current, &mut insertions, &mut deletions, &mut map);
    map
}

fn path_from_diff_header(line: &str) -> Option<String> {
    if let Some(rest) = line.strip_prefix("diff --git ") {
        // `diff --git a/foo b/foo`
        let mut parts = rest.split_whitespace();
        let _a = parts.next()?;
        let b = parts.next()?;
        return Some(strip_diff_prefix(b).to_owned());
    }
    if let Some(rest) = line.strip_prefix("+++ ") {
        let path = rest.split('\t').next().unwrap_or(rest).trim();
        if path == "/dev/null" {
            return None;
        }
        return Some(strip_diff_prefix(path).to_owned());
    }
    None
}

fn strip_diff_prefix(path: &str) -> &str {
    path.strip_prefix("b/")
        .or_else(|| path.strip_prefix("a/"))
        .unwrap_or(path)
}

/// Build list rows from daemon changed-files + optional whole-tree patch stats.
pub fn build_file_rows(files: &[GitChangedFile], patch: Option<&str>) -> Vec<ReviewFileRow> {
    let stats = patch.map(file_line_stats).unwrap_or_default();
    files
        .iter()
        .map(|file| {
            let path = file.path.display().to_string();
            let (insertions, deletions) = stats
                .get(&path)
                .copied()
                .map(|(i, d)| (Some(i), Some(d)))
                .unwrap_or((None, None));
            ReviewFileRow {
                path,
                kind_label: change_kind_label(file.kind).to_owned(),
                status_code: file.status_code.clone().unwrap_or_default(),
                insertions,
                deletions,
            }
        })
        .collect()
}

/// Line indices of `@@` hunk headers in a unified patch.
pub fn hunk_line_indices(patch: &str) -> Vec<usize> {
    patch
        .lines()
        .enumerate()
        .filter(|(_, line)| line.starts_with("@@"))
        .map(|(idx, _)| idx)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn stats_count_plus_minus_per_file() {
        let patch = "\
diff --git a/src/a.rs b/src/a.rs
--- a/src/a.rs
+++ b/src/a.rs
@@ -1,2 +1,3 @@
 line
-old
+new
+extra
diff --git a/src/b.rs b/src/b.rs
--- a/src/b.rs
+++ b/src/b.rs
@@ -1 +1 @@
-x
+y
";
        let stats = file_line_stats(patch);
        assert_eq!(stats.get("src/a.rs"), Some(&(2, 1)));
        assert_eq!(stats.get("src/b.rs"), Some(&(1, 1)));
    }

    #[test]
    fn build_rows_merge_status_and_stats() {
        let files = vec![GitChangedFile {
            path: PathBuf::from("src/a.rs"),
            kind: GitChangeKind::Modified,
            status_code: Some(" M".into()),
        }];
        let patch = "+++ b/src/a.rs\n+one\n-two\n";
        let rows = build_file_rows(&files, Some(patch));
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].kind_label, "M");
        assert_eq!(rows[0].status_code, " M");
        assert_eq!(rows[0].insertions, Some(1));
        assert_eq!(rows[0].deletions, Some(1));
    }

    #[test]
    fn hunk_indices_find_at_at() {
        let patch = "header\n@@ -1 +1 @@\n line\n@@ -2 +2 @@\n";
        assert_eq!(hunk_line_indices(patch), vec![1, 3]);
    }
}
