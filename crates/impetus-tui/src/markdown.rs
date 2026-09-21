use std::sync::OnceLock;

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use syntect::easy::HighlightLines;
use syntect::highlighting::{FontStyle, Style as SyntectStyle, ThemeSet};
use syntect::parsing::SyntaxSet;
use unicode_width::UnicodeWidthChar;

use crate::theme::Theme;

/// Hard cap on markdown source accepted by the renderer (bytes).
pub const MAX_MARKDOWN_INPUT_BYTES: usize = 32 * 1024;
/// Hard cap on rendered output lines after wrap.
pub const MAX_MARKDOWN_OUTPUT_LINES: usize = 1_024;
/// Hard cap on Span nodes allocated during one render pass.
pub const MAX_MARKDOWN_SPANS: usize = 4_096;
/// Hard cap on a single fenced code block before syntect highlight.
pub const MAX_CODE_BLOCK_BYTES: usize = 8 * 1024;

const TRUNCATION_NOTICE: &str =
    "… markdown truncated in TUI (bound); use artifact/raw view for full content";

static SYNTAXES: OnceLock<SyntaxSet> = OnceLock::new();
static THEMES: OnceLock<ThemeSet> = OnceLock::new();

#[derive(Debug, Clone, Copy)]
struct RenderBudget {
    max_lines: usize,
    max_spans: usize,
    lines: usize,
    spans: usize,
    truncated: bool,
}

impl RenderBudget {
    fn new() -> Self {
        Self {
            max_lines: MAX_MARKDOWN_OUTPUT_LINES,
            max_spans: MAX_MARKDOWN_SPANS,
            lines: 0,
            spans: 0,
            truncated: false,
        }
    }

    fn remaining_lines(&self) -> usize {
        self.max_lines.saturating_sub(self.lines)
    }

    fn can_add_spans(&self, count: usize) -> bool {
        !self.truncated && self.spans + count <= self.max_spans
    }

    fn record_line(&mut self, span_count: usize) -> bool {
        if self.truncated {
            return false;
        }
        if self.lines >= self.max_lines || self.spans + span_count > self.max_spans {
            self.truncated = true;
            return false;
        }
        self.lines += 1;
        self.spans += span_count;
        true
    }

    fn mark_truncated(&mut self) {
        self.truncated = true;
    }
}

pub fn render_markdown(input: &str, width: usize, theme: Theme) -> Vec<Line<'static>> {
    let width = width.max(8);
    let (source, input_truncated) = truncate_input(input, MAX_MARKDOWN_INPUT_BYTES);
    let mut budget = RenderBudget::new();
    let mut truncated = input_truncated;

    let mut output = Vec::new();
    let mut code_language: Option<String> = None;
    let mut code = String::new();

    for raw_line in source.lines() {
        if budget.remaining_lines() == 0 {
            budget.mark_truncated();
            truncated = true;
            break;
        }

        if let Some(language) = code_language.as_ref() {
            if raw_line.trim_start().starts_with("```") {
                let code_source = truncate_code(&code);
                truncated |= code_source.truncated;
                let code_lines = render_code(code_source.text, language, width, theme, &mut budget);
                if !push_lines(&mut output, &mut budget, code_lines) {
                    truncated = true;
                    break;
                }
                truncated |= budget.truncated;
                code.clear();
                code_language = None;
            } else {
                code.push_str(raw_line);
                code.push('\n');
                if code.len() > MAX_CODE_BLOCK_BYTES * 2 {
                    // Stop accumulating adversarial open fences early.
                    truncated = true;
                    let code_source = truncate_code(&code);
                    let code_lines =
                        render_code(code_source.text, language, width, theme, &mut budget);
                    if !push_lines(&mut output, &mut budget, code_lines) {
                        truncated = true;
                        break;
                    }
                    truncated |= budget.truncated;
                    code.clear();
                    code_language = None;
                }
            }
            continue;
        }

        if let Some(language) = raw_line.trim_start().strip_prefix("```") {
            code_language = Some(language.trim().to_owned());
            continue;
        }

        if raw_line.trim().is_empty() {
            if !push_lines(&mut output, &mut budget, vec![Line::from("")]) {
                truncated = true;
                break;
            }
            continue;
        }

        let (prefix, content, base_style) = classify_line(raw_line, theme);
        let mut spans = Vec::new();
        if !prefix.is_empty() {
            spans.push(Span::styled(prefix, base_style));
        }
        spans.extend(inline_spans(content, base_style, theme, &mut budget));
        truncated |= budget.truncated;
        if !push_lines(&mut output, &mut budget, wrap_spans(spans, width)) {
            truncated = true;
            break;
        }
    }

    if let Some(language) = code_language {
        let code_source = truncate_code(&code);
        truncated |= code_source.truncated;
        let code_lines = render_code(code_source.text, &language, width, theme, &mut budget);
        if !push_lines(&mut output, &mut budget, code_lines) {
            truncated = true;
        }
        truncated |= budget.truncated;
    }

    if truncated || budget.truncated {
        push_truncation_notice(&mut output, theme);
    }

    if output.is_empty() {
        output.push(Line::from(""));
    }
    output
}

pub fn render_plain_wrapped(input: &str, width: usize, style: Style) -> Vec<Line<'static>> {
    let mut output = Vec::new();
    let mut budget = RenderBudget::new();
    let (source, input_truncated) = truncate_input(input, MAX_MARKDOWN_INPUT_BYTES);
    let mut truncated = input_truncated;
    for line in source.lines() {
        if !push_lines(
            &mut output,
            &mut budget,
            wrap_spans(vec![Span::styled(line.to_owned(), style)], width.max(1)),
        ) {
            truncated = true;
            break;
        }
    }
    if truncated || budget.truncated {
        push_truncation_notice(&mut output, Theme::default());
    }
    if output.is_empty() {
        output.push(Line::from(""));
    }
    output
}

fn truncate_input(input: &str, max_bytes: usize) -> (&str, bool) {
    if input.len() <= max_bytes {
        return (input, false);
    }
    let mut end = max_bytes;
    while end > 0 && !input.is_char_boundary(end) {
        end -= 1;
    }
    (&input[..end], true)
}

struct TruncatedCode<'a> {
    text: &'a str,
    truncated: bool,
}

fn truncate_code(code: &str) -> TruncatedCode<'_> {
    let (text, truncated) = truncate_input(code, MAX_CODE_BLOCK_BYTES);
    TruncatedCode { text, truncated }
}

fn push_lines(
    output: &mut Vec<Line<'static>>,
    budget: &mut RenderBudget,
    lines: Vec<Line<'static>>,
) -> bool {
    for line in lines {
        let span_count = line.spans.len().max(1);
        if !budget.record_line(span_count) {
            return false;
        }
        output.push(line);
        if budget.remaining_lines() == 0 {
            budget.mark_truncated();
            return false;
        }
    }
    true
}

fn push_truncation_notice(output: &mut Vec<Line<'static>>, theme: Theme) {
    // Force-admit notice even when line budget is exhausted.
    if output.last().is_some_and(|line| {
        line.spans
            .iter()
            .any(|span| span.content.contains("truncated"))
    }) {
        return;
    }
    output.push(Line::from(Span::styled(
        TRUNCATION_NOTICE.to_owned(),
        Style::default()
            .fg(theme.muted)
            .add_modifier(Modifier::ITALIC),
    )));
}

fn classify_line(line: &str, theme: Theme) -> (String, &str, Style) {
    let trimmed = line.trim_start();
    if let Some(content) = trimmed.strip_prefix("### ") {
        return (
            "▸ ".to_owned(),
            content,
            Style::default().fg(theme.cyan).add_modifier(Modifier::BOLD),
        );
    }
    if let Some(content) = trimmed.strip_prefix("## ") {
        return (
            "◆ ".to_owned(),
            content,
            Style::default().fg(theme.blue).add_modifier(Modifier::BOLD),
        );
    }
    if let Some(content) = trimmed.strip_prefix("# ") {
        return (
            "■ ".to_owned(),
            content,
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        );
    }
    if let Some(content) = trimmed
        .strip_prefix("- ")
        .or_else(|| trimmed.strip_prefix("* "))
    {
        return ("  • ".to_owned(), content, Style::default().fg(theme.text));
    }
    if let Some(content) = numbered_item(trimmed) {
        let prefix_len = trimmed.len() - content.len();
        return (
            format!("  {}", &trimmed[..prefix_len]),
            content,
            Style::default().fg(theme.text),
        );
    }
    if let Some(content) = trimmed.strip_prefix("> ") {
        return (
            "  │ ".to_owned(),
            content,
            Style::default()
                .fg(theme.muted)
                .add_modifier(Modifier::ITALIC),
        );
    }
    (String::new(), line, Style::default().fg(theme.text))
}

fn numbered_item(line: &str) -> Option<&str> {
    let dot = line.find(". ")?;
    (dot > 0 && line[..dot].chars().all(|ch| ch.is_ascii_digit())).then_some(&line[dot + 2..])
}

fn inline_spans(
    input: &str,
    base: Style,
    theme: Theme,
    budget: &mut RenderBudget,
) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let mut rest = input;
    while !rest.is_empty() {
        if !budget.can_add_spans(spans.len() + 1) {
            budget.mark_truncated();
            if !rest.is_empty() {
                spans.push(Span::styled(rest.to_owned(), base));
            }
            break;
        }
        let markers = ["`", "**", "["];
        let next = markers
            .iter()
            .filter_map(|marker| rest.find(marker).map(|index| (index, *marker)))
            .min_by_key(|(index, _)| *index);
        let Some((index, marker)) = next else {
            spans.push(Span::styled(rest.to_owned(), base));
            break;
        };
        if index > 0 {
            spans.push(Span::styled(rest[..index].to_owned(), base));
            rest = &rest[index..];
        }
        match marker {
            "`" => {
                if let Some(end) = rest[1..].find('`') {
                    let end = end + 1;
                    spans.push(Span::styled(
                        rest[1..end].to_owned(),
                        Style::default().fg(theme.yellow).bg(theme.surface_alt),
                    ));
                    rest = &rest[end + 1..];
                } else {
                    spans.push(Span::styled("`".to_owned(), base));
                    rest = &rest[1..];
                }
            }
            "**" => {
                if let Some(end) = rest[2..].find("**") {
                    let end = end + 2;
                    spans.push(Span::styled(
                        rest[2..end].to_owned(),
                        base.add_modifier(Modifier::BOLD),
                    ));
                    rest = &rest[end + 2..];
                } else {
                    spans.push(Span::styled("**".to_owned(), base));
                    rest = &rest[2..];
                }
            }
            "[" => {
                if let Some(close_text) = rest.find("](")
                    && let Some(close_url) = rest[close_text + 2..].find(')')
                {
                    let url_end = close_text + 2 + close_url;
                    let label = &rest[1..close_text];
                    let url = &rest[close_text + 2..url_end];
                    spans.push(Span::styled(
                        label.to_owned(),
                        Style::default()
                            .fg(theme.blue)
                            .add_modifier(Modifier::UNDERLINED),
                    ));
                    spans.push(Span::styled(
                        format!(" <{url}>"),
                        Style::default().fg(theme.muted),
                    ));
                    rest = &rest[url_end + 1..];
                } else {
                    spans.push(Span::styled("[".to_owned(), base));
                    rest = &rest[1..];
                }
            }
            _ => {
                spans.push(Span::styled(marker.to_owned(), base));
                rest = &rest[marker.len()..];
            }
        }
    }
    spans
}

fn render_code(
    code: &str,
    language: &str,
    width: usize,
    theme: Theme,
    budget: &mut RenderBudget,
) -> Vec<Line<'static>> {
    let syntaxes = SYNTAXES.get_or_init(SyntaxSet::load_defaults_newlines);
    let themes = THEMES.get_or_init(ThemeSet::load_defaults);
    let syntax = syntaxes
        .find_syntax_by_token(language)
        .unwrap_or_else(|| syntaxes.find_syntax_plain_text());
    let syntax_theme = themes
        .themes
        .get("base16-ocean.dark")
        .or_else(|| themes.themes.values().next());

    let mut output = vec![Line::from(vec![
        Span::styled("┌─ ", Style::default().fg(theme.border)),
        Span::styled(
            if language.is_empty() {
                "text"
            } else {
                language
            }
            .to_owned(),
            Style::default().fg(theme.muted),
        ),
    ])];

    let line_budget = budget.remaining_lines().saturating_sub(2); // header + footer
    let mut emitted = 0usize;

    if let Some(syntax_theme) = syntax_theme {
        let mut highlighter = HighlightLines::new(syntax, syntax_theme);
        for raw in code.lines() {
            if emitted >= line_budget {
                budget.mark_truncated();
                break;
            }
            let highlighted = highlighter.highlight_line(raw, syntaxes).ok();
            let spans = highlighted.map_or_else(
                || {
                    vec![Span::styled(
                        raw.to_owned(),
                        Style::default().fg(theme.text),
                    )]
                },
                |ranges| {
                    ranges
                        .into_iter()
                        .map(|(style, fragment)| {
                            Span::styled(fragment.to_owned(), syntect_style(style))
                        })
                        .collect::<Vec<_>>()
                },
            );
            let mut with_gutter = vec![Span::styled("│ ", Style::default().fg(theme.border))];
            with_gutter.extend(spans);
            let wrapped = wrap_spans(with_gutter, width);
            emitted += wrapped.len();
            output.extend(wrapped);
        }
    } else {
        for raw in code.lines() {
            if emitted >= line_budget {
                budget.mark_truncated();
                break;
            }
            let wrapped = wrap_spans(
                vec![
                    Span::styled("│ ", Style::default().fg(theme.border)),
                    Span::styled(raw.to_owned(), Style::default().fg(theme.text)),
                ],
                width,
            );
            emitted += wrapped.len();
            output.extend(wrapped);
        }
    }
    output.push(Line::from(Span::styled(
        "└─",
        Style::default().fg(theme.border),
    )));
    output
}

fn syntect_style(style: SyntectStyle) -> Style {
    let mut output = Style::default().fg(Color::Rgb(
        style.foreground.r,
        style.foreground.g,
        style.foreground.b,
    ));
    if style.font_style.contains(FontStyle::BOLD) {
        output = output.add_modifier(Modifier::BOLD);
    }
    if style.font_style.contains(FontStyle::ITALIC) {
        output = output.add_modifier(Modifier::ITALIC);
    }
    if style.font_style.contains(FontStyle::UNDERLINE) {
        output = output.add_modifier(Modifier::UNDERLINED);
    }
    output
}

pub fn wrap_spans(spans: Vec<Span<'static>>, width: usize) -> Vec<Line<'static>> {
    let width = width.max(1);
    let mut lines: Vec<Vec<Span<'static>>> = vec![Vec::new()];
    let mut current_width = 0usize;

    for span in spans {
        let style = span.style;
        let text = span.content.into_owned();
        let mut buffer = String::new();
        for ch in text.chars() {
            if ch == '\n' {
                flush_buffer(&mut lines, &mut buffer, style);
                lines.push(Vec::new());
                current_width = 0;
                continue;
            }
            let ch_width = ch.width().unwrap_or(1).max(1);
            if current_width > 0 && current_width + ch_width > width {
                flush_buffer(&mut lines, &mut buffer, style);
                lines.push(Vec::new());
                current_width = 0;
            }
            buffer.push(ch);
            current_width += ch_width;
        }
        flush_buffer(&mut lines, &mut buffer, style);
    }

    lines.into_iter().map(Line::from).collect()
}

fn flush_buffer(lines: &mut [Vec<Span<'static>>], buffer: &mut String, style: Style) {
    if buffer.is_empty() {
        return;
    }
    if let Some(line) = lines.last_mut() {
        line.push(Span::styled(std::mem::take(buffer), style));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line_text(line: &Line<'_>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>()
    }

    fn has_truncation_notice(lines: &[Line<'_>]) -> bool {
        lines
            .iter()
            .any(|line| line_text(line).contains("truncated"))
    }

    #[test]
    fn markdown_renders_fenced_code_and_headings() {
        let lines = render_markdown(
            "# Title\n\n```rust\nfn main() {}\n```",
            40,
            Theme::default(),
        );
        assert!(lines.len() >= 5);
        assert!(!has_truncation_notice(&lines));
    }

    #[test]
    fn wrapping_keeps_line_width_bounded() {
        let lines = render_plain_wrapped("abcdefghij", 4, Style::default().fg(Color::White));
        assert_eq!(lines.len(), 3);
    }

    #[test]
    fn adversarial_large_input_stays_within_line_bound() {
        let line = "- item with `code` and **bold** and [link](https://example.com/x)\n";
        let input = line.repeat(20_000);
        let lines = render_markdown(&input, 80, Theme::default());
        // +1 for forced truncation notice beyond the hard line budget.
        assert!(
            lines.len() <= MAX_MARKDOWN_OUTPUT_LINES + 1,
            "got {} lines",
            lines.len()
        );
        assert!(has_truncation_notice(&lines));
    }

    #[test]
    fn input_byte_cap_truncates_with_notice() {
        let input = "a".repeat(MAX_MARKDOWN_INPUT_BYTES + 4_096);
        let lines = render_markdown(&input, 40, Theme::default());
        assert!(has_truncation_notice(&lines));
        assert!(lines.len() <= MAX_MARKDOWN_OUTPUT_LINES + 1);
    }

    #[test]
    fn oversized_code_fence_is_capped() {
        let code = "x".repeat(MAX_CODE_BLOCK_BYTES + 2_048);
        let input = format!("```text\n{code}\n```");
        let lines = render_markdown(&input, 40, Theme::default());
        assert!(has_truncation_notice(&lines));
        assert!(lines.len() <= MAX_MARKDOWN_OUTPUT_LINES + 1);
    }

    #[test]
    fn links_render_as_label_and_url_text() {
        let lines = render_markdown("see [docs](https://example.com)", 60, Theme::default());
        let joined = lines.iter().map(line_text).collect::<String>();
        assert!(joined.contains("docs"));
        assert!(joined.contains("<https://example.com>"));
    }
}
