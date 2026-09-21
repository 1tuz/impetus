//! Bounded unified-diff viewport for harness-visible diffs.
//!
//! Renders approval `diff_preview` strings and DiffObservation-shaped JSON that
//! already reach the client via durable events / attachment DTOs. No
//! `impetus-core` import — wire shapes are mirrored locally for presentation.

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use serde::Deserialize;

use crate::markdown::wrap_spans;
use crate::theme::Theme;

/// Soft ceiling on painted diff lines so a huge preview cannot wedge the UI.
pub const MAX_DIFF_LINES: usize = 200;

/// Soft ceiling on input characters before parse/paint.
pub const MAX_DIFF_CHARS: usize = 32_000;

const TRUNCATION_MARKER: &str =
    "… diff truncated in TUI; open attachment/artifact for full content";

/// Local mirror of harness `DiffObservation` JSON (presentation only).
#[derive(Debug, Clone, Deserialize)]
struct DiffObservationDto {
    #[serde(default)]
    summary: Option<String>,
    #[serde(default)]
    files_changed: Option<usize>,
    #[serde(default)]
    insertions: Option<usize>,
    #[serde(default)]
    deletions: Option<usize>,
    #[serde(default)]
    hunks: Vec<DiffHunkDto>,
    #[serde(default)]
    artifact_ref: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct DiffHunkDto {
    file: String,
    #[serde(default)]
    preview: String,
    #[serde(default)]
    old_start: Option<usize>,
    #[serde(default)]
    old_lines: Option<usize>,
    #[serde(default)]
    new_start: Option<usize>,
    #[serde(default)]
    new_lines: Option<usize>,
}

/// True when `input` looks like unified diff text or DiffObservation JSON.
pub fn looks_like_diff(input: &str) -> bool {
    let trimmed = input.trim_start();
    if trimmed.is_empty() {
        return false;
    }
    if parse_observation(trimmed).is_some() {
        return true;
    }
    looks_like_unified_diff(trimmed)
}

fn looks_like_unified_diff(input: &str) -> bool {
    let mut has_minus = false;
    let mut has_plus = false;
    let mut has_hunk = false;
    for line in input.lines().take(40) {
        let line = line.trim_end();
        if line.starts_with("--- ") || line.starts_with("---\t") {
            has_minus = true;
        } else if line.starts_with("+++ ") || line.starts_with("+++\t") {
            has_plus = true;
        } else if line.starts_with("@@") {
            has_hunk = true;
        }
        if (has_minus && has_plus) || has_hunk {
            return true;
        }
    }
    false
}

fn parse_observation(input: &str) -> Option<DiffObservationDto> {
    let trimmed = input.trim();
    if !(trimmed.starts_with('{') && trimmed.contains("\"hunks\"")) {
        return None;
    }
    let obs: DiffObservationDto = serde_json::from_str(trimmed).ok()?;
    if obs.hunks.is_empty() && obs.summary.is_none() {
        return None;
    }
    Some(obs)
}

/// Paint a bounded colored unified-diff view (file headers + hunks).
pub fn render_diff_view(input: &str, width: usize, theme: Theme) -> Vec<Line<'static>> {
    let width = width.max(8);
    let bounded = bound_input(input);
    let mut lines = if let Some(obs) = parse_observation(bounded.trim()) {
        render_observation(&obs, width, theme)
    } else {
        render_unified(bounded, width, theme)
    };
    if lines.is_empty() {
        lines.push(Line::from(Span::styled(
            "(empty diff)",
            Style::default().fg(theme.muted),
        )));
    }
    truncate_lines(lines, MAX_DIFF_LINES, theme)
}

fn bound_input(input: &str) -> &str {
    if input.chars().count() <= MAX_DIFF_CHARS {
        return input;
    }
    // Keep a char-safe prefix; final line truncation adds the marker.
    let mut end = 0;
    for (idx, ch) in input.char_indices() {
        if idx >= MAX_DIFF_CHARS {
            break;
        }
        end = idx + ch.len_utf8();
    }
    &input[..end]
}

fn render_observation(obs: &DiffObservationDto, width: usize, theme: Theme) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let summary = obs
        .summary
        .clone()
        .unwrap_or_else(|| "DiffObservation".to_owned());
    lines.push(Line::from(Span::styled(
        summary,
        Style::default().fg(theme.text).add_modifier(Modifier::BOLD),
    )));

    let mut stats = Vec::new();
    if let Some(n) = obs.files_changed {
        stats.push(format!("{n} files"));
    }
    if let Some(n) = obs.insertions {
        stats.push(format!("+{n}"));
    }
    if let Some(n) = obs.deletions {
        stats.push(format!("-{n}"));
    }
    if !stats.is_empty() {
        lines.push(Line::from(Span::styled(
            stats.join(" · "),
            Style::default().fg(theme.muted),
        )));
    }
    if let Some(artifact) = &obs.artifact_ref {
        lines.push(Line::from(Span::styled(
            format!("artifact: {artifact}"),
            Style::default().fg(theme.cyan),
        )));
    }
    if !obs.hunks.is_empty() {
        lines.push(Line::from(""));
    }

    for hunk in &obs.hunks {
        lines.extend(file_header(&hunk.file, width, theme));
        if let (Some(os), Some(ol), Some(ns), Some(nl)) = (
            hunk.old_start,
            hunk.old_lines,
            hunk.new_start,
            hunk.new_lines,
        ) {
            lines.push(Line::from(Span::styled(
                format!("@@ -{os},{ol} +{ns},{nl} @@"),
                Style::default().fg(theme.magenta),
            )));
        }
        if hunk.preview.is_empty() {
            lines.push(Line::from(Span::styled(
                "(no hunk preview)",
                Style::default().fg(theme.muted),
            )));
        } else {
            lines.extend(render_unified_lines(&hunk.preview, width, theme, false));
        }
        lines.push(Line::from(""));
    }
    lines
}

fn render_unified(input: &str, width: usize, theme: Theme) -> Vec<Line<'static>> {
    render_unified_lines(input, width, theme, true)
}

fn render_unified_lines(
    input: &str,
    width: usize,
    theme: Theme,
    emit_file_headers: bool,
) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let mut current_file: Option<String> = None;

    for raw in input.lines() {
        if emit_file_headers {
            if let Some(path) = file_path_from_plus_header(raw) {
                if current_file.as_deref() != Some(path.as_str()) {
                    current_file = Some(path.clone());
                    lines.extend(file_header(&path, width, theme));
                }
            }
        }

        let style = line_style(raw, theme);
        let display = if raw.is_empty() { " " } else { raw };
        lines.extend(wrap_spans(
            vec![Span::styled(display.to_owned(), style)],
            width,
        ));
    }
    lines
}

fn file_header(path: &str, width: usize, theme: Theme) -> Vec<Line<'static>> {
    let title = format!("── {path} ");
    let pad = "─".repeat(width.saturating_sub(title.chars().count()).max(2));
    vec![Line::from(Span::styled(
        format!("{title}{pad}"),
        Style::default().fg(theme.cyan).add_modifier(Modifier::BOLD),
    ))]
}

fn file_path_from_plus_header(line: &str) -> Option<String> {
    let rest = line.strip_prefix("+++ ")?;
    let path = rest.split('\t').next().unwrap_or(rest).trim();
    if path == "/dev/null" {
        return None;
    }
    Some(strip_diff_prefix(path).to_owned())
}

fn strip_diff_prefix(path: &str) -> &str {
    path.strip_prefix("b/")
        .or_else(|| path.strip_prefix("a/"))
        .unwrap_or(path)
}

fn line_style(line: &str, theme: Theme) -> Style {
    if line.starts_with("+++") || line.starts_with("---") {
        Style::default().fg(theme.muted)
    } else if line.starts_with("@@") {
        Style::default().fg(theme.magenta)
    } else if line.starts_with('+') {
        Style::default().fg(theme.green)
    } else if line.starts_with('-') {
        Style::default().fg(theme.red)
    } else if line.starts_with('\\') {
        // "\ No newline at end of file"
        Style::default().fg(theme.muted)
    } else {
        Style::default().fg(theme.text)
    }
}

fn truncate_lines(
    mut lines: Vec<Line<'static>>,
    max_lines: usize,
    theme: Theme,
) -> Vec<Line<'static>> {
    if lines.len() <= max_lines {
        return lines;
    }
    let omitted = lines.len().saturating_sub(max_lines.saturating_sub(1));
    lines.truncate(max_lines.saturating_sub(1));
    lines.push(Line::from(Span::styled(
        format!("{TRUNCATION_MARKER} ({omitted} more lines)"),
        Style::default().fg(theme.yellow),
    )));
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_unified() -> &'static str {
        "--- a/src/main.rs\n+++ b/src/main.rs\n@@ -1,3 +1,4 @@\n fn main() {\n-    println!(\"old\");\n+    println!(\"new\");\n+    // note\n }\n"
    }

    #[test]
    fn detects_unified_and_observation() {
        assert!(looks_like_diff(sample_unified()));
        assert!(!looks_like_diff("plain status text"));
        let json = r#"{
            "summary": "1 file changed",
            "files_changed": 1,
            "insertions": 2,
            "deletions": 1,
            "hunks": [{"file": "src/main.rs", "preview": "+ok\n-old"}]
        }"#;
        assert!(looks_like_diff(json));
    }

    #[test]
    fn unified_render_includes_file_header_and_colors() {
        let lines = render_diff_view(sample_unified(), 48, Theme::default());
        let joined: String = lines
            .iter()
            .flat_map(|line| line.spans.iter().map(|s| s.content.as_ref()))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(joined.contains("src/main.rs"));
        assert!(joined.contains("@@ -1,3 +1,4 @@"));
        assert!(joined.contains("+    println!(\"new\");"));
    }

    #[test]
    fn observation_json_renders_hunk_file_header() {
        let json = r#"{
            "summary": "2 files changed, 3 insertions(+), 1 deletion(-)",
            "files_changed": 2,
            "insertions": 3,
            "deletions": 1,
            "hunks": [
                {
                    "file": "crates/impetus-tui/src/diff.rs",
                    "old_start": 1,
                    "old_lines": 2,
                    "new_start": 1,
                    "new_lines": 3,
                    "preview": " line\n-old\n+new\n+extra"
                }
            ],
            "artifact_ref": "artifact-label"
        }"#;
        let lines = render_diff_view(json, 60, Theme::default());
        let joined: String = lines
            .iter()
            .flat_map(|line| line.spans.iter().map(|s| s.content.as_ref()))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(joined.contains("2 files changed"));
        assert!(joined.contains("crates/impetus-tui/src/diff.rs"));
        assert!(joined.contains("artifact: artifact-label"));
        assert!(joined.contains("@@ -1,2 +1,3 @@"));
    }

    #[test]
    fn oversized_diff_is_bounded_with_marker() {
        let mut body = String::from("--- a/big.rs\n+++ b/big.rs\n@@ -1 +1 @@\n");
        for i in 0..(MAX_DIFF_LINES + 80) {
            body.push_str(&format!("+line {i}\n"));
        }
        let lines = render_diff_view(&body, 40, Theme::default());
        assert!(lines.len() <= MAX_DIFF_LINES);
        let joined: String = lines
            .iter()
            .flat_map(|line| line.spans.iter().map(|s| s.content.as_ref()))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(joined.contains("diff truncated in TUI"));
        assert!(joined.contains("more lines"));
    }

    #[test]
    fn char_ceiling_keeps_ui_responsive() {
        let mut body = String::from("--- a/x\n+++ b/x\n@@\n");
        body.push_str(&"+".repeat(MAX_DIFF_CHARS + 4_000));
        body.push('\n');
        let lines = render_diff_view(&body, 32, Theme::default());
        assert!(!lines.is_empty());
        assert!(lines.len() <= MAX_DIFF_LINES);
    }
}
