//! Durable UiEvent → timeline projection.

use uuid::Uuid;

use crate::model::{
    AppState, ItemKind, Overlay, RunState, TimelineItem, UiEvent, UiEventKind, bounded,
    ingest_stream_chunk,
};

use super::short;

pub(super) fn ingest_event(app: &mut AppState, event: UiEvent) {
    if event.sequence <= app.last_sequence {
        return;
    }
    let sequence = event.sequence;
    let at = event.at_unix_ms;
    match event.kind {
        UiEventKind::SessionCreated => app.push_item(
            TimelineItem::new(sequence, at, ItemKind::Notice, "session created")
                .with_body("Durable session created by impetusd."),
        ),
        UiEventKind::SessionWorkspace { workspace } => {
            if let Some(active) = app.active_session
                && let Some(session) = app.sessions.iter_mut().find(|session| session.id == active)
            {
                session.workspace = Some(workspace.clone());
            }
            app.push_item(
                TimelineItem::new(sequence, at, ItemKind::Notice, "workspace")
                    .with_body(workspace.clone())
                    .with_details(format!("workspace_root: {workspace}")),
            );
        }
        UiEventKind::SessionAttached => {
            app.status_message = "attached".to_owned();
            app.last_sequence = sequence;
        }
        UiEventKind::ExecutionModeChanged { mode } => {
            app.mode = mode;
            app.push_item(
                TimelineItem::new(sequence, at, ItemKind::Notice, "execution mode")
                    .with_body(format!("daemon mode set to {}", mode.label())),
            );
        }
        UiEventKind::UserInput { text, artifact } => {
            close_tools_activity(app);
            close_reasoning_activity(app);
            let stripped = strip_mode_prefix(&text);
            let body = match artifact {
                Some(artifact) => {
                    let ref_line = crate::model::format_artifact_ref_label(
                        &artifact.id,
                        artifact.byte_count,
                        None,
                    );
                    if stripped.is_empty() {
                        ref_line
                    } else {
                        format!("{stripped}\n{ref_line}")
                    }
                }
                None => stripped,
            };
            app.push_item(TimelineItem::new(sequence, at, ItemKind::User, "you").with_body(body));
        }
        UiEventKind::Plan { summary } => app
            .push_item(TimelineItem::new(sequence, at, ItemKind::Plan, "plan").with_body(summary)),
        UiEventKind::RunStarted { run_id } => {
            app.run_state = RunState::Working;
            set_active_session_status(app, "working");
            app.status_message = format!("run {}", short(run_id));
            app.push_item(
                TimelineItem::new(sequence, at, ItemKind::Notice, "run started")
                    .with_body(format!("run_id: {run_id}")),
            );
        }
        UiEventKind::RunCompleted { run_id } => {
            app.flush_stream_to_timeline();
            close_tools_activity(app);
            close_reasoning_activity(app);
            app.run_state = RunState::Idle;
            set_active_session_status(app, "ready");
            app.status_message = "complete".to_owned();
            app.push_item(
                TimelineItem::new(sequence, at, ItemKind::Notice, "run completed")
                    .with_body(format!("run_id: {run_id}")),
            );
        }
        UiEventKind::RunFailed { run_id, reason } => {
            app.flush_stream_to_timeline();
            app.run_state = RunState::Failed;
            set_active_session_status(app, "failed");
            app.status_message = "failed".to_owned();
            let hint = crate::model::remediation_hint("run failed", None);
            push_or_coalesce_noise(
                app,
                TimelineItem::new(sequence, at, ItemKind::Error, "run failed")
                    .with_body(format!("{reason}\n→ {hint}"))
                    .with_details(format!("run_id: {run_id}\nremediation: {hint}")),
            );
            app.show_toast(format!("run failed → {hint}"), true);
        }
        UiEventKind::RunCancelled { run_id } => {
            app.flush_stream_to_timeline();
            app.run_state = RunState::Idle;
            set_active_session_status(app, "cancelled");
            app.status_message = "cancelled".to_owned();
            app.push_item(
                TimelineItem::new(sequence, at, ItemKind::Notice, "run cancelled")
                    .with_body(format!("run_id: {run_id}")),
            );
        }
        UiEventKind::RunUnknown { run_id } => {
            app.flush_stream_to_timeline();
            app.run_state = RunState::Unknown;
            set_active_session_status(app, "unknown");
            app.status_message = "unknown outcome".to_owned();
            let hint = crate::model::remediation_hint("unknown outcome", None);
            push_or_coalesce_noise(
                app,
                TimelineItem::new(sequence, at, ItemKind::Error, "unknown outcome")
                    .with_body(format!(
                        "The client disconnected before the daemon could prove completion. Do not retry non-replayable work automatically.\n→ {hint}"
                    ))
                    .with_details(format!("run_id: {run_id}\nremediation: {hint}")),
            );
            app.show_toast(format!("unknown outcome → {hint}"), true);
        }
        UiEventKind::AgentChunk {
            run_id,
            chunk_id,
            text,
        } => {
            ingest_stream_chunk(app, run_id, sequence, at, chunk_id, text);
        }
        UiEventKind::AgentFinal { run_id, text } => {
            let key = run_id.to_string();
            if app.stream_run_id == Some(run_id) {
                let pending = app.stream_buffer.flush();
                crate::model::append_stream_body(app, &key, &pending);
                app.stream_run_id = None;
            }
            if let Some(item) = app
                .timeline
                .iter_mut()
                .rev()
                .find(|item| item.streaming_key.as_deref() == Some(key.as_str()))
            {
                item.body = bounded(text, crate::model::MAX_BODY_CHARS);
                item.streaming_key = None;
                item.sequence = sequence;
                item.at_unix_ms = at;
                item.details = format!("run_id: {run_id}\nfinal: true");
                app.last_sequence = sequence;
                app.dirty = true;
            } else {
                app.push_item(
                    TimelineItem::new(sequence, at, ItemKind::Assistant, "assistant")
                        .with_body(text)
                        .with_details(format!("run_id: {run_id}\nfinal: true")),
                );
            }
            close_tools_activity(app);
            close_reasoning_activity(app);
        }
        UiEventKind::ReasoningSummary { run_id, text } => {
            close_tools_activity(app);
            upsert_reasoning_activity(app, sequence, at, run_id, text);
        }
        UiEventKind::ChildStarted {
            child_id,
            role,
            parent_id,
        } => {
            close_tools_activity(app);
            close_reasoning_activity(app);
            let key = activity_child_key(&child_id);
            app.push_item(
                TimelineItem::new(sequence, at, ItemKind::Activity, format!("◆ {role}"))
                    .with_details(format!(
                        "child_id: {child_id}\nparent_id: {parent_id}\nrole: {role}"
                    ))
                    .collapsed(),
            );
            if let Some(item) = app.timeline.back_mut() {
                item.streaming_key = Some(key);
            }
        }
        UiEventKind::ChildStatus {
            child_id,
            status,
            current_action,
        } => {
            let key = activity_child_key(&child_id);
            if let Some(item) = find_activity_by_key_mut(app, &key) {
                item.sequence = sequence;
                item.at_unix_ms = at;
                let role = activity_role(item).unwrap_or_else(|| "child".to_owned());
                item.title = activity_title(&role, Some(&status), activity_step_count(item));
                if let Some(action) = current_action.filter(|a| !a.trim().is_empty()) {
                    append_activity_step(item, compact_action_line(&action), None);
                }
                app.last_sequence = sequence;
                app.dirty = true;
            } else {
                app.push_item(
                    TimelineItem::new(
                        sequence,
                        at,
                        ItemKind::Activity,
                        format!("◆ child {status}"),
                    )
                    .with_body(current_action.unwrap_or(child_id))
                    .collapsed(),
                );
            }
        }
        UiEventKind::ChildFinished {
            child_id,
            status,
            summary,
            error,
        } => {
            let key = activity_child_key(&child_id);
            if let Some(item) = find_activity_by_key_mut(app, &key) {
                item.sequence = sequence;
                item.at_unix_ms = at;
                let role = activity_role(item).unwrap_or_else(|| "child".to_owned());
                item.title = activity_title(&role, Some(&status), activity_step_count(item));
                if let Some(err) = error.filter(|e| !e.trim().is_empty()) {
                    append_activity_step(item, format!("error · {err}"), None);
                } else if let Some(summary) = summary.filter(|s| !s.trim().is_empty()) {
                    // Prefer short footer line only when no tool steps yet.
                    if activity_step_count(item) == 0 {
                        append_activity_step(item, truncate_one_line(&summary, 72), None);
                    } else {
                        item.details = format!("{}\nsummary: {summary}", item.details);
                    }
                }
                item.title = activity_title(&role, Some(&status), activity_step_count(item));
                item.streaming_key = None;
                item.collapsed = true;
                app.last_sequence = sequence;
                app.dirty = true;
            } else {
                app.push_item(
                    TimelineItem::new(
                        sequence,
                        at,
                        ItemKind::Activity,
                        activity_title("child", Some(&status), 0),
                    )
                    .with_body(error.or(summary).unwrap_or(child_id))
                    .collapsed(),
                );
            }
        }
        UiEventKind::ToolStarted { name } => {
            fold_tool_step(app, sequence, at, &name, "…", None, false);
        }
        UiEventKind::ToolFinished { name, summary } => {
            fold_tool_step(app, sequence, at, &name, &summary, None, false);
        }
        UiEventKind::ToolObserved {
            call_id,
            name,
            arguments,
            outcome,
            preview,
            artifact,
            error,
        } => {
            let is_error = error.is_some() || outcome.to_ascii_lowercase().contains("error");
            let label = if let Some(err) = &error {
                format!("{name} · {err}")
            } else if !preview.trim().is_empty() {
                format!("{name} · {}", truncate_one_line(&preview, 48))
            } else {
                format!("{name} · {outcome}")
            };
            let mut details =
                format!("call_id: {call_id}\noutcome: {outcome}\narguments:\n{arguments}");
            if let Some(artifact) = artifact {
                details.push_str(&format!("\nartifact: {artifact}"));
            }
            if let Some(error) = error {
                details.push_str(&format!("\nerror: {error}"));
            }
            fold_tool_step(app, sequence, at, &name, &label, Some(details), is_error);
        }
        UiEventKind::ToolDeferred {
            approval_id,
            call_id,
            name,
            arguments,
        } => {
            close_tools_activity(app);
            app.push_item(
                TimelineItem::new(
                    sequence,
                    at,
                    ItemKind::Approval,
                    format!("deferred · {name}"),
                )
                .with_body("Waiting for an exact user approval.")
                .with_details(format!(
                    "approval_id: {approval_id}\ncall_id: {call_id}\narguments:\n{arguments}"
                )),
            );
        }
        UiEventKind::ApprovalRequested { approval } => {
            app.run_state = RunState::WaitingApproval;
            app.push_item(
                TimelineItem::new(sequence, at, ItemKind::Approval, "approval requested")
                    .with_body(format!(
                        "{}\n{}",
                        approval.summary,
                        approval.target.clone().unwrap_or_default()
                    ))
                    .with_details(format!(
                        "id: {}\nkind: {}\nreason: {}\nfingerprint: {}",
                        approval.id, approval.action_kind, approval.reason, approval.fingerprint
                    )),
            );
            if !app
                .approval_queue
                .iter()
                .any(|pending| pending.id == approval.id)
            {
                app.approval_queue.push_back(approval);
            }
            if matches!(app.overlay, Overlay::None) {
                app.overlay = Overlay::Approval { selected: 0 };
            }
        }
        UiEventKind::ApprovalResolved {
            approval_id,
            accepted,
        } => {
            app.approval_queue
                .retain(|approval| approval.id != approval_id);
            app.push_item(
                TimelineItem::new(sequence, at, ItemKind::Approval, "approval resolved")
                    .with_body(if accepted {
                        "approved once"
                    } else {
                        "rejected"
                    })
                    .with_details(format!("approval_id: {approval_id}")),
            );
            if app.approval_queue.is_empty() {
                app.run_state = RunState::Working;
            }
        }
        UiEventKind::Backend {
            title,
            detail,
            healthy,
        } => {
            let kind = if healthy {
                ItemKind::Notice
            } else {
                ItemKind::Error
            };
            if healthy {
                push_or_coalesce_noise(
                    app,
                    TimelineItem::new(sequence, at, kind, title).with_body(detail),
                );
            } else {
                let hint = crate::model::remediation_hint(&title, None);
                push_or_coalesce_noise(
                    app,
                    TimelineItem::new(sequence, at, kind, title.clone())
                        .with_body(format!("{detail}\n→ {hint}"))
                        .with_details(format!("remediation: {hint}")),
                );
                app.show_toast(format!("{title} → {hint}"), true);
            }
        }
        UiEventKind::BudgetUpdated(budget) => {
            app.budget = budget;
            app.last_sequence = sequence;
            app.dirty = true;
        }
        UiEventKind::BudgetWarning { message } => {
            app.budget.warning = Some(message.clone());
            push_or_coalesce_noise(
                app,
                TimelineItem::new(sequence, at, ItemKind::Budget, "budget warning")
                    .with_body(message),
            );
        }
        UiEventKind::Notice {
            title,
            message,
            error,
            remediation,
        } => {
            let kind = if error {
                ItemKind::Error
            } else {
                ItemKind::Notice
            };
            let mut item = TimelineItem::new(sequence, at, kind, title.clone()).with_body(message);
            if error {
                let hint = crate::model::remediation_hint(&title, remediation.as_deref());
                item.body = format!("{}\n→ {hint}", item.body);
                item.details = format!("remediation: {hint}");
                app.show_toast(format!("{title} → {hint}"), true);
            }
            push_or_coalesce_noise(app, item);
        }
        UiEventKind::Retry {
            title,
            message,
            failed,
        } => {
            let kind = if failed {
                ItemKind::Error
            } else {
                ItemKind::Notice
            };
            let mut item = TimelineItem::new(sequence, at, kind, title.clone()).with_body(message);
            if failed {
                let hint = crate::model::remediation_hint(&title, None);
                item.body = format!("{}\n→ {hint}", item.body);
                item.details = format!("remediation: {hint}");
                app.show_toast(format!("{title} → {hint}"), true);
            }
            push_or_coalesce_noise(app, item);
        }
        UiEventKind::ActivityStep {
            label,
            detail,
            is_error,
        } => {
            let tool_name = label.split(['·', ' ']).next().unwrap_or("step").trim();
            fold_tool_step(app, sequence, at, tool_name, &label, detail, is_error);
        }
    }
}

fn set_active_session_status(app: &mut AppState, status: &str) {
    if let Some(active) = app.active_session
        && let Some(session) = app.sessions.iter_mut().find(|session| session.id == active)
    {
        session.status = status.to_owned();
    }
}

const ACTIVITY_TOOLS_KEY: &str = "activity:tools";
const ACTIVITY_REASONING_KEY: &str = "activity:reasoning";

fn activity_child_key(child_id: &str) -> String {
    format!("activity:child:{child_id}")
}

fn activity_title(label: &str, status: Option<&str>, steps: usize) -> String {
    let head = match status {
        Some(status) if !status.is_empty() => format!("◆ {label} {status}"),
        _ => format!("◆ {label}"),
    };
    if steps == 0 {
        head
    } else {
        format!("{head} · {steps}")
    }
}

fn activity_role(item: &TimelineItem) -> Option<String> {
    item.details.lines().find_map(|line| {
        line.strip_prefix("role: ")
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
    })
}

fn parse_activity_steps(body: &str) -> Vec<String> {
    body.lines()
        .filter_map(|line| {
            let trimmed = line.trim();
            trimmed
                .strip_prefix("├ ")
                .or_else(|| trimmed.strip_prefix("└ "))
                .or(if trimmed.is_empty() {
                    None
                } else {
                    Some(trimmed)
                })
                .map(str::to_owned)
        })
        .collect()
}

fn format_activity_tree(steps: &[String]) -> String {
    let last = steps.len().saturating_sub(1);
    steps
        .iter()
        .enumerate()
        .map(|(index, step)| {
            let prefix = if index == last { "└ " } else { "├ " };
            format!("{prefix}{step}")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub(super) fn activity_step_count(item: &TimelineItem) -> usize {
    parse_activity_steps(&item.body).len()
}

fn refresh_activity_title(item: &mut TimelineItem, status: Option<&str>) {
    let label = if item.streaming_key.as_deref() == Some(ACTIVITY_TOOLS_KEY)
        || item.details.starts_with("bucket: tools")
    {
        "tools".to_owned()
    } else if item.streaming_key.as_deref() == Some(ACTIVITY_REASONING_KEY)
        || item.details.starts_with("bucket: reasoning")
    {
        "reasoning".to_owned()
    } else {
        activity_role(item).unwrap_or_else(|| "child".to_owned())
    };
    item.title = activity_title(&label, status, activity_step_count(item));
}

fn bump_activity_step_count(item: &mut TimelineItem) {
    let steps = activity_step_count(item);
    let base = item
        .title
        .rsplit_once(" · ")
        .filter(|(_, count)| count.chars().all(|ch| ch.is_ascii_digit()))
        .map(|(head, _)| head)
        .unwrap_or(item.title.as_str());
    item.title = if steps == 0 {
        base.to_owned()
    } else {
        format!("{base} · {steps}")
    };
}

fn append_activity_step(item: &mut TimelineItem, step: impl Into<String>, detail: Option<String>) {
    let step = step.into();
    let mut steps = parse_activity_steps(&item.body);
    steps.push(step);
    item.body = format_activity_tree(&steps);
    if let Some(detail) = detail {
        if item.details.is_empty() {
            item.details = detail;
        } else {
            item.details = format!("{}\n---\n{detail}", item.details);
        }
    }
    bump_activity_step_count(item);
}

fn upsert_activity_step(
    item: &mut TimelineItem,
    tool_name: &str,
    label: &str,
    detail: Option<String>,
) {
    let mut steps = parse_activity_steps(&item.body);
    let replace = steps.last().is_some_and(|last| {
        last == tool_name
            || last.starts_with(&format!("{tool_name} ·"))
            || last.starts_with(&format!("{tool_name} "))
            || last == "…"
            || last.ends_with(" …")
                && last
                    .strip_suffix(" …")
                    .is_some_and(|prefix| prefix == tool_name)
    });
    if replace {
        if let Some(last) = steps.last_mut() {
            *last = label.to_owned();
        }
    } else {
        steps.push(label.to_owned());
    }
    item.body = format_activity_tree(&steps);
    if let Some(detail) = detail {
        if item.details.is_empty() {
            item.details = detail;
        } else {
            item.details = format!("{}\n---\n{detail}", item.details);
        }
    }
    bump_activity_step_count(item);
}

fn find_activity_by_key_mut<'a>(app: &'a mut AppState, key: &str) -> Option<&'a mut TimelineItem> {
    app.timeline
        .iter_mut()
        .rev()
        .find(|item| item.kind == ItemKind::Activity && item.streaming_key.as_deref() == Some(key))
}

fn open_child_activity_mut(app: &mut AppState) -> Option<&mut TimelineItem> {
    app.timeline.iter_mut().rev().find(|item| {
        item.kind == ItemKind::Activity
            && item
                .streaming_key
                .as_deref()
                .is_some_and(|key| key.starts_with("activity:child:"))
    })
}

fn close_tools_activity(app: &mut AppState) {
    if let Some(item) = find_activity_by_key_mut(app, ACTIVITY_TOOLS_KEY) {
        item.streaming_key = None;
        item.collapsed = true;
        refresh_activity_title(item, Some("done"));
    }
}

fn close_reasoning_activity(app: &mut AppState) {
    if let Some(item) = find_activity_by_key_mut(app, ACTIVITY_REASONING_KEY) {
        item.streaming_key = None;
        item.collapsed = true;
    }
}

fn truncate_one_line(text: &str, max_chars: usize) -> String {
    let flat = text.lines().next().unwrap_or(text).trim();
    if flat.chars().count() <= max_chars {
        flat.to_owned()
    } else {
        let end = flat
            .char_indices()
            .nth(max_chars.saturating_sub(1))
            .map(|(i, _)| i)
            .unwrap_or(flat.len());
        format!("{}…", &flat[..end])
    }
}

fn compact_action_line(action: &str) -> String {
    truncate_one_line(action, 64)
}

fn fold_tool_step(
    app: &mut AppState,
    sequence: u64,
    at: u64,
    tool_name: &str,
    label: &str,
    detail: Option<String>,
    is_error: bool,
) {
    let label = if label == "…" {
        format!("{tool_name} …")
    } else if label.starts_with(tool_name) {
        label.to_owned()
    } else {
        format!("{tool_name} · {label}")
    };

    if let Some(item) = open_child_activity_mut(app) {
        item.sequence = sequence;
        item.at_unix_ms = at;
        upsert_activity_step(item, tool_name, &label, detail);
        if is_error {
            item.details = format!("{}\noutcome: error", item.details);
        }
        app.last_sequence = sequence;
        app.dirty = true;
        return;
    }

    if find_activity_by_key_mut(app, ACTIVITY_TOOLS_KEY).is_none() {
        app.push_item(
            TimelineItem::new(sequence, at, ItemKind::Activity, "◆ tools")
                .with_details("bucket: tools")
                .collapsed(),
        );
        if let Some(item) = app.timeline.back_mut() {
            item.streaming_key = Some(ACTIVITY_TOOLS_KEY.to_owned());
        }
    }
    if let Some(item) = find_activity_by_key_mut(app, ACTIVITY_TOOLS_KEY) {
        item.sequence = sequence;
        item.at_unix_ms = at;
        upsert_activity_step(item, tool_name, &label, detail);
        if is_error {
            item.details = format!("{}\noutcome: error", item.details);
        }
        app.last_sequence = sequence;
        app.dirty = true;
    }
}

fn upsert_reasoning_activity(
    app: &mut AppState,
    sequence: u64,
    at: u64,
    run_id: Uuid,
    text: String,
) {
    let line = truncate_one_line(&text, 72);
    if let Some(item) = find_activity_by_key_mut(app, ACTIVITY_REASONING_KEY) {
        item.sequence = sequence;
        item.at_unix_ms = at;
        // Coalesce: keep latest summary as single step (not every delta).
        item.body = format_activity_tree(&[line]);
        item.details = format!("run_id: {run_id}\n{text}");
        item.collapsed = true;
        refresh_activity_title(item, None);
        app.last_sequence = sequence;
        app.dirty = true;
        return;
    }
    app.push_item(
        TimelineItem::new(sequence, at, ItemKind::Activity, "◆ reasoning")
            .with_body(format_activity_tree(&[line]))
            .with_details(format!("bucket: reasoning\nrun_id: {run_id}\n{text}"))
            .collapsed(),
    );
    if let Some(item) = app.timeline.back_mut() {
        item.streaming_key = Some(ACTIVITY_REASONING_KEY.to_owned());
    }
}

/// Collapse consecutive same-title Notice/Error/Budget noise into one timeline row.
fn push_or_coalesce_noise(app: &mut AppState, item: TimelineItem) {
    let coalesce = matches!(
        item.kind,
        ItemKind::Notice | ItemKind::Error | ItemKind::Budget
    );
    if coalesce
        && let Some(last) = app.timeline.back_mut()
        && last.kind == item.kind
        && last.title == item.title
        && last.streaming_key.is_none()
    {
        last.sequence = item.sequence;
        last.at_unix_ms = item.at_unix_ms;
        last.body = item.body;
        last.details = item.details;
        app.last_sequence = app.last_sequence.max(last.sequence);
        if app.follow_tail {
            app.line_scroll_from_bottom = 0;
            app.selected_item = app.timeline.len().checked_sub(1);
        }
        app.dirty = true;
        return;
    }
    app.push_item(item);
}

fn strip_mode_prefix(text: &str) -> String {
    if text.starts_with("[Impetus UI mode:") {
        text.split_once("\n\n")
            .map(|(_, body)| body.to_owned())
            .unwrap_or_else(|| text.to_owned())
    } else {
        text.to_owned()
    }
}
