use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Direction, Layout, Margin, Rect},
    style::{Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, BorderType, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap},
};

use crate::{
    command,
    markdown::{render_markdown, render_plain_wrapped},
    model::{AppState, ExecutionMode, Focus, ItemKind, Overlay, RunState, short_id},
    theme::Theme,
};

pub fn render(frame: &mut Frame, app: &AppState, theme: Theme) {
    let area = frame.area();
    frame.render_widget(Block::default().style(theme.base()), area);

    if area.width < 52 || area.height < 14 {
        render_too_small(frame, area, theme);
        return;
    }

    let composer_width = area.width.saturating_sub(4);
    let composer_rows = app.composer.view(composer_width, 7).total_rows.clamp(1, 7) as u16;
    let composer_height = composer_rows + 2;
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(6),
            Constraint::Length(composer_height),
            Constraint::Length(1),
        ])
        .split(area);

    render_header(frame, rows[0], app, theme);
    render_main(frame, rows[1], app, theme);
    render_composer(frame, rows[2], app, theme);
    render_footer(frame, rows[3], app, theme);

    if matches!(app.overlay, Overlay::None) && app.composer.text().trim_start().starts_with('/') {
        render_inline_command_palette(frame, rows[2], app, theme);
    }
    render_overlay(frame, app, theme);
    render_toast(frame, app, theme);
}

fn render_header(frame: &mut Frame, area: Rect, app: &AppState, theme: Theme) {
    let state_color = match app.run_state {
        RunState::Idle => theme.green,
        RunState::Working => theme.accent,
        RunState::WaitingApproval => theme.yellow,
        RunState::Cancelling => theme.yellow,
        RunState::Failed | RunState::Unknown => theme.red,
    };
    let mut spans = vec![
        Span::styled(
            " IMPETUS ",
            Style::default()
                .fg(theme.background)
                .bg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled("  ", Style::default().bg(theme.surface)),
        Span::styled(
            app.active_session_label(),
            Style::default().fg(theme.text).bg(theme.surface),
        ),
        Span::styled("  ·  ", Style::default().fg(theme.border).bg(theme.surface)),
        Span::styled(
            app.mode.label(),
            Style::default()
                .fg(theme.cyan)
                .bg(theme.surface)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled("  ·  ", Style::default().fg(theme.border).bg(theme.surface)),
        Span::styled(
            format!("● {}", app.run_state.label()),
            Style::default()
                .fg(state_color)
                .bg(theme.surface)
                .add_modifier(Modifier::BOLD),
        ),
    ];
    if area.width > 100 {
        spans.extend([
            Span::styled("  ·  ", Style::default().fg(theme.border).bg(theme.surface)),
            Span::styled(
                format!("IPC v{}", app.connection.protocol_version),
                Style::default().fg(theme.muted).bg(theme.surface),
            ),
            Span::styled("  ", Style::default().bg(theme.surface)),
        ]);
    }
    frame.render_widget(
        Paragraph::new(Line::from(spans)).style(Style::default().bg(theme.surface)),
        area,
    );
}

fn render_main(frame: &mut Frame, area: Rect, app: &AppState, theme: Theme) {
    let show_sessions = app.show_sessions && area.width >= 100;
    let show_inspector = app.show_inspector && area.width >= 122;
    let constraints = match (show_sessions, show_inspector) {
        (true, true) => vec![
            Constraint::Length(25),
            Constraint::Min(48),
            Constraint::Length(34),
        ],
        (true, false) => vec![Constraint::Length(25), Constraint::Min(48)],
        (false, true) => vec![Constraint::Min(48), Constraint::Length(34)],
        (false, false) => vec![Constraint::Min(48)],
    };
    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints(constraints)
        .split(area);

    let mut index = 0;
    if show_sessions {
        render_sessions_panel(frame, columns[index], app, theme);
        index += 1;
    }
    render_timeline(frame, columns[index], app, theme);
    index += 1;
    if show_inspector {
        render_inspector(frame, columns[index], app, theme);
    }
}

fn render_sessions_panel(frame: &mut Frame, area: Rect, app: &AppState, theme: Theme) {
    let items = app
        .sessions
        .iter()
        .map(|session| {
            let active = app.active_session == Some(session.id);
            let marker = if active { "●" } else { "○" };
            let status = truncate(&session.status, 9);
            let lines = vec![
                Line::from(vec![
                    Span::styled(
                        format!("{marker} "),
                        Style::default().fg(if active { theme.green } else { theme.border }),
                    ),
                    Span::styled(
                        truncate(&session.label, 18),
                        Style::default()
                            .fg(if active { theme.text } else { theme.muted })
                            .add_modifier(if active {
                                Modifier::BOLD
                            } else {
                                Modifier::empty()
                            }),
                    ),
                ]),
                Line::from(vec![
                    Span::raw("  "),
                    Span::styled(short_id(session.id), Style::default().fg(theme.border)),
                    Span::styled(" · ", Style::default().fg(theme.border)),
                    Span::styled(status, Style::default().fg(theme.muted)),
                ]),
            ];
            ListItem::new(lines)
        })
        .collect::<Vec<_>>();
    let block = panel_block(" sessions · F2 ", false, theme);
    let list = List::new(items)
        .block(block)
        .highlight_style(theme.selected());
    let mut state = ListState::default();
    let selected = app
        .active_session
        .and_then(|active| app.sessions.iter().position(|session| session.id == active));
    state.select(selected);
    frame.render_stateful_widget(list, area, &mut state);
}

fn render_timeline(frame: &mut Frame, area: Rect, app: &AppState, theme: Theme) {
    let block = panel_block(
        format!(" event log · {} ", app.status_message),
        app.focus == Focus::Timeline,
        theme,
    );
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.width < 4 || inner.height == 0 {
        return;
    }

    let lines = build_timeline_lines(app, inner.width as usize, theme);
    let height = inner.height as usize;
    let offset = app.line_scroll_from_bottom.min(lines.len());
    let end = lines.len().saturating_sub(offset);
    let start = end.saturating_sub(height);
    let visible = lines[start..end].to_vec();
    frame.render_widget(Paragraph::new(Text::from(visible)), inner);

    if lines.len() > height {
        let indicator = if app.follow_tail {
            format!(" {} lines · following ", lines.len())
        } else {
            format!(" ↑{} · End follows ", offset)
        };
        let x = inner
            .right()
            .saturating_sub(indicator.chars().count() as u16);
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                indicator,
                Style::default().fg(theme.muted).bg(theme.surface),
            ))),
            Rect::new(x, area.y, inner.right().saturating_sub(x), 1),
        );
    }
}

fn build_timeline_lines(app: &AppState, width: usize, theme: Theme) -> Vec<Line<'static>> {
    let width = width.max(8);
    let mut lines = Vec::new();
    for (index, item) in app.timeline.iter().enumerate() {
        let selected = app.selected_item == Some(index);
        let accent = theme.item_color(item.kind);
        let marker = if selected { "▐" } else { "▌" };
        lines.push(Line::from(vec![
            Span::styled(marker.to_owned(), Style::default().fg(accent)),
            Span::styled(
                format!(" {} ", format_time(item.at_unix_ms)),
                Style::default().fg(theme.border),
            ),
            Span::styled(
                item.title.clone(),
                Style::default()
                    .fg(if selected { theme.text } else { accent })
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                if item.collapsed { "  [collapsed]" } else { "" },
                Style::default().fg(theme.muted),
            ),
        ]));
        if !item.collapsed && !item.body.is_empty() {
            let body_width = width.saturating_sub(3);
            let body_lines = match item.kind {
                ItemKind::Assistant | ItemKind::User | ItemKind::Plan => {
                    render_markdown(&item.body, body_width, theme)
                }
                _ => render_plain_wrapped(&item.body, body_width, Style::default().fg(theme.text)),
            };
            for line in body_lines {
                let mut spans = vec![
                    Span::styled("│ ", Style::default().fg(theme.border)),
                    Span::raw(" "),
                ];
                spans.extend(line.spans);
                lines.push(Line::from(spans));
            }
        }
        lines.push(Line::from(""));
    }
    if lines.is_empty() {
        lines.extend([
            Line::from(""),
            Line::from(Span::styled(
                "  No durable events yet.",
                Style::default().fg(theme.muted),
            )),
            Line::from(Span::styled(
                "  Type a task below; the daemon remains authoritative.",
                Style::default().fg(theme.border),
            )),
        ]);
    }
    lines
}

fn render_inspector(frame: &mut Frame, area: Rect, app: &AppState, theme: Theme) {
    let block = panel_block(" inspector · F3 ", app.focus == Focus::Inspector, theme);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let mut lines = Vec::new();
    if let Some(item) = app.selected_item.and_then(|index| app.timeline.get(index)) {
        lines.extend([
            Line::from(Span::styled(
                item.title.clone(),
                Style::default().fg(theme.text).add_modifier(Modifier::BOLD),
            )),
            Line::from(vec![
                Span::styled("event ", Style::default().fg(theme.muted)),
                Span::styled(item.sequence.to_string(), Style::default().fg(theme.cyan)),
                Span::styled(" · ", Style::default().fg(theme.border)),
                Span::styled(format!("{:?}", item.kind), Style::default().fg(theme.muted)),
            ]),
            Line::from(""),
        ]);
        let detail = if item.details.is_empty() {
            &item.body
        } else {
            &item.details
        };
        lines.extend(render_plain_wrapped(
            detail,
            inner.width as usize,
            Style::default().fg(theme.text),
        ));
    } else {
        lines.extend([
            Line::from(Span::styled(
                "Nothing selected",
                Style::default().fg(theme.muted),
            )),
            Line::from(""),
            Line::from(Span::styled(
                "Alt+↑/↓ selects an event.",
                Style::default().fg(theme.border),
            )),
        ]);
    }
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}

fn render_composer(frame: &mut Frame, area: Rect, app: &AppState, theme: Theme) {
    let focus = app.focus == Focus::Composer && matches!(app.overlay, Overlay::None);
    let title = format!(
        " task · {} · Enter send · Alt+Enter newline ",
        app.mode.label()
    );
    let block = panel_block(title, focus, theme);
    let inner = block.inner(area).inner(Margin {
        horizontal: 1,
        vertical: 0,
    });
    frame.render_widget(block, area);
    let view = app
        .composer
        .view(inner.width.saturating_sub(2), inner.height);
    let mut lines = Vec::new();
    if app.composer.is_empty() {
        lines.push(Line::from(vec![
            Span::styled("❯ ", Style::default().fg(theme.accent)),
            Span::styled(
                "Describe a task, paste code, or type / for commands",
                Style::default().fg(theme.muted),
            ),
        ]));
    } else {
        for (index, line) in view.lines.iter().enumerate() {
            lines.push(Line::from(vec![
                Span::styled(
                    if index == 0 { "❯ " } else { "  " },
                    Style::default().fg(theme.accent),
                ),
                Span::styled(line.clone(), Style::default().fg(theme.text)),
            ]));
        }
    }
    frame.render_widget(Paragraph::new(lines), inner);
    if focus {
        frame.set_cursor_position((
            inner.x + 2 + view.cursor_col,
            inner.y + view.cursor_row.min(inner.height.saturating_sub(1)),
        ));
    }
}

fn render_footer(frame: &mut Frame, area: Rect, app: &AppState, theme: Theme) {
    let left = " F1 help  F2 sessions  F3 details  F4 mode  Ctrl+P commands  Ctrl+Q quit";
    let warning = if app.budget.warning.is_some() {
        " · !"
    } else {
        ""
    };
    let right = format!(
        "{} tok · ctx {}% · {} turn{} ",
        compact_number(app.budget.tokens_used),
        app.budget.context_used_percent,
        app.budget.turns_used,
        warning,
    );
    let available = area.width as usize;
    let right_width = right.chars().count();
    let left = truncate(left, available.saturating_sub(right_width));
    let gap = available.saturating_sub(left.chars().count() + right_width);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(left, Style::default().fg(theme.muted).bg(theme.surface)),
            Span::styled(" ".repeat(gap), Style::default().bg(theme.surface)),
            Span::styled(right, Style::default().fg(theme.cyan).bg(theme.surface)),
        ])),
        area,
    );
}

fn render_inline_command_palette(
    frame: &mut Frame,
    composer_area: Rect,
    app: &AppState,
    theme: Theme,
) {
    let suggestions = command::suggestions(app.composer.text());
    if suggestions.is_empty() {
        return;
    }
    let requested_height = suggestions.len().min(7) as u16 + 2;
    let available_above = composer_area.y.saturating_sub(frame.area().y);
    let height = requested_height.min(available_above);
    if height < 3 {
        return;
    }
    let width = composer_area.width.min(76);
    let area = Rect::new(
        composer_area.x,
        composer_area.y.saturating_sub(height),
        width,
        height,
    );
    frame.render_widget(Clear, area);
    let items = suggestions
        .into_iter()
        .take(area.height.saturating_sub(2) as usize)
        .map(|spec| {
            ListItem::new(Line::from(vec![
                Span::styled(
                    format!("/{:<14}", spec.name),
                    Style::default().fg(theme.cyan),
                ),
                Span::styled(spec.description, Style::default().fg(theme.muted)),
            ]))
        })
        .collect::<Vec<_>>();
    frame.render_widget(
        List::new(items).block(panel_block(" commands ", true, theme)),
        area,
    );
}

fn render_overlay(frame: &mut Frame, app: &AppState, theme: Theme) {
    match &app.overlay {
        Overlay::None => {}
        Overlay::Help => render_help(frame, theme),
        Overlay::Sessions { selected, query } => {
            render_session_picker(frame, app, *selected, query, theme)
        }
        Overlay::Commands { selected, query } => {
            render_command_picker(frame, *selected, query, theme)
        }
        Overlay::Modes { selected } => render_mode_picker(frame, app, *selected, theme),
        Overlay::Approval { selected } => render_approval(frame, app, *selected, theme),
        Overlay::ApprovalDetail => render_approval_detail(frame, app, theme),
        Overlay::LargePaste => render_large_paste(frame, app, theme),
        Overlay::Diagnostics { text } => render_text_modal(
            frame,
            " diagnostics · redacted ",
            text,
            84,
            80,
            false,
            theme,
        ),
        Overlay::Message { title, body, error } => {
            render_text_modal(frame, title, body, 70, 55, *error, theme)
        }
    }
}

fn render_help(frame: &mut Frame, theme: Theme) {
    let body = [
        "NAVIGATION",
        "  PageUp/PageDown   scroll durable event history",
        "  End               resume following the newest events",
        "  Alt+Up/Down       select event for inspector",
        "  Enter             collapse/expand selected event when timeline focused",
        "",
        "COMPOSER",
        "  Enter             submit task",
        "  Alt+Enter/Ctrl+J  newline",
        "  Up/Down           prompt history",
        "  Ctrl+A/E          line start/end",
        "  Ctrl+W            delete previous word",
        "  Ctrl+P            command palette",
        "",
        "HARNESS",
        "  F2 sessions       attach durable session",
        "  F4 mode           Plan / Ask / Auto-Safe",
        "  Ctrl+C            cancel run or dismiss current input",
        "  Y / N             approve once / reject exact pending action",
        "",
        "SAFETY",
        "  The TUI never decides that an action is safe. It only displays the",
        "  daemon decision and forwards an explicit user response. Accept Edits",
        "  and Full Auto stay locked until impetusd exposes durable scoped grants.",
        "",
        "Esc closes this window.",
    ]
    .join("\n");
    render_text_modal(frame, " help · keymap ", &body, 82, 82, false, theme);
}

fn render_session_picker(
    frame: &mut Frame,
    app: &AppState,
    selected: usize,
    query: &str,
    theme: Theme,
) {
    let area = centered_rect(78, 76, frame.area());
    frame.render_widget(Clear, area);
    let block = panel_block(" sessions · Enter attach · N new · Esc close ", true, theme);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(2), Constraint::Min(3)])
        .split(inner);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("search: ", Style::default().fg(theme.muted)),
            Span::styled(query.to_owned(), Style::default().fg(theme.text)),
            Span::styled("█", Style::default().fg(theme.accent)),
        ])),
        rows[0],
    );
    let filtered = filtered_sessions(app, query);
    let items = filtered
        .iter()
        .map(|session| {
            ListItem::new(vec![
                Line::from(vec![
                    Span::styled(
                        truncate(&session.label, 36),
                        Style::default().fg(theme.text).add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        format!("  {}", session.status),
                        Style::default().fg(theme.green),
                    ),
                ]),
                Line::from(vec![
                    Span::styled(session.id.to_string(), Style::default().fg(theme.muted)),
                    Span::styled(
                        session
                            .workspace
                            .as_ref()
                            .map(|workspace| format!("  ·  {workspace}"))
                            .unwrap_or_default(),
                        Style::default().fg(theme.border),
                    ),
                ]),
            ])
        })
        .collect::<Vec<_>>();
    let mut state = ListState::default();
    state.select((!items.is_empty()).then_some(selected.min(items.len() - 1)));
    frame.render_stateful_widget(
        List::new(items)
            .highlight_symbol("▸ ")
            .highlight_style(theme.selected()),
        rows[1],
        &mut state,
    );
}

fn render_command_picker(frame: &mut Frame, selected: usize, query: &str, theme: Theme) {
    let area = centered_rect(72, 68, frame.area());
    frame.render_widget(Clear, area);
    let block = panel_block(" command palette · Enter run · Esc close ", true, theme);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(2), Constraint::Min(3)])
        .split(inner);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("/", Style::default().fg(theme.accent)),
            Span::styled(query.to_owned(), Style::default().fg(theme.text)),
            Span::styled("█", Style::default().fg(theme.accent)),
        ])),
        rows[0],
    );
    let suggestions = command::suggestions(query);
    let items = suggestions
        .iter()
        .map(|spec| {
            ListItem::new(Line::from(vec![
                Span::styled(
                    format!("/{:<16}", spec.name),
                    Style::default().fg(theme.cyan),
                ),
                Span::styled(spec.description, Style::default().fg(theme.text)),
                Span::styled(
                    if spec.shortcut.is_empty() {
                        String::new()
                    } else {
                        format!("  {}", spec.shortcut)
                    },
                    Style::default().fg(theme.muted),
                ),
            ]))
        })
        .collect::<Vec<_>>();
    let mut state = ListState::default();
    state.select((!items.is_empty()).then_some(selected.min(items.len() - 1)));
    frame.render_stateful_widget(
        List::new(items)
            .highlight_symbol("▸ ")
            .highlight_style(theme.selected()),
        rows[1],
        &mut state,
    );
}

fn render_mode_picker(frame: &mut Frame, app: &AppState, selected: usize, theme: Theme) {
    let area = centered_rect(70, 58, frame.area());
    frame.render_widget(Clear, area);
    let block = panel_block(" execution mode · Enter select · Esc close ", true, theme);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let items = ExecutionMode::ALL
        .iter()
        .map(|mode| {
            let available = mode.is_available(&app.connection.capabilities);
            let active = app.mode == *mode;
            ListItem::new(vec![
                Line::from(vec![
                    Span::styled(
                        if active { "● " } else { "○ " },
                        Style::default().fg(if active { theme.green } else { theme.border }),
                    ),
                    Span::styled(
                        format!("{:<14}", mode.label()),
                        Style::default()
                            .fg(if available { theme.text } else { theme.muted })
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        if available {
                            ""
                        } else {
                            "  🔒 daemon capability required"
                        },
                        Style::default().fg(theme.yellow),
                    ),
                ]),
                Line::from(Span::styled(
                    format!("  {}", mode.description()),
                    Style::default().fg(theme.muted),
                )),
            ])
        })
        .collect::<Vec<_>>();
    let mut state = ListState::default();
    state.select(Some(selected.min(items.len().saturating_sub(1))));
    frame.render_stateful_widget(
        List::new(items)
            .highlight_symbol("▸ ")
            .highlight_style(theme.selected()),
        inner,
        &mut state,
    );
}

fn render_approval(frame: &mut Frame, app: &AppState, selected: usize, theme: Theme) {
    let area = centered_rect(82, 70, frame.area());
    frame.render_widget(Clear, area);
    let count = app.approval_queue.len();
    let block = panel_block(
        format!(" approval required · 1/{count} · exact action "),
        true,
        theme,
    );
    let inner = block.inner(area).inner(Margin {
        horizontal: 1,
        vertical: 1,
    });
    frame.render_widget(block, area);
    let Some(approval) = app.approval_queue.front() else {
        frame.render_widget(Paragraph::new("No pending approval."), inner);
        return;
    };
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(8),
            Constraint::Length(4),
            Constraint::Length(1),
        ])
        .split(inner);
    let mut body = vec![
        Line::from(vec![
            Span::styled("ACTION   ", Style::default().fg(theme.muted)),
            Span::styled(
                approval.action_kind.clone(),
                Style::default()
                    .fg(theme.yellow)
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(vec![
            Span::styled("SUMMARY  ", Style::default().fg(theme.muted)),
            Span::styled(approval.summary.clone(), Style::default().fg(theme.text)),
        ]),
    ];
    if let Some(target) = &approval.target {
        body.push(Line::from(vec![
            Span::styled("TARGET   ", Style::default().fg(theme.muted)),
            Span::styled(target.clone(), Style::default().fg(theme.cyan)),
        ]));
    }
    body.extend([
        Line::from(vec![
            Span::styled("REASON   ", Style::default().fg(theme.muted)),
            Span::styled(approval.reason.clone(), Style::default().fg(theme.text)),
        ]),
        Line::from(""),
        Line::from(Span::styled(
            "The daemon matched this request to a stable action fingerprint.",
            Style::default().fg(theme.muted),
        )),
        Line::from(Span::styled(
            truncate(&approval.fingerprint, rows[0].width as usize),
            Style::default().fg(theme.border),
        )),
    ]);
    frame.render_widget(Paragraph::new(body).wrap(Wrap { trim: false }), rows[0]);
    let options = [
        ("Approve once", "Y", theme.green),
        ("Reject", "N", theme.red),
        ("Inspect diff/details", "D", theme.cyan),
    ];
    let option_lines = options
        .iter()
        .enumerate()
        .map(|(index, (label, key, color))| {
            Line::from(vec![
                Span::styled(
                    if selected == index { "▸ " } else { "  " },
                    Style::default().fg(*color),
                ),
                Span::styled(
                    format!("[{key}] {label}"),
                    Style::default()
                        .fg(*color)
                        .add_modifier(if selected == index {
                            Modifier::BOLD
                        } else {
                            Modifier::empty()
                        }),
                ),
            ])
        })
        .collect::<Vec<_>>();
    frame.render_widget(Paragraph::new(option_lines), rows[1]);
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "Esc does not approve; it leaves the request pending.",
            Style::default().fg(theme.muted),
        ))),
        rows[2],
    );
}

fn render_approval_detail(frame: &mut Frame, app: &AppState, theme: Theme) {
    let area = centered_rect(88, 82, frame.area());
    frame.render_widget(Clear, area);
    let block = panel_block(" approval detail · Esc back ", true, theme);
    let inner = block.inner(area).inner(Margin {
        horizontal: 1,
        vertical: 1,
    });
    frame.render_widget(block, area);
    let Some(approval) = app.approval_queue.front() else {
        frame.render_widget(Paragraph::new("No pending approval."), inner);
        return;
    };
    let mut lines = vec![
        Line::from(Span::styled(
            approval.summary.clone(),
            Style::default().fg(theme.text).add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
    ];
    if let Some(detail) = &approval.detail {
        if !detail.affected_files.is_empty() {
            lines.push(Line::from(Span::styled(
                "Affected files",
                Style::default().fg(theme.cyan).add_modifier(Modifier::BOLD),
            )));
            for file in &detail.affected_files {
                lines.push(Line::from(vec![
                    Span::styled("  • ", Style::default().fg(theme.border)),
                    Span::styled(file.clone(), Style::default().fg(theme.text)),
                ]));
            }
            lines.push(Line::from(""));
        }
        if let Some(scope) = &detail.estimated_scope {
            lines.push(Line::from(vec![
                Span::styled("Estimated scope: ", Style::default().fg(theme.muted)),
                Span::styled(scope.clone(), Style::default().fg(theme.yellow)),
            ]));
            lines.push(Line::from(""));
        }
        if !detail.attachment_refs.is_empty() {
            lines.push(Line::from(Span::styled(
                "Attachments",
                Style::default().fg(theme.cyan).add_modifier(Modifier::BOLD),
            )));
            for attachment in &detail.attachment_refs {
                lines.push(Line::from(vec![
                    Span::styled("  • ", Style::default().fg(theme.border)),
                    Span::styled(attachment.to_string(), Style::default().fg(theme.text)),
                ]));
            }
            lines.push(Line::from(""));
        }
        if let Some(diff) = &detail.diff_preview {
            lines.push(Line::from(Span::styled(
                "Diff preview",
                Style::default().fg(theme.cyan).add_modifier(Modifier::BOLD),
            )));
            for line in diff.lines() {
                let style = if line.starts_with('+') && !line.starts_with("+++") {
                    Style::default().fg(theme.green)
                } else if line.starts_with('-') && !line.starts_with("---") {
                    Style::default().fg(theme.red)
                } else if line.starts_with("@@") {
                    Style::default().fg(theme.magenta)
                } else {
                    Style::default().fg(theme.text)
                };
                lines.extend(render_plain_wrapped(line, inner.width as usize, style));
            }
        }
    } else {
        lines.push(Line::from(Span::styled(
            "Loading structured approval detail…",
            Style::default().fg(theme.muted),
        )));
    }
    frame.render_widget(Paragraph::new(lines), inner);
}

fn render_large_paste(frame: &mut Frame, app: &AppState, theme: Theme) {
    let bytes = app
        .pending_large_paste
        .as_ref()
        .map(|paste| paste.len())
        .unwrap_or_default();
    let direct_action = if bytes <= crate::model::MAX_DIRECT_PROMPT_BYTES {
        "[Y] send once through the current direct prompt path"
    } else {
        "[Y] unavailable: paste exceeds the safe direct-IPC budget"
    };
    let body = format!(
        "The paste is {} ({} lines). The current IPC accepts prompt text but does not yet expose chunked ArtifactStore upload.\n\n{}\n[I] insert into the composer so it can be trimmed or split\n[N] cancel and discard this pending paste\n\nThe production adapter also preflights the exact serialized IPC line before sending.",
        human_bytes(bytes),
        app.pending_large_paste
            .as_ref()
            .map(|paste| paste.lines().count())
            .unwrap_or_default(),
        direct_action,
    );
    render_text_modal(frame, " large paste ", &body, 72, 52, false, theme);
}

fn render_text_modal(
    frame: &mut Frame,
    title: &str,
    body: &str,
    width_percent: u16,
    height_percent: u16,
    error: bool,
    theme: Theme,
) {
    let area = centered_rect(width_percent, height_percent, frame.area());
    frame.render_widget(Clear, area);
    let mut block = panel_block(title, true, theme);
    if error {
        block = block.border_style(Style::default().fg(theme.red));
    }
    let inner = block.inner(area).inner(Margin {
        horizontal: 1,
        vertical: 1,
    });
    frame.render_widget(block, area);
    frame.render_widget(
        Paragraph::new(render_markdown(body, inner.width as usize, theme))
            .wrap(Wrap { trim: false }),
        inner,
    );
}

fn render_toast(frame: &mut Frame, app: &AppState, theme: Theme) {
    let Some(toast) = &app.toast else {
        return;
    };
    let width = (toast.text.chars().count() as u16 + 4)
        .min(frame.area().width.saturating_sub(4))
        .max(20);
    let area = Rect::new(
        frame.area().right().saturating_sub(width + 1),
        frame.area().bottom().saturating_sub(4),
        width,
        3,
    );
    frame.render_widget(Clear, area);
    let color = if toast.error { theme.red } else { theme.green };
    frame.render_widget(
        Paragraph::new(toast.text.clone())
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(color))
                    .style(theme.panel()),
            )
            .alignment(Alignment::Center),
        area,
    );
}

fn render_too_small(frame: &mut Frame, area: Rect, theme: Theme) {
    let text = vec![
        Line::from(Span::styled(
            "IMPETUS TUI",
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(Span::styled(
            format!(
                "Terminal is {}×{}; minimum is 52×14.",
                area.width, area.height
            ),
            Style::default().fg(theme.text),
        )),
        Line::from(Span::styled(
            "Resize the terminal. Ctrl+Q still exits.",
            Style::default().fg(theme.muted),
        )),
    ];
    frame.render_widget(
        Paragraph::new(text)
            .alignment(Alignment::Center)
            .block(panel_block(" resize required ", true, theme)),
        centered_rect(80, 50, area),
    );
}

fn panel_block<'a, T>(title: T, focused: bool, theme: Theme) -> Block<'a>
where
    T: Into<Line<'a>>,
{
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .title(title)
        .title_style(Style::default().fg(if focused { theme.accent } else { theme.muted }))
        .border_style(Style::default().fg(if focused { theme.accent } else { theme.border }))
        .style(theme.panel())
}

fn centered_rect(width_percent: u16, height_percent: u16, area: Rect) -> Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - height_percent) / 2),
            Constraint::Percentage(height_percent),
            Constraint::Percentage((100 - height_percent) / 2),
        ])
        .split(area);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - width_percent) / 2),
            Constraint::Percentage(width_percent),
            Constraint::Percentage((100 - width_percent) / 2),
        ])
        .split(vertical[1])[1]
}

pub fn filtered_sessions<'a>(
    app: &'a AppState,
    query: &str,
) -> Vec<&'a crate::model::SessionSummary> {
    let query = query.trim().to_ascii_lowercase();
    app.sessions
        .iter()
        .filter(|session| {
            query.is_empty()
                || session.label.to_ascii_lowercase().contains(&query)
                || session.id.to_string().contains(&query)
                || session
                    .workspace
                    .as_ref()
                    .is_some_and(|workspace| workspace.to_ascii_lowercase().contains(&query))
        })
        .collect()
}

fn format_time(unix_ms: u64) -> String {
    let seconds = (unix_ms / 1_000) % 86_400;
    format!(
        "{:02}:{:02}:{:02}",
        seconds / 3_600,
        (seconds % 3_600) / 60,
        seconds % 60
    )
}

fn compact_number(value: u64) -> String {
    if value >= 1_000_000 {
        format!("{:.1}m", value as f64 / 1_000_000.0)
    } else if value >= 1_000 {
        format!("{:.1}k", value as f64 / 1_000.0)
    } else {
        value.to_string()
    }
}

fn human_bytes(value: usize) -> String {
    if value >= 1024 * 1024 {
        format!("{:.1} MiB", value as f64 / (1024.0 * 1024.0))
    } else if value >= 1024 {
        format!("{:.1} KiB", value as f64 / 1024.0)
    } else {
        format!("{value} B")
    }
}

fn truncate(input: &str, max_chars: usize) -> String {
    if input.chars().count() <= max_chars {
        return input.to_owned();
    }
    if max_chars <= 1 {
        return "…".to_owned();
    }
    let mut output = input.chars().take(max_chars - 1).collect::<String>();
    output.push('…');
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ConnectionInfo, TimelineItem};
    use ratatui::{Terminal, backend::TestBackend};

    #[test]
    fn renders_narrow_and_wide_frames_without_panicking() {
        for (width, height) in [(80, 24), (144, 42)] {
            let backend = TestBackend::new(width, height);
            let mut terminal = Terminal::new(backend).unwrap();
            let mut app = AppState::new(ConnectionInfo::default());
            app.push_item(
                TimelineItem::new(1, 1_700_000_000_000, ItemKind::Assistant, "assistant")
                    .with_body("# Result\n\n```rust\nfn main() {}\n```"),
            );
            terminal
                .draw(|frame| render(frame, &app, Theme::default()))
                .unwrap();
        }
    }
}
