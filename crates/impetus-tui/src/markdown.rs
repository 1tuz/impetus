use std::sync::OnceLock;

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use syntect::easy::HighlightLines;
use syntect::highlighting::{FontStyle, Style as SyntectStyle, ThemeSet};
use syntect::parsing::SyntaxSet;
use unicode_width::UnicodeWidthChar;

use crate::theme::Theme;

static SYNTAXES: OnceLock<SyntaxSet> = OnceLock::new();
static THEMES: OnceLock<ThemeSet> = OnceLock::new();

pub fn render_markdown(input: &str, width: usize, theme: Theme) -> Vec<Line<'static>> {
    let width = width.max(8);
    let mut output = Vec::new();
    let mut code_language: Option<String> = None;
    let mut code = String::new();

    for raw_line in input.lines() {
        if let Some(language) = code_language.as_ref() {
            if raw_line.trim_start().starts_with("```") {
                output.extend(render_code(&code, language, width, theme));
                code.clear();
                code_language = None;
            } else {
                code.push_str(raw_line);
                code.push('\n');
            }
            continue;
        }

        if let Some(language) = raw_line.trim_start().strip_prefix("```") {
            code_language = Some(language.trim().to_owned());
            continue;
        }

        if raw_line.trim().is_empty() {
            output.push(Line::from(""));
            continue;
        }

        let (prefix, content, base_style) = classify_line(raw_line, theme);
        let mut spans = Vec::new();
        if !prefix.is_empty() {
            spans.push(Span::styled(prefix, base_style));
        }
        spans.extend(inline_spans(content, base_style, theme));
        output.extend(wrap_spans(spans, width));
    }

    if let Some(language) = code_language {
        output.extend(render_code(&code, &language, width, theme));
    }

    if output.is_empty() {
        output.push(Line::from(""));
    }
    output
}

pub fn render_plain_wrapped(input: &str, width: usize, style: Style) -> Vec<Line<'static>> {
    let mut output = Vec::new();
    for line in input.lines() {
        output.extend(wrap_spans(
            vec![Span::styled(line.to_owned(), style)],
            width.max(1),
        ));
    }
    if output.is_empty() {
        output.push(Line::from(""));
    }
    output
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

fn inline_spans(input: &str, base: Style, theme: Theme) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let mut rest = input;
    while !rest.is_empty() {
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

fn render_code(code: &str, language: &str, width: usize, theme: Theme) -> Vec<Line<'static>> {
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

    if let Some(syntax_theme) = syntax_theme {
        let mut highlighter = HighlightLines::new(syntax, syntax_theme);
        for raw in code.lines() {
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
            output.extend(wrap_spans(with_gutter, width));
        }
    } else {
        for raw in code.lines() {
            output.extend(wrap_spans(
                vec![
                    Span::styled("│ ", Style::default().fg(theme.border)),
                    Span::styled(raw.to_owned(), Style::default().fg(theme.text)),
                ],
                width,
            ));
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

    #[test]
    fn markdown_renders_fenced_code_and_headings() {
        let lines = render_markdown(
            "# Title\n\n```rust\nfn main() {}\n```",
            40,
            Theme::default(),
        );
        assert!(lines.len() >= 5);
    }

    #[test]
    fn wrapping_keeps_line_width_bounded() {
        let lines = render_plain_wrapped("abcdefghij", 4, Style::default().fg(Color::White));
        assert_eq!(lines.len(), 3);
    }
}
