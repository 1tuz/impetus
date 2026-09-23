use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Direction, Layout, Margin, Rect},
    style::{Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, BorderType, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap},
};

use crate::{
    command,
    composer::ComposerLayoutMode,
    diff::{looks_like_diff, render_approval_diff, render_diff_observation, render_diff_view},
    hit::{HitKind, HitTarget, RectHit},
    markdown::{render_markdown, render_plain_wrapped},
    model::{
        AppState, EXECUTION_MODE_ALL, FilesFocus, Focus, ItemKind, Overlay, ReviewFocus, RunState,
        execution_mode_description, execution_mode_is_available, format_status_strip, short_id,
    },
    theme::Theme,
};

pub fn render(frame: &mut Frame, app: &mut AppState, theme: Theme) {
    app.hit_targets.clear();
    let area = frame.area();
    frame.render_widget(Block::default().style(theme.base()), area);

    if area.width < 52 || area.height < 14 {
        render_too_small(frame, area, theme);
        return;
    }

    let composer_width = area.width.saturating_sub(4);
    let composer_max_rows: u16 = match app.composer.layout_mode() {
        ComposerLayoutMode::SingleLine => 1,
        ComposerLayoutMode::MultiLine => 7,
    };
    let composer_rows = app
        .composer
        .view(composer_width, composer_max_rows)
        .total_rows
        .clamp(1, composer_max_rows as usize) as u16;
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
            app.session_model_label(),
            Style::default().fg(theme.yellow).bg(theme.surface),
        ),
        Span::styled("  ·  ", Style::default().fg(theme.border).bg(theme.surface)),
        Span::styled(
            app.current_branch
                .as_deref()
                .map(|name| format!("⎇ {name}"))
                .unwrap_or_else(|| "⎇ —".to_owned()),
            Style::default().fg(theme.green).bg(theme.surface),
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

fn render_main(frame: &mut Frame, area: Rect, app: &mut AppState, theme: Theme) {
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

fn render_sessions_panel(frame: &mut Frame, area: Rect, app: &mut AppState, theme: Theme) {
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
    let block = panel_block(" sessions · F2/Ctrl+O ", false, theme);
    let inner = block.inner(area);
    let list = List::new(items)
        .block(block)
        .highlight_style(theme.selected());
    let mut state = ListState::default();
    let selected = app
        .active_session
        .and_then(|active| app.sessions.iter().position(|session| session.id == active));
    state.select(selected);
    frame.render_stateful_widget(list, area, &mut state);

    let row_height = 2u16;
    for index in 0..app.sessions.len() {
        let y = inner
            .y
            .saturating_add((index as u16).saturating_mul(row_height));
        if y >= inner.y.saturating_add(inner.height) {
            break;
        }
        let height = row_height.min(inner.y.saturating_add(inner.height).saturating_sub(y));
        app.hit_targets.push(HitTarget {
            rect: RectHit::new(inner.x, y, inner.width, height),
            kind: HitKind::SessionPanelRow { index },
        });
    }
}

fn render_timeline(frame: &mut Frame, area: Rect, app: &mut AppState, theme: Theme) {
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

    let (lines, line_items) = build_timeline_lines(app, inner.width as usize, theme);
    let height = inner.height as usize;
    app.note_timeline_metrics(height, lines.len());
    let offset = app.line_scroll_from_bottom.min(lines.len());
    let end = lines.len().saturating_sub(offset);
    let start = end.saturating_sub(height);
    let visible = lines[start..end].to_vec();
    frame.render_widget(Paragraph::new(Text::from(visible)), inner);

    for (row_offset, item_index) in line_items[start..end].iter().enumerate() {
        let Some(index) = *item_index else {
            continue;
        };
        app.hit_targets.push(HitTarget {
            rect: RectHit::new(
                inner.x,
                inner.y.saturating_add(row_offset as u16),
                inner.width,
                1,
            ),
            kind: HitKind::TimelineItem { index },
        });
    }

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

/// Build painted timeline lines plus a parallel item-index map for hit-testing.
fn build_timeline_lines(
    app: &AppState,
    width: usize,
    theme: Theme,
) -> (Vec<Line<'static>>, Vec<Option<usize>>) {
    let width = width.max(8);
    let mut lines = Vec::new();
    let mut line_items = Vec::new();
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
                if item.streaming_key.is_some() {
                    "  …"
                } else if item.collapsed {
                    if item.kind == ItemKind::Activity {
                        "  ▸"
                    } else {
                        "  [collapsed]"
                    }
                } else {
                    ""
                },
                Style::default().fg(theme.muted),
            ),
        ]));
        line_items.push(Some(index));
        if !item.collapsed && (!item.body.is_empty() || item.streaming_key.is_some()) {
            let body_width = width.saturating_sub(3);
            let body_lines = if looks_like_diff(&item.body) {
                render_diff_view(&item.body, body_width, theme)
            } else {
                match item.kind {
                    ItemKind::Assistant | ItemKind::User | ItemKind::Plan => {
                        render_markdown(&item.body, body_width, theme)
                    }
                    _ => render_plain_wrapped(
                        &item.body,
                        body_width,
                        Style::default().fg(theme.text),
                    ),
                }
            };
            for line in body_lines {
                let mut spans = vec![
                    Span::styled("│ ", Style::default().fg(theme.border)),
                    Span::raw(" "),
                ];
                spans.extend(line.spans);
                lines.push(Line::from(spans));
                line_items.push(Some(index));
            }
        }
        lines.push(Line::from(""));
        line_items.push(Some(index));
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
        line_items.extend([None, None, None]);
    }
    debug_assert_eq!(lines.len(), line_items.len());
    (lines, line_items)
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
        if looks_like_diff(&item.body) {
            lines.extend(render_diff_view(&item.body, inner.width as usize, theme));
            if !item.details.is_empty() {
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled(
                    "Event metadata",
                    Style::default()
                        .fg(theme.muted)
                        .add_modifier(Modifier::BOLD),
                )));
                lines.extend(render_plain_wrapped(
                    &item.details,
                    inner.width as usize,
                    Style::default().fg(theme.muted),
                ));
            }
        } else {
            lines.extend(render_plain_wrapped(
                detail,
                inner.width as usize,
                Style::default().fg(theme.text),
            ));
        }
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

fn render_composer(frame: &mut Frame, area: Rect, app: &mut AppState, theme: Theme) {
    let focus = app.focus == Focus::Composer && matches!(app.overlay, Overlay::None);
    let mode = app.composer.layout_mode();
    let newline_hint = match mode {
        ComposerLayoutMode::SingleLine => "Alt+M multi",
        ComposerLayoutMode::MultiLine => {
            "Shift/Alt+Enter newline · \\+Enter continue · Alt+M single"
        }
    };
    let title = format!(
        " task · {} · {} · {} · Enter send · {newline_hint} ",
        app.mode.label(),
        app.prompt_intent.label(),
        mode.label()
    );
    let block = panel_block(title, focus, theme);
    let inner = block.inner(area).inner(Margin {
        horizontal: 1,
        vertical: 0,
    });
    frame.render_widget(block, area);
    let max_rows = match mode {
        ComposerLayoutMode::SingleLine => 1,
        ComposerLayoutMode::MultiLine => inner.height,
    };
    let view = app.composer.view(inner.width.saturating_sub(2), max_rows);
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
    app.hit_targets.push(HitTarget {
        rect: RectHit::new(area.x, area.y, area.width, area.height),
        kind: HitKind::Composer,
    });
}

fn render_footer(frame: &mut Frame, area: Rect, app: &AppState, theme: Theme) {
    let left =
        " ? help  Ctrl+Shift+A attach  Ctrl+B branch  Ctrl+F files  Ctrl+\\ pty  Ctrl+Q quit";
    let right = format!(" {} ", format_status_strip(app));
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

fn render_overlay(frame: &mut Frame, app: &mut AppState, theme: Theme) {
    // Clone so hit recording can mutably borrow `app` while matching overlay.
    let overlay = app.overlay.clone();
    match overlay {
        Overlay::None => {}
        Overlay::Help => render_help(frame, theme),
        Overlay::Sessions { selected, query } => {
            render_session_picker(frame, app, selected, &query, theme)
        }
        Overlay::Commands { selected, query } => {
            render_command_picker(frame, selected, &query, theme)
        }
        Overlay::Modes { selected } => render_mode_picker(frame, app, selected, theme),
        Overlay::Themes { selected } => render_theme_picker(frame, app, selected, theme),
        Overlay::ModelPicker { state } => render_model_picker(frame, app, &state, theme),
        Overlay::Approval { selected } => render_approval(frame, app, selected, theme),
        Overlay::ApprovalDetail => render_approval_detail(frame, app, theme),
        Overlay::LargePaste => render_large_paste(frame, app, theme),
        Overlay::AttachPath { path } => render_text_modal(
            frame,
            " attach path ",
            &format!(
                "path: {path}_\n\nType absolute or relative filesystem path.\nEnter uploads via chunked artifact_upload (DurableArtifactRef in composer).\nEsc cancel · max {}.",
                human_bytes(crate::model::MAX_PASTE_UPLOAD_BYTES)
            ),
            72,
            48,
            false,
            theme,
        ),
        Overlay::TextPrompt { title, value, .. } => render_text_modal(
            frame,
            &title,
            &format!("{value}\n\nType value · Enter confirm · Esc cancel"),
            70,
            40,
            false,
            theme,
        ),
        Overlay::Checkpoints {
            selected,
            checkpoints,
        } => {
            let mut body = String::from("Enter restore · N create · Esc close\n\n");
            if checkpoints.is_empty() {
                body.push_str("(no checkpoints yet — press N to name one)");
            } else {
                for (idx, cp) in checkpoints.iter().enumerate() {
                    let mark = if idx == selected { "▸" } else { " " };
                    let id = crate::model::short_id(cp.id);
                    body.push_str(&format!(
                        "{mark} {id}  @{seq}  {name}\n",
                        seq = cp.sequence,
                        name = cp.name
                    ));
                }
            }
            render_text_modal(frame, " checkpoints ", &body, 84, 70, false, theme);
        }
        Overlay::Diagnostics { text } => render_text_modal(
            frame,
            " diagnostics · redacted ",
            &text,
            84,
            80,
            false,
            theme,
        ),
        Overlay::Message { title, body, error } => {
            render_text_modal(frame, &title, &body, 70, 55, error, theme)
        }
        Overlay::Files { state } => render_files(frame, &state, theme),
        Overlay::Branches {
            selected,
            query,
            branches,
        } => render_branches(frame, selected, &query, &branches, theme),
        Overlay::Review { state } => render_review(frame, &state, theme),
    }
}

fn render_help(frame: &mut Frame, theme: Theme) {
    let body = [
        "NAVIGATION",
        "  PageUp/PageDown   scroll durable event history",
        "  Home              scroll timeline to top (when timeline focused)",
        "  End               resume following the newest events",
        "  Alt+Up/Down       select event for inspector",
        "  Enter             collapse/expand selected event when timeline focused",
        "  Tab               cycle focus Composer → Timeline → Inspector",
        "  click event       select timeline item; double-click toggle collapse",
        "",
        "COMPOSER",
        "  Enter             submit task",
        "  Shift/Alt+Enter   newline (multiline mode)",
        "  Ctrl+J            newline (multiline mode)",
        "  \\ then Enter      continue line (multiline mode)",
        "  Alt+M             toggle single-line / multiline",
        "  Up/Down           prompt history",
        "  Ctrl+A/E          line start/end",
        "  Ctrl+W            delete previous word",
        "  Ctrl+P            command palette",
        "  Ctrl+Shift+P      cycle intent Prompt → Steer → FollowUp",
        "  Ctrl+T            set Steer intent (active run) or show hint",
        "  /prompt /steer /follow-up   composer intent",
        "  click composer    focus input",
        "",
        "HARNESS",
        "  ? / F1            keymap help ( ? only when composer empty )",
        "  F2 / Ctrl+O       session picker",
        "  Ctrl+F            workspace Files (tree + preview via IPC)",
        "  Ctrl+Shift+A      attach local file (artifact_upload → ArtifactRef)",
        "  Ctrl+B            git branch picker (list/filter/switch/create)",
        "  Ctrl+\\            PTY pass-through ($SHELL; Ctrl+] detaches)",
        "  F6 / Ctrl+R       Review pane (changed files + GetFileDiff)",
        "  F7                session checkpoints (list · Enter restore · N name)",
        "  Ctrl+Shift+K      fork active session at tip (shared-prefix)",
        "  F3                toggle inspector",
        "  Shift+Tab         cycle ASK → ACCEPT EDITS → PLAN → AUTO (daemon IPC)",
        "  F4                execution mode picker (includes BYPASS when unlocked)",
        "  F8 / /model       Provider→Model→Reasoning→options (daemon catalog)",
        "  /mode /plan /ask /auto   set mode via daemon IPC",
        "  /theme            theme picker (Impetus neon + geek pack)",
        "  /attach [path]    attach local file via durable artifact_upload",
        "  /files            workspace Files overlay",
        "  /fork [seq]       fork session at tip or sequence",
        "  /checkpoint [name]  create named checkpoint (prompt if no name)",
        "  /checkpoints      list checkpoints; Enter restores new branch",
        "  /pty [cmd…]       PTY pass-through (no ANSI emulator)",
        "  /review           Review pane (daemon Git IPC)",
        "  Ctrl+Shift+T      cycle theme",
        "  F5                theme picker",
        "  F6                Review pane",
        "  F7                checkpoints overlay",
        "  Ctrl+C            cancel run or dismiss current input",
        "  Ctrl+L            clear local viewport",
        "  Ctrl+D            open selected diff/details",
        "  Ctrl+Q            quit client (daemon sessions keep running)",
        "  Y / N             approve once / reject exact pending action",
        "  click session     activate (picker or side panel)",
        "  click Y / N       resolve approval when overlay open",
        "",
        "FILES OVERLAY",
        "  Up/Down           move tree selection (loads preview via ReadWorkspaceFile)",
        "  Left/Right/Enter  collapse / expand directory or open file preview",
        "  Tab               focus tree ↔ preview",
        "  r                 refresh listing from harness",
        "  Esc               close",
        "",
        "REVIEW PANE",
        "  Up/Down           select changed file (loads GetFileDiff)",
        "  Enter/Right       open selected file diff",
        "  Tab               focus files ↔ diff",
        "  n / ]             next hunk · [ / p (in diff) previous hunk",
        "  r                 refresh via GitStatus / GetDiff",
        "  Esc               close (approvals stay independent)",
        "",
        "FORK / CHECKPOINT",
        "  /fork [seq]       ForkSession at tip or sequence (needs events)",
        "  Ctrl+Shift+K      same as /fork tip",
        "  /checkpoint name  CreateCheckpoint at tip",
        "  F7 /checkpoints   list · Enter RestoreCheckpoint (new branch)",
        "  N in list         name prompt for CreateCheckpoint",
        "  header label      shows fork@seq ← parent when SessionInfo has it",
        "",
        "BRANCH PICKER",
        "  Enter             switch to selected branch (daemon SwitchBranch)",
        "  Ctrl+N            create+checkout from filter text",
        "  type              filter; Enter with no matches creates+checkouts name",
        "  Esc               cancel",
        "",
        "SESSION PICKER",
        "  Enter             attach selected session",
        "  N / Ctrl+N        workspace path prompt → CreateSession",
        "  type              filter by label / id / workspace",
        "",
        "SAFETY",
        "  Execution mode is daemon-owned via IPC; the TUI never injects mode",
        "  text into prompts. ACCEPT EDITS and BYPASS stay locked until impetusd",
        "  exposes the matching approval-scope capabilities.",
        "",
        "Esc closes this window.",
    ]
    .join("\n");
    render_text_modal(frame, " help · keymap ", &body, 86, 90, false, theme);
}

fn render_files(frame: &mut Frame, state: &crate::model::FilesOverlayState, theme: Theme) {
    let area = centered_rect(90, 84, frame.area());
    frame.render_widget(Clear, area);
    let title = match state.focus {
        FilesFocus::Search => format!(
            " files · search: {}█ · Enter run · Esc cancel ",
            state.search_query
        ),
        _ if state.search_active => format!(
            " files · {} hits{} · Esc clear · Enter preview ",
            state.search_hits.len(),
            if state.search_truncated {
                " (truncated)"
            } else {
                ""
            }
        ),
        _ => match state.selected_row() {
            Some(row) => format!(
                " files · {} · / search · type filter · Tab · r · Esc ",
                row.path
            ),
            None => " files · / search · type filter · Tab · r refresh · Esc ".to_owned(),
        },
    };
    let block = panel_block(title, true, theme);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(42), Constraint::Percentage(58)])
        .split(inner);

    let tree_focused = state.focus == FilesFocus::Tree;
    let preview_focused = state.focus == FilesFocus::Preview;
    let tree_block = panel_block(" tree ", tree_focused, theme);
    let preview_block = panel_block(" preview ", preview_focused, theme);
    let tree_inner = tree_block.inner(cols[0]);
    let preview_inner = preview_block.inner(cols[1]);
    frame.render_widget(tree_block, cols[0]);
    frame.render_widget(preview_block, cols[1]);

    let rows = state.visible_rows();
    let items = rows
        .iter()
        .enumerate()
        .map(|(idx, row)| {
            let marker = if row.is_dir {
                if row.expanded { "▾ " } else { "▸ " }
            } else {
                "  "
            };
            let indent = "  ".repeat(row.depth);
            let label = format!("{indent}{marker}{}", row.name);
            let mut style = Style::default().fg(if row.is_dir { theme.accent } else { theme.text });
            if idx == state.selected {
                style = style.bg(theme.surface_alt).add_modifier(Modifier::BOLD);
            }
            ListItem::new(Line::from(Span::styled(label, style)))
        })
        .collect::<Vec<_>>();

    let mut list_state = ListState::default();
    if !rows.is_empty() {
        list_state.select(Some(state.selected.min(rows.len() - 1)));
    }
    if state.loading_dirs.contains(".") && rows.is_empty() {
        frame.render_widget(
            Paragraph::new(Span::styled("loading…", Style::default().fg(theme.muted))),
            tree_inner,
        );
    } else if let Some(error) = &state.error {
        frame.render_widget(
            Paragraph::new(Span::styled(error.clone(), Style::default().fg(theme.red)))
                .wrap(Wrap { trim: true }),
            tree_inner,
        );
    } else {
        frame.render_stateful_widget(List::new(items), tree_inner, &mut list_state);
    }

    let preview_body = if state.preview_loading {
        "loading preview…".to_owned()
    } else if let Some(error) = &state.preview_error {
        error.clone()
    } else if let Some(text) = &state.preview_text {
        text.clone()
    } else {
        match state.selected_row() {
            Some(row) if row.is_dir => "(directory — Enter/Right to expand)".to_owned(),
            Some(_) => "(select a file for preview)".to_owned(),
            None => "(empty)".to_owned(),
        }
    };
    let preview_style = if state.preview_error.is_some() {
        Style::default().fg(theme.red)
    } else {
        Style::default().fg(theme.text)
    };
    frame.render_widget(
        Paragraph::new(preview_body)
            .style(preview_style)
            .wrap(Wrap { trim: false })
            .scroll((state.preview_scroll as u16, 0)),
        preview_inner,
    );
}

fn render_review(frame: &mut Frame, state: &crate::model::ReviewOverlayState, theme: Theme) {
    let area = centered_rect(92, 86, frame.area());
    frame.render_widget(Clear, area);
    let dirty = if state.dirty { "dirty" } else { "clean" };
    let title = format!(
        " review · {} · {dirty} · F6/Ctrl+R · Tab focus · n/] hunks · Esc close ",
        if state.branch_label.is_empty() {
            "?"
        } else {
            state.branch_label.as_str()
        }
    );
    let block = panel_block(title, true, theme);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(38), Constraint::Percentage(62)])
        .split(inner);

    let files_focused = state.focus == ReviewFocus::Files;
    let diff_focused = state.focus == ReviewFocus::Diff;
    let files_block = panel_block(" changed files ", files_focused, theme);
    let diff_block = panel_block(
        match state.diff_path.as_deref() {
            Some(path) => format!(" diff · {path} "),
            None => " diff ".to_owned(),
        },
        diff_focused,
        theme,
    );
    let files_inner = files_block.inner(cols[0]);
    let diff_inner = diff_block.inner(cols[1]);
    frame.render_widget(files_block, cols[0]);
    frame.render_widget(diff_block, cols[1]);

    let mut file_lines = Vec::new();
    if state.loading {
        file_lines.push(Line::from(Span::styled(
            "Loading GitStatus…",
            Style::default().fg(theme.muted),
        )));
    } else if let Some(error) = &state.error {
        file_lines.push(Line::from(Span::styled(
            error.clone(),
            Style::default().fg(theme.red),
        )));
    } else if state.files.is_empty() {
        file_lines.push(Line::from(Span::styled(
            "(no changed files)",
            Style::default().fg(theme.muted),
        )));
    } else {
        for (idx, row) in state.files.iter().enumerate() {
            let stats = match (row.insertions, row.deletions) {
                (Some(i), Some(d)) => format!(" +{i}/-{d}"),
                (Some(i), None) => format!(" +{i}"),
                (None, Some(d)) => format!(" -{d}"),
                (None, None) => String::new(),
            };
            let code = if row.status_code.is_empty() {
                String::new()
            } else {
                format!(" {}", row.status_code.trim())
            };
            let label = format!("{}{} {}{}", row.kind_label, code, row.path, stats);
            let mut style = Style::default().fg(theme.text);
            if idx == state.selected {
                style = style.bg(theme.surface_alt).add_modifier(Modifier::BOLD);
            }
            file_lines.push(Line::from(Span::styled(label, style)));
        }
    }
    frame.render_widget(Paragraph::new(file_lines), files_inner);

    let mut diff_lines = Vec::new();
    if state.diff_loading {
        diff_lines.push(Line::from(Span::styled(
            "Loading GetFileDiff…",
            Style::default().fg(theme.muted),
        )));
    } else if let Some(error) = &state.diff_error {
        diff_lines.push(Line::from(Span::styled(
            error.clone(),
            Style::default().fg(theme.red),
        )));
    } else if let Some(obs) = state
        .diff_observation
        .as_ref()
        .filter(|o| !o.hunks.is_empty())
    {
        if !obs.hunks.is_empty() {
            diff_lines.push(Line::from(Span::styled(
                format!(
                    "structured · {} hunks · +{}/-{} · n/] next · [ /p prev",
                    obs.hunks.len(),
                    obs.insertions,
                    obs.deletions
                ),
                Style::default().fg(theme.muted),
            )));
            diff_lines.push(Line::from(""));
        }
        diff_lines.extend(render_diff_observation(
            obs,
            diff_inner.width as usize,
            theme,
        ));
    } else if let Some(patch) = &state.diff_patch {
        if !state.hunk_line_idxs.is_empty() {
            diff_lines.push(Line::from(Span::styled(
                format!(
                    "hunk {}/{} · n/] next · [ /p prev",
                    state.selected_hunk.saturating_add(1),
                    state.hunk_line_idxs.len()
                ),
                Style::default().fg(theme.muted),
            )));
            diff_lines.push(Line::from(""));
        }
        diff_lines.extend(render_diff_view(patch, diff_inner.width as usize, theme));
    } else {
        diff_lines.push(Line::from(Span::styled(
            "Select a file and press Enter.",
            Style::default().fg(theme.muted),
        )));
    }
    let scroll = state.diff_scroll.min(diff_lines.len().saturating_sub(1)) as u16;
    frame.render_widget(
        Paragraph::new(diff_lines)
            .wrap(Wrap { trim: false })
            .scroll((scroll, 0)),
        diff_inner,
    );
}

fn render_branches(
    frame: &mut Frame,
    selected: usize,
    query: &str,
    branches: &[impetus_client::protocol::GitBranchInfo],
    theme: Theme,
) {
    let area = centered_rect(72, 70, frame.area());
    frame.render_widget(Clear, area);
    let block = panel_block(
        " branches · Enter switch · Ctrl+N create · Esc close ",
        true,
        theme,
    );
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(2), Constraint::Min(3)])
        .split(inner);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("filter: ", Style::default().fg(theme.muted)),
            Span::styled(query.to_owned(), Style::default().fg(theme.text)),
            Span::styled("█", Style::default().fg(theme.accent)),
        ])),
        rows[0],
    );
    let filtered = filtered_branches(branches, query);
    let items = filtered
        .iter()
        .enumerate()
        .map(|(idx, branch)| {
            let marker = if branch.current { "* " } else { "  " };
            let mut style = Style::default().fg(theme.text);
            if idx == selected {
                style = style.bg(theme.surface_alt).add_modifier(Modifier::BOLD);
            }
            ListItem::new(Line::from(Span::styled(
                format!("{marker}{}", branch.name),
                style,
            )))
        })
        .collect::<Vec<_>>();
    let mut list_state = ListState::default();
    if !filtered.is_empty() {
        list_state.select(Some(selected.min(filtered.len() - 1)));
    }
    frame.render_stateful_widget(List::new(items), rows[1], &mut list_state);
}

fn render_session_picker(
    frame: &mut Frame,
    app: &mut AppState,
    selected: usize,
    query: &str,
    theme: Theme,
) {
    let area = centered_rect(78, 76, frame.area());
    frame.render_widget(Clear, area);
    let block = panel_block(
        " sessions · Enter attach · N/Ctrl+N new · Esc close ",
        true,
        theme,
    );
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

    let list_area = rows[1];
    let row_height = 2u16;
    for index in 0..filtered.len() {
        let y = list_area
            .y
            .saturating_add((index as u16).saturating_mul(row_height));
        if y >= list_area.y.saturating_add(list_area.height) {
            break;
        }
        let height = row_height.min(
            list_area
                .y
                .saturating_add(list_area.height)
                .saturating_sub(y),
        );
        app.hit_targets.push(HitTarget {
            rect: RectHit::new(list_area.x, y, list_area.width, height),
            kind: HitKind::SessionPickerRow { index },
        });
    }
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
    let block = panel_block(
        " execution mode · Enter confirm via IPC · Esc close ",
        true,
        theme,
    );
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let items = EXECUTION_MODE_ALL
        .iter()
        .map(|mode| {
            let available = execution_mode_is_available(*mode, &app.connection.capabilities);
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
                    format!("  {}", execution_mode_description(*mode)),
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

fn render_model_picker(
    frame: &mut Frame,
    app: &AppState,
    state: &crate::catalog::ModelPickerState,
    theme: Theme,
) {
    use crate::catalog::{
        ModelPickerStep, filter_by_query, models_for_provider, option_choices, provider_choices,
        row_is_selectable, session_model_label,
    };

    let area = centered_rect(78, 72, frame.area());
    frame.render_widget(Clear, area);
    let step_label = match state.step {
        ModelPickerStep::Provider => "provider",
        ModelPickerStep::Model => "model",
        ModelPickerStep::Reasoning => "reasoning",
        ModelPickerStep::Options => "options",
    };
    let draft = session_model_label(
        Some(&impetus_client::protocol::SessionModelSelection {
            provider_id: state
                .draft_provider_id
                .clone()
                .unwrap_or_else(|| "—".into()),
            model_id: state.draft_model_id.clone().unwrap_or_else(|| "—".into()),
            reasoning_effort: state.draft_reasoning.clone(),
            service_tier: None,
            provider_options: serde_json::Value::Null,
        }),
        state.draft_options.as_ref(),
    );
    let title = format!(
        " model · {step_label} · Enter · Esc/Left back · filter `{query}` · {draft} ",
        query = if state.query.is_empty() {
            "…"
        } else {
            state.query.as_str()
        }
    );
    let block = panel_block(title.as_str(), true, theme);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let items: Vec<ListItem> = match state.step {
        ModelPickerStep::Provider => {
            let providers = provider_choices(&app.provider_catalog);
            let filtered = filter_by_query(&providers, &state.query, |p| p.display_name.clone());
            filtered
                .into_iter()
                .map(|(_, p)| {
                    let active = app
                        .session_model
                        .as_ref()
                        .is_some_and(|s| s.provider_id == p.provider_id);
                    ListItem::new(Line::from(vec![
                        Span::styled(
                            if active { "● " } else { "○ " },
                            Style::default().fg(if active { theme.green } else { theme.border }),
                        ),
                        Span::styled(
                            p.display_name.clone(),
                            Style::default()
                                .fg(if p.selectable {
                                    theme.text
                                } else {
                                    theme.muted
                                })
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(
                            if p.selectable {
                                format!("  ({})", p.provider_id)
                            } else {
                                format!("  ({})  unavailable", p.provider_id)
                            },
                            Style::default().fg(theme.muted),
                        ),
                    ]))
                })
                .collect()
        }
        ModelPickerStep::Model => {
            let provider_id = state.draft_provider_id.as_deref().unwrap_or("");
            let models = models_for_provider(&app.provider_catalog, provider_id);
            let filtered = filter_by_query(&models, &state.query, |row| {
                row.model_display_name
                    .clone()
                    .unwrap_or_else(|| row.model_id.clone())
            });
            filtered
                .into_iter()
                .map(|(_, row)| {
                    let selectable = row_is_selectable(row);
                    let active = app.session_model.as_ref().is_some_and(|s| {
                        s.provider_id == row.provider_id && s.model_id == row.model_id
                    });
                    let name = row
                        .model_display_name
                        .clone()
                        .unwrap_or_else(|| row.model_id.clone());
                    ListItem::new(Line::from(vec![
                        Span::styled(
                            if active { "● " } else { "○ " },
                            Style::default().fg(if active { theme.green } else { theme.border }),
                        ),
                        Span::styled(
                            name,
                            Style::default()
                                .fg(if selectable { theme.text } else { theme.muted })
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(
                            if selectable {
                                String::new()
                            } else {
                                "  unavailable".to_owned()
                            },
                            Style::default().fg(theme.yellow),
                        ),
                    ]))
                })
                .collect()
        }
        ModelPickerStep::Reasoning => {
            let provider_id = state.draft_provider_id.as_deref().unwrap_or("");
            let model_id = state.draft_model_id.as_deref().unwrap_or("");
            let efforts = crate::catalog::find_row(&app.provider_catalog, provider_id, model_id)
                .map(|row| row.reasoning_efforts.clone())
                .unwrap_or_default();
            efforts
                .into_iter()
                .map(|effort| {
                    let active = state.draft_reasoning.as_deref() == Some(effort.as_str())
                        || app
                            .session_model
                            .as_ref()
                            .and_then(|s| s.reasoning_effort.as_deref())
                            == Some(effort.as_str());
                    ListItem::new(Line::from(vec![
                        Span::styled(
                            if active { "● " } else { "○ " },
                            Style::default().fg(if active { theme.green } else { theme.border }),
                        ),
                        Span::styled(effort, Style::default().fg(theme.text)),
                    ]))
                })
                .collect()
        }
        ModelPickerStep::Options => {
            let provider_id = state.draft_provider_id.as_deref().unwrap_or("");
            let model_id = state.draft_model_id.as_deref().unwrap_or("");
            let mut items = vec![ListItem::new(Line::from(Span::styled(
                "○ Skip options (catalog only)",
                Style::default().fg(theme.muted),
            )))];
            if let Some(row) =
                crate::catalog::find_row(&app.provider_catalog, provider_id, model_id)
            {
                for choice in option_choices(row) {
                    items.push(ListItem::new(Line::from(Span::styled(
                        format!("○ {}", choice.label),
                        Style::default().fg(theme.text),
                    ))));
                }
            }
            items
        }
    };

    if items.is_empty() {
        frame.render_widget(
            Paragraph::new(Span::styled(
                "Catalog empty for this step.",
                Style::default().fg(theme.muted),
            )),
            inner,
        );
        return;
    }

    let mut list_state = ListState::default();
    list_state.select(Some(state.selected.min(items.len().saturating_sub(1))));
    frame.render_stateful_widget(
        List::new(items)
            .highlight_symbol("▸ ")
            .highlight_style(theme.selected()),
        inner,
        &mut list_state,
    );
}

fn render_theme_picker(frame: &mut Frame, app: &AppState, selected: usize, theme: Theme) {
    use crate::theme::THEME_CATALOG;

    let area = centered_rect(72, 72, frame.area());
    frame.render_widget(Clear, area);
    let block = panel_block(
        " theme · Enter select · Ctrl+Shift+T cycle · Esc close ",
        true,
        theme,
    );
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let items = THEME_CATALOG
        .iter()
        .map(|meta| {
            let active = app.theme_id == meta.id;
            ListItem::new(vec![
                Line::from(vec![
                    Span::styled(
                        if active { "● " } else { "○ " },
                        Style::default().fg(if active { theme.accent } else { theme.border }),
                    ),
                    Span::styled(
                        format!("{:<18}", meta.label),
                        Style::default().fg(theme.text).add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(meta.id, Style::default().fg(theme.cyan)),
                ]),
                Line::from(Span::styled(
                    format!("  {}", meta.blurb),
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

fn render_approval(frame: &mut Frame, app: &mut AppState, selected: usize, theme: Theme) {
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
    let option_kinds = [
        HitKind::ApprovalAccept,
        HitKind::ApprovalReject,
        HitKind::ApprovalInspect,
    ];
    for (index, kind) in option_kinds.into_iter().enumerate() {
        let y = rows[1].y.saturating_add(index as u16);
        if y >= rows[1].y.saturating_add(rows[1].height) {
            break;
        }
        app.hit_targets.push(HitTarget {
            rect: RectHit::new(rows[1].x, y, rows[1].width, 1),
            kind,
        });
    }
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
                let fetched = detail
                    .attachments
                    .iter()
                    .find(|body| body.id == *attachment);
                let label = match fetched {
                    Some(body) => {
                        let preview = String::from_utf8_lossy(&body.content);
                        let preview = preview.lines().next().unwrap_or("").trim();
                        if preview.is_empty() {
                            format!("{attachment} ({})", body.content_type)
                        } else {
                            let clipped: String = preview.chars().take(72).collect();
                            format!("{attachment} · {clipped}")
                        }
                    }
                    None => attachment.to_string(),
                };
                lines.push(Line::from(vec![
                    Span::styled("  • ", Style::default().fg(theme.border)),
                    Span::styled(label, Style::default().fg(theme.text)),
                ]));
            }
            lines.push(Line::from(""));
        }
        let diff_lines = render_approval_diff(
            detail.diff_observation.as_ref(),
            detail.diff_preview.as_deref(),
            inner.width as usize,
            theme,
        );
        if !diff_lines.is_empty() {
            lines.push(Line::from(Span::styled(
                "Diff preview",
                Style::default().fg(theme.cyan).add_modifier(Modifier::BOLD),
            )));
            lines.push(Line::from(""));
            lines.extend(diff_lines);
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
    let lines = app
        .pending_large_paste
        .as_ref()
        .map(|paste| crate::model::paste_line_count(paste))
        .unwrap_or_default();
    let placeholder = crate::model::format_paste_placeholder(bytes, lines);
    let body = format!(
        "Composer shows compact placeholder:\n{placeholder}\n\nSize: {} · {lines} lines.\nConfirm uploads via chunked artifact_upload; durable events keep only ArtifactRef + label.\n\n[Y] upload and send\n[I] insert full text into the composer for editing\n[N] cancel and discard this pending paste\n\nMaximum upload size: {}.",
        human_bytes(bytes),
        human_bytes(crate::model::MAX_PASTE_UPLOAD_BYTES),
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
    let content = if looks_like_diff(body) {
        render_diff_view(body, inner.width as usize, theme)
    } else {
        render_markdown(body, inner.width as usize, theme)
    };
    frame.render_widget(Paragraph::new(content).wrap(Wrap { trim: false }), inner);
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

pub fn filtered_branches<'a>(
    branches: &'a [impetus_client::protocol::GitBranchInfo],
    query: &str,
) -> Vec<&'a impetus_client::protocol::GitBranchInfo> {
    let needle = query.trim().to_ascii_lowercase();
    branches
        .iter()
        .filter(|branch| needle.is_empty() || branch.name.to_ascii_lowercase().contains(&needle))
        .collect()
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
    use crate::model::{ConnectionInfo, SessionSummary, TimelineItem};
    use ratatui::{Terminal, backend::TestBackend};
    use uuid::Uuid;

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
                .draw(|frame| render(frame, &mut app, Theme::default()))
                .unwrap();
        }
    }

    #[test]
    fn filtered_sessions_matches_label_id_and_workspace() {
        let mut app = AppState::new(ConnectionInfo::default());
        let keep = Uuid::from_u128(0x10);
        let drop = Uuid::from_u128(0x20);
        app.sessions = vec![
            SessionSummary {
                id: keep,
                label: "Router hardening".to_owned(),
                status: "working".to_owned(),
                workspace: Some("~/dev/impetus".to_owned()),
            },
            SessionSummary {
                id: drop,
                label: "other".to_owned(),
                status: "saved".to_owned(),
                workspace: Some("~/tmp".to_owned()),
            },
        ];
        assert_eq!(filtered_sessions(&app, "router").len(), 1);
        assert_eq!(filtered_sessions(&app, "impetus").len(), 1);
        assert_eq!(filtered_sessions(&app, &keep.to_string()).len(), 1);
        assert!(filtered_sessions(&app, "missing").is_empty());
    }
}
