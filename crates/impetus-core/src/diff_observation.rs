//! DiffObservation producers from text pairs and `git diff` output.
//!
//! Does not own Git IPC. Shells out to system `git` only when a repo path /
//! commit range is provided. Line diffs for approval previews use a small
//! LCS-based unified emitter (no extra crate).

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::observations::{DiffHunk, DiffObservation, DiffSource};

/// Soft ceiling on hunk body lines kept in each [`DiffHunk::preview`].
pub const MAX_HUNK_PREVIEW_LINES: usize = 80;

/// Soft ceiling on hunks kept in one observation.
pub const MAX_HUNKS: usize = 64;

/// Soft ceiling on unified preview string lines (approval `diff_preview`).
pub const MAX_UNIFIED_PREVIEW_LINES: usize = 50;

/// Build a [`DiffObservation`] comparing two text blobs for one file path.
pub fn from_texts(file: impl Into<PathBuf>, before: &str, after: &str) -> DiffObservation {
    let file = file.into();
    let before_lines: Vec<&str> = before.lines().collect();
    let after_lines: Vec<&str> = after.lines().collect();
    let edits = line_edits(&before_lines, &after_lines);
    let hunks = edits_to_hunks(&file, &before_lines, &after_lines, &edits);
    let (insertions, deletions) = count_insert_delete(&edits);
    let files_changed = usize::from(before != after);
    let summary = format!(
        "{files_changed} file changed, {insertions} insertions(+), {deletions} deletions(-)"
    );
    DiffObservation {
        source: DiffSource::Files {
            before: file.clone(),
            after: file,
        },
        files_changed,
        insertions,
        deletions,
        summary,
        hunks,
        artifact_ref: None,
    }
}

/// Parse unified diff text (e.g. `git diff` stdout) into a [`DiffObservation`].
pub fn from_unified(source: DiffSource, unified: &str) -> DiffObservation {
    let hunks = parse_unified_hunks(unified);
    let (insertions, deletions) = count_hunk_preview_stats(&hunks);
    let files_changed = hunks
        .iter()
        .map(|h| h.file.clone())
        .collect::<std::collections::BTreeSet<_>>()
        .len();
    let summary = format!(
        "{files_changed} files changed, {insertions} insertions(+), {deletions} deletions(-)"
    );
    DiffObservation {
        source,
        files_changed,
        insertions,
        deletions,
        summary,
        hunks,
        artifact_ref: None,
    }
}

/// Run `git -C <repo> diff --no-color <range_args…>` and build a [`DiffObservation`].
///
/// `range_args` are passed to git as-is (e.g. `["HEAD"]`, `["main...branch"]`,
/// `["HEAD", "--", "path"]`). Returns `None` when git is missing, repo invalid,
/// or diff is empty.
pub fn from_git_diff(repo: &Path, range_args: &[&str]) -> Option<DiffObservation> {
    let mut cmd = Command::new("git");
    cmd.args(["-C", &repo.to_string_lossy(), "diff", "--no-color"]);
    cmd.args(range_args);
    let output = cmd.output().ok()?;
    // git diff exits 0 (clean) or 1 (differences); both OK when stdout present.
    if !output.status.success() && output.status.code() != Some(1) {
        return None;
    }
    let unified = String::from_utf8_lossy(&output.stdout);
    if unified.trim().is_empty() {
        return None;
    }
    let commit_range = range_args.join(" ");
    Some(from_unified(
        DiffSource::Git {
            commit_range: Some(commit_range),
        },
        &unified,
    ))
}

/// Format a bounded unified-diff preview string from an observation.
pub fn unified_preview(obs: &DiffObservation, max_lines: usize) -> String {
    let mut lines: Vec<String> = Vec::new();
    for hunk in &obs.hunks {
        let path = hunk.file.display();
        lines.push(format!("--- a/{path}"));
        lines.push(format!("+++ b/{path}"));
        lines.push(format!(
            "@@ -{},{} +{},{} @@",
            hunk.old_start, hunk.old_lines, hunk.new_start, hunk.new_lines
        ));
        for preview_line in hunk.preview.lines() {
            lines.push(preview_line.to_string());
            if lines.len() >= max_lines {
                lines.push("... (truncated)".to_string());
                return lines.join("\n");
            }
        }
    }
    if lines.is_empty() {
        return obs.summary.clone();
    }
    lines.join("\n")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Edit {
    Keep(usize, usize), // before_idx, after_idx (same content)
    Delete(usize),
    Insert(usize),
}

fn line_edits(before: &[&str], after: &[&str]) -> Vec<Edit> {
    let n = before.len();
    let m = after.len();
    // ponytail: O(n*m) LCS table; fine for approval-sized text (ceiling: huge files
    // should spill to artifact / git --no-index later).
    let mut dp = vec![vec![0u32; m + 1]; n + 1];
    for i in 0..n {
        for j in 0..m {
            if before[i] == after[j] {
                dp[i + 1][j + 1] = dp[i][j] + 1;
            } else {
                dp[i + 1][j + 1] = dp[i + 1][j].max(dp[i][j + 1]);
            }
        }
    }
    let mut edits = Vec::new();
    let mut i = n;
    let mut j = m;
    while i > 0 || j > 0 {
        if i > 0 && j > 0 && before[i - 1] == after[j - 1] {
            edits.push(Edit::Keep(i - 1, j - 1));
            i -= 1;
            j -= 1;
        } else if j > 0 && (i == 0 || dp[i][j - 1] >= dp[i - 1][j]) {
            edits.push(Edit::Insert(j - 1));
            j -= 1;
        } else {
            edits.push(Edit::Delete(i - 1));
            i -= 1;
        }
    }
    edits.reverse();
    edits
}

fn count_insert_delete(edits: &[Edit]) -> (usize, usize) {
    let mut insertions = 0usize;
    let mut deletions = 0usize;
    for edit in edits {
        match edit {
            Edit::Insert(_) => insertions += 1,
            Edit::Delete(_) => deletions += 1,
            Edit::Keep(_, _) => {}
        }
    }
    (insertions, deletions)
}

fn edits_to_hunks(file: &Path, before: &[&str], after: &[&str], edits: &[Edit]) -> Vec<DiffHunk> {
    if edits.iter().all(|e| matches!(e, Edit::Keep(_, _))) {
        return Vec::new();
    }

    // Group into hunks with 3 lines of context.
    const CONTEXT: usize = 3;
    let mut change_flags = vec![false; edits.len()];
    for (idx, edit) in edits.iter().enumerate() {
        change_flags[idx] = !matches!(edit, Edit::Keep(_, _));
    }

    let mut hunks = Vec::new();
    let mut i = 0;
    while i < edits.len() {
        if !change_flags[i] {
            i += 1;
            continue;
        }
        let mut start = i.saturating_sub(CONTEXT);
        while start > 0 && change_flags[start - 1] {
            start -= 1;
        }
        let mut end = i + 1;
        while end < edits.len() {
            if change_flags[end] {
                end += 1;
                continue;
            }
            // peek ahead for next change within 2*CONTEXT
            let mut peek = end;
            let mut found = false;
            while peek < edits.len() && peek < end + CONTEXT * 2 {
                if change_flags[peek] {
                    found = true;
                    break;
                }
                peek += 1;
            }
            if found {
                end = peek + 1;
            } else {
                end = (end + CONTEXT).min(edits.len());
                break;
            }
        }

        let slice = &edits[start..end];
        let mut old_start = 1usize;
        let mut new_start = 1usize;
        let mut old_lines = 0usize;
        let mut new_lines = 0usize;
        let mut preview_lines = Vec::new();

        // Determine starting line numbers from first Keep/Delete/Insert.
        for edit in slice.iter().take(1) {
            match edit {
                Edit::Keep(bi, ai) => {
                    old_start = bi + 1;
                    new_start = ai + 1;
                }
                Edit::Delete(bi) => {
                    old_start = bi + 1;
                    new_start = after_line_for_delete(edits, *bi).unwrap_or(1);
                }
                Edit::Insert(ai) => {
                    new_start = ai + 1;
                    old_start = before_line_for_insert(edits, *ai).unwrap_or(1);
                }
            }
        }

        for edit in slice {
            match edit {
                Edit::Keep(bi, ai) => {
                    old_lines += 1;
                    new_lines += 1;
                    if preview_lines.len() < MAX_HUNK_PREVIEW_LINES {
                        preview_lines.push(format!(" {}", before.get(*bi).copied().unwrap_or("")));
                        let _ = ai;
                    }
                }
                Edit::Delete(bi) => {
                    old_lines += 1;
                    if preview_lines.len() < MAX_HUNK_PREVIEW_LINES {
                        preview_lines.push(format!("-{}", before.get(*bi).copied().unwrap_or("")));
                    }
                }
                Edit::Insert(ai) => {
                    new_lines += 1;
                    if preview_lines.len() < MAX_HUNK_PREVIEW_LINES {
                        preview_lines.push(format!("+{}", after.get(*ai).copied().unwrap_or("")));
                    }
                }
            }
        }

        hunks.push(DiffHunk {
            file: file.to_path_buf(),
            old_start,
            old_lines,
            new_start,
            new_lines,
            preview: preview_lines.join("\n"),
        });
        if hunks.len() >= MAX_HUNKS {
            break;
        }
        i = end;
    }
    hunks
}

fn after_line_for_delete(edits: &[Edit], before_idx: usize) -> Option<usize> {
    // Nearest Keep after this delete gives after index; else 1.
    for edit in edits {
        if let Edit::Keep(bi, ai) = edit
            && *bi >= before_idx
        {
            return Some(ai + 1);
        }
    }
    Some(1)
}

fn before_line_for_insert(edits: &[Edit], after_idx: usize) -> Option<usize> {
    for edit in edits {
        if let Edit::Keep(bi, ai) = edit
            && *ai >= after_idx
        {
            return Some(bi + 1);
        }
    }
    Some(1)
}

fn parse_unified_hunks(unified: &str) -> Vec<DiffHunk> {
    let mut hunks = Vec::new();
    let mut current_file = PathBuf::from("unknown");
    let mut old_start = 1usize;
    let mut old_lines = 0usize;
    let mut new_start = 1usize;
    let mut new_lines = 0usize;
    let mut preview: Vec<String> = Vec::new();
    let mut in_hunk = false;

    let flush = |hunks: &mut Vec<DiffHunk>,
                 file: &Path,
                 old_start: usize,
                 old_lines: usize,
                 new_start: usize,
                 new_lines: usize,
                 preview: &mut Vec<String>| {
        if preview.is_empty() && old_lines == 0 && new_lines == 0 {
            return;
        }
        hunks.push(DiffHunk {
            file: file.to_path_buf(),
            old_start,
            old_lines,
            new_start,
            new_lines,
            preview: preview.join("\n"),
        });
        preview.clear();
    };

    for line in unified.lines() {
        if let Some(rest) = line.strip_prefix("--- ") {
            if in_hunk {
                flush(
                    &mut hunks,
                    &current_file,
                    old_start,
                    old_lines,
                    new_start,
                    new_lines,
                    &mut preview,
                );
                in_hunk = false;
            }
            current_file = PathBuf::from(strip_ab_prefix(rest));
        } else if let Some(rest) = line.strip_prefix("+++ ") {
            current_file = PathBuf::from(strip_ab_prefix(rest));
        } else if let Some(rest) = line.strip_prefix("@@") {
            if in_hunk {
                flush(
                    &mut hunks,
                    &current_file,
                    old_start,
                    old_lines,
                    new_start,
                    new_lines,
                    &mut preview,
                );
            }
            if let Some((os, ol, ns, nl)) = parse_hunk_header(rest) {
                old_start = os;
                old_lines = ol;
                new_start = ns;
                new_lines = nl;
            }
            in_hunk = true;
            preview.clear();
        } else if in_hunk {
            if line.starts_with('+') || line.starts_with('-') || line.starts_with(' ') {
                if preview.len() < MAX_HUNK_PREVIEW_LINES {
                    preview.push(line.to_string());
                }
            } else if line.starts_with('\\') {
                // "\ No newline at end of file"
                continue;
            } else if line.starts_with("diff ") {
                flush(
                    &mut hunks,
                    &current_file,
                    old_start,
                    old_lines,
                    new_start,
                    new_lines,
                    &mut preview,
                );
                in_hunk = false;
            }
        }
        if hunks.len() >= MAX_HUNKS {
            break;
        }
    }
    if in_hunk {
        flush(
            &mut hunks,
            &current_file,
            old_start,
            old_lines,
            new_start,
            new_lines,
            &mut preview,
        );
    }
    hunks
}

fn strip_ab_prefix(path: &str) -> &str {
    let path = path.trim();
    // "a/foo\t" or "b/foo" or "/dev/null"
    let path = path.split('\t').next().unwrap_or(path);
    path.strip_prefix("a/")
        .or_else(|| path.strip_prefix("b/"))
        .unwrap_or(path)
}

fn parse_hunk_header(rest: &str) -> Option<(usize, usize, usize, usize)> {
    // " -12,3 +14,4 @@" or " -1 +1 @@"
    let rest = rest.trim_start();
    let mut parts = rest.split_whitespace();
    let old = parts.next()?.strip_prefix('-')?;
    let new = parts.next()?.strip_prefix('+')?;
    let (os, ol) = parse_range(old)?;
    let (ns, nl) = parse_range(new)?;
    Some((os, ol, ns, nl))
}

fn parse_range(s: &str) -> Option<(usize, usize)> {
    if let Some((start, count)) = s.split_once(',') {
        Some((start.parse().ok()?, count.parse().ok()?))
    } else {
        Some((s.parse().ok()?, 1))
    }
}

fn count_hunk_preview_stats(hunks: &[DiffHunk]) -> (usize, usize) {
    let mut insertions = 0usize;
    let mut deletions = 0usize;
    for hunk in hunks {
        for line in hunk.preview.lines() {
            if line.starts_with('+') && !line.starts_with("+++") {
                insertions += 1;
            } else if line.starts_with('-') && !line.starts_with("---") {
                deletions += 1;
            }
        }
    }
    (insertions, deletions)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::process::Command;

    #[test]
    fn from_texts_produces_insert_and_delete_hunks() {
        let obs = from_texts(
            "src/a.rs",
            "one\ntwo\nthree\n",
            "one\ntwo changed\nthree\nfour\n",
        );
        assert_eq!(obs.files_changed, 1);
        assert!(obs.insertions >= 1);
        assert!(obs.deletions >= 1);
        assert!(!obs.hunks.is_empty());
        let preview = &obs.hunks[0].preview;
        assert!(preview.contains("-two") || preview.contains("-two\n") || preview.contains("two"));
        assert!(
            preview.contains('+'),
            "expected addition markers in {preview}"
        );
        let unified = unified_preview(&obs, MAX_UNIFIED_PREVIEW_LINES);
        assert!(unified.contains("@@"));
        assert!(unified.contains("a.rs"));
    }

    #[test]
    fn from_texts_identical_is_empty_hunks() {
        let obs = from_texts("same.txt", "a\nb\n", "a\nb\n");
        assert_eq!(obs.files_changed, 0);
        assert!(obs.hunks.is_empty());
        assert_eq!(obs.insertions, 0);
        assert_eq!(obs.deletions, 0);
    }

    #[test]
    fn from_unified_parses_git_style_hunk() {
        let unified = "\
diff --git a/foo.txt b/foo.txt
--- a/foo.txt
+++ b/foo.txt
@@ -1,2 +1,3 @@
 line1
-old
+new
+extra
";
        let obs = from_unified(
            DiffSource::Git {
                commit_range: Some("HEAD".into()),
            },
            unified,
        );
        assert_eq!(obs.files_changed, 1);
        assert_eq!(obs.hunks.len(), 1);
        assert_eq!(obs.hunks[0].file, PathBuf::from("foo.txt"));
        assert!(obs.insertions >= 2);
        assert!(obs.deletions >= 1);
        let json = serde_json::to_string(&obs).expect("serialize");
        let back: DiffObservation = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.hunks[0].preview, obs.hunks[0].preview);
    }

    #[test]
    fn from_git_diff_temp_repo() {
        let root = tempfile::tempdir().expect("temp");
        let repo = root.path();
        run(repo, &["git", "init"]);
        run(repo, &["git", "config", "user.email", "test@example.com"]);
        run(repo, &["git", "config", "user.name", "Test"]);
        fs::write(repo.join("tracked.txt"), "alpha\n").expect("write");
        run(repo, &["git", "add", "tracked.txt"]);
        run(repo, &["git", "commit", "-m", "init"]);
        fs::write(repo.join("tracked.txt"), "alpha\nbeta\n").expect("modify");

        let obs = from_git_diff(repo, &["HEAD"]).expect("git diff observation");
        assert_eq!(obs.files_changed, 1);
        assert!(obs.insertions >= 1);
        assert!(!obs.hunks.is_empty());
        assert!(
            obs.hunks
                .iter()
                .any(|h| h.preview.contains('+') && h.preview.contains("beta"))
        );
    }

    fn run(cwd: &Path, args: &[&str]) {
        let status = Command::new(args[0])
            .args(&args[1..])
            .current_dir(cwd)
            .status()
            .unwrap_or_else(|_| panic!("spawn {:?}", args));
        assert!(status.success(), "{args:?} failed");
    }
}
