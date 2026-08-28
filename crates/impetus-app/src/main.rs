use gpui::{
    AnyElement, App, Bounds, Context, FocusHandle, IntoElement, KeyBinding, Render, SharedString,
    Window, WindowBounds, WindowOptions, actions, div, prelude::*, px, rgb, size,
};
use gpui_platform::application;
use impetus_core::{
    CapabilityRegistry, CiBackend, CiProject, Job, JobStatus, LocalCiEvent, LocalGitlabBackend,
    Pipeline, RemoteGitlabBackend,
};
use std::{collections::VecDeque, time::Duration};
use tracing_subscriber::EnvFilter;

mod terminal_themes;

use terminal_themes::{STANDARD_TERMINAL_THEMES, TerminalTheme, theme_by_id};

actions!(
    ci_actions,
    [
        CiRunLocal,
        CiRefreshRemote,
        CiSelectNext,
        CiSelectPrevious,
        CiShowDetails,
        CiShowFullLog,
        CiClose,
    ]
);

const MAX_VISIBLE_CI_LOG_LINES: usize = 600;
const MAX_CI_OUTPUT_PREVIEW_LINES: usize = 12;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CiSource {
    Local,
    Remote,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CiDetails {
    Hidden,
    Summary,
    FullLog,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CiOutputTone {
    Normal,
    Muted,
    Accent,
    Success,
    Warning,
    Failure,
}

struct AgenticTerminalView {
    headline: SharedString,
    status: SharedString,
    selected_theme_id: &'static str,
    focus_handle: FocusHandle,
    ci_visible: bool,
    ci_source: Option<CiSource>,
    ci_project: Option<CiProject>,
    ci_pipeline: Option<Pipeline>,
    ci_status: SharedString,
    ci_log: VecDeque<String>,
    ci_log_was_trimmed: bool,
    selected_ci_job: usize,
    ci_details: CiDetails,
}

impl AgenticTerminalView {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let capability_count =
            CapabilityRegistry::from_json(include_str!("../../../config/capabilities.json"))
                .expect("validate bundled capability catalog")
                .all()
                .count();
        let focus_handle = cx.focus_handle();
        window.focus(&focus_handle, cx);
        Self {
            headline: "Impetus — local-first".into(),
            status: format!(
                "Client-only preview · no harness state ownership · {capability_count} planned capabilities"
            )
            .into(),
            selected_theme_id: "dracula",
            focus_handle,
            ci_visible: true,
            ci_source: None,
            ci_project: None,
            ci_pipeline: None,
            ci_status: "CI is idle · Local run or remote status".into(),
            ci_log: VecDeque::new(),
            ci_log_was_trimmed: false,
            selected_ci_job: 0,
            ci_details: CiDetails::Hidden,
        }
    }

    fn selected_theme(&self) -> TerminalTheme {
        theme_by_id(self.selected_theme_id).expect("default terminal theme is registered")
    }

    fn select_theme(&mut self, theme_id: &'static str, cx: &mut Context<Self>) {
        self.selected_theme_id = theme_id;
        cx.notify();
    }

    fn start_local(&mut self, cx: &mut Context<Self>) {
        // Explicit GPUI button action outside a harness task. This client-only
        // experiment owns neither policy nor durable session state.
        self.reset_ci(CiSource::Local, "Preparing gitlab-ci-local…");
        let workspace = match std::env::current_dir() {
            Ok(workspace) => workspace,
            Err(error) => {
                self.ci_status = format!("CI is unavailable: {error}").into();
                return;
            }
        };
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { LocalGitlabBackend.start(&workspace) })
                .await;
            let run = match result {
                Ok(run) => run,
                Err(error) => {
                    let _ = this.update(cx, |view, cx| {
                        view.ci_status = format!("Local CI is unavailable: {error}").into();
                        cx.notify();
                    });
                    return;
                }
            };
            let initial_pipeline = run.pipeline.clone();
            if this
                .update(cx, |view, cx| {
                    view.ci_pipeline = Some(initial_pipeline);
                    view.ci_status = "Local pipeline is running".into();
                    cx.notify();
                })
                .is_err()
            {
                return;
            }

            let mut completed = false;
            while !completed {
                while let Ok(Some(event)) = run.try_next() {
                    completed = matches!(
                        event,
                        LocalCiEvent::Completed { .. } | LocalCiEvent::Failed(_)
                    );
                    if this
                        .update(cx, |view, cx| {
                            view.apply_local_ci_event(event, cx);
                        })
                        .is_err()
                    {
                        return;
                    }
                }
                if !completed {
                    cx.background_executor()
                        .timer(Duration::from_millis(80))
                        .await;
                }
            }
        })
        .detach();
    }

    fn refresh_remote(&mut self, cx: &mut Context<Self>) {
        // Explicit GPUI button action outside a harness task; `glab` owns its
        // own authentication and the result is not a harness audit event.
        self.reset_ci(CiSource::Remote, "Reading GitLab status through glab…");
        let workspace = match std::env::current_dir() {
            Ok(workspace) => workspace,
            Err(error) => {
                self.ci_status = format!("CI is unavailable: {error}").into();
                return;
            }
        };
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let backend = RemoteGitlabBackend;
                    let project = backend.detect(&workspace)?;
                    let pipeline = backend.status(&project)?;
                    Ok::<_, impetus_core::CiError>((project, pipeline))
                })
                .await;
            let _ = this.update(cx, |view, cx| match result {
                Ok((project, pipeline)) => {
                    view.ci_project = Some(project);
                    view.ci_status = "GitLab pipeline status loaded".into();
                    view.ci_pipeline = Some(pipeline);
                    cx.notify();
                }
                Err(error) => {
                    view.ci_status = format!("Remote CI is unavailable: {error}").into();
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn reset_ci(&mut self, source: CiSource, status: &str) {
        self.ci_visible = true;
        self.ci_source = Some(source);
        self.ci_project = None;
        self.ci_pipeline = None;
        self.ci_status = status.into();
        self.ci_log.clear();
        self.ci_log_was_trimmed = false;
        self.selected_ci_job = 0;
        self.ci_details = CiDetails::Hidden;
    }

    fn apply_local_ci_event(&mut self, event: LocalCiEvent, cx: &mut Context<Self>) {
        match event {
            LocalCiEvent::Output(line) => {
                self.append_ci_log(line.clone());
                if let Some(pipeline) = &mut self.ci_pipeline {
                    impetus_core::ci::apply_local_output(pipeline, &line);
                }
            }
            LocalCiEvent::Completed {
                succeeded,
                exit_code,
                duration,
            } => {
                let log = self.full_ci_log();
                if let Some(pipeline) = &mut self.ci_pipeline {
                    pipeline.duration = Some(duration);
                    impetus_core::ci::finalize_local_run(pipeline, succeeded, exit_code, &log);
                    self.ci_status = if succeeded {
                        "Local pipeline completed".into()
                    } else {
                        "Local pipeline failed · press Enter for the error".into()
                    };
                }
            }
            LocalCiEvent::Failed(message) => {
                self.ci_status = format!("Local CI failed to start: {message}").into();
            }
        }
        cx.notify();
    }

    fn append_ci_log(&mut self, line: String) {
        if self.ci_log.len() == MAX_VISIBLE_CI_LOG_LINES {
            self.ci_log.pop_front();
            self.ci_log_was_trimmed = true;
        }
        self.ci_log.push_back(line);
    }

    fn full_ci_log(&self) -> String {
        self.ci_log.iter().cloned().collect::<Vec<_>>().join("\n")
    }

    fn select_ci_job(&mut self, index: usize, cx: &mut Context<Self>) {
        if index < self.ci_job_count() {
            self.selected_ci_job = index;
            self.ci_details = CiDetails::Hidden;
            cx.notify();
        }
    }

    fn select_next_ci_job(&mut self, cx: &mut Context<Self>) {
        let count = self.ci_job_count();
        if count > 0 {
            self.selected_ci_job = (self.selected_ci_job + 1).min(count - 1);
            self.ci_details = CiDetails::Hidden;
            cx.notify();
        }
    }

    fn select_previous_ci_job(&mut self, cx: &mut Context<Self>) {
        if self.selected_ci_job > 0 {
            self.selected_ci_job -= 1;
            self.ci_details = CiDetails::Hidden;
            cx.notify();
        }
    }

    fn show_ci_details(&mut self, cx: &mut Context<Self>) {
        if self.selected_ci_job().is_none() {
            return;
        }
        self.ci_details = match self.ci_details {
            CiDetails::Hidden => CiDetails::Summary,
            CiDetails::Summary | CiDetails::FullLog => CiDetails::FullLog,
        };
        if self.ci_details == CiDetails::FullLog && self.ci_source == Some(CiSource::Remote) {
            self.load_remote_log(cx);
        }
        cx.notify();
    }

    fn show_full_ci_log(&mut self, cx: &mut Context<Self>) {
        self.ci_details = CiDetails::FullLog;
        if self.ci_source == Some(CiSource::Remote) {
            self.load_remote_log(cx);
        }
        cx.notify();
    }

    fn load_remote_log(&mut self, cx: &mut Context<Self>) {
        let Some(project) = self.ci_project.clone() else {
            return;
        };
        let Some(job_id) = self.selected_ci_job().and_then(|job| job.id.clone()) else {
            return;
        };
        if self
            .selected_ci_job()
            .and_then(|job| job.log.as_ref())
            .is_some()
        {
            return;
        }
        self.ci_status = "Loading selected job log…".into();
        let selected = self.selected_ci_job;
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { RemoteGitlabBackend.logs(&project, &job_id) })
                .await;
            let _ = this.update(cx, |view, cx| match result {
                Ok(log) => {
                    if let Some(job) = view.selected_ci_job_mut_at(selected) {
                        job.error_summary = impetus_core::ci::extract_error_summary(&log);
                        job.log = Some(log);
                    }
                    view.ci_status = "Selected job log loaded".into();
                    cx.notify();
                }
                Err(error) => {
                    view.ci_status = format!("Could not load the job log: {error}").into();
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn ci_job_count(&self) -> usize {
        self.ci_pipeline
            .as_ref()
            .map(|pipeline| pipeline.stages.iter().map(|stage| stage.jobs.len()).sum())
            .unwrap_or_default()
    }

    fn selected_ci_job(&self) -> Option<&Job> {
        self.ci_pipeline
            .as_ref()?
            .stages
            .iter()
            .flat_map(|stage| stage.jobs.iter())
            .nth(self.selected_ci_job)
    }

    fn selected_ci_job_mut_at(&mut self, selected: usize) -> Option<&mut Job> {
        self.ci_pipeline
            .as_mut()?
            .stages
            .iter_mut()
            .flat_map(|stage| stage.jobs.iter_mut())
            .nth(selected)
    }
}

impl Render for AgenticTerminalView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.selected_theme();
        let theme_choices = STANDARD_TERMINAL_THEMES
            .into_iter()
            .map(|choice| {
                let is_selected = choice.id == theme.id;
                let border = if is_selected {
                    choice.cursor
                } else {
                    choice.selection
                };
                div()
                    .id(choice.id)
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .border_1()
                    .border_color(rgb(border))
                    .bg(rgb(choice.background))
                    .text_color(rgb(choice.foreground))
                    .text_sm()
                    .cursor_pointer()
                    .hover(move |style| style.bg(rgb(choice.selection)))
                    .child(format!("{} · {}", choice.name, choice.appearance.label()))
                    .on_click(cx.listener(move |this, _, _, cx| this.select_theme(choice.id, cx)))
            })
            .collect::<Vec<_>>();
        let ci_panel = self.render_ci_panel(theme, cx);

        div()
            .track_focus(&self.focus_handle)
            .key_context("AgenticTerminal")
            .on_action(cx.listener(|this, _: &CiRunLocal, _, cx| this.start_local(cx)))
            .on_action(cx.listener(|this, _: &CiRefreshRemote, _, cx| this.refresh_remote(cx)))
            .on_action(cx.listener(|this, _: &CiSelectNext, _, cx| this.select_next_ci_job(cx)))
            .on_action(cx.listener(|this, _: &CiSelectPrevious, _, cx| {
                this.select_previous_ci_job(cx)
            }))
            .on_action(cx.listener(|this, _: &CiShowDetails, _, cx| this.show_ci_details(cx)))
            .on_action(cx.listener(|this, _: &CiShowFullLog, _, cx| this.show_full_ci_log(cx)))
            .on_action(cx.listener(|this, _: &CiClose, _, cx| {
                this.ci_visible = false;
                cx.notify();
            }))
            .flex()
            .flex_col()
            .gap_3()
            .bg(rgb(theme.background))
            .size(px(980.0))
            .p_6()
            .text_color(rgb(theme.foreground))
            .child(div().text_xl().child(self.headline.clone()))
            .child(div().text_color(rgb(theme.ansi[6])).child(self.status.clone()))
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap_2()
                    .children(theme_choices),
            )
            .child(
                div()
                    .bg(rgb(theme.selection))
                    .p_4()
                    .text_color(rgb(theme.foreground))
                    .child("$ harness input arrives after client IPC wiring"),
            )
            .child(
                div()
                    .bg(rgb(theme.ansi[0]))
                    .p_4()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .text_color(rgb(theme.foreground))
                    .child(format!("Terminal preview · {}", theme.name))
                    .child("$ printf 'ANSI palette'")
                    .child(
                        div()
                            .flex()
                            .gap_2()
                            .child(div().text_color(rgb(theme.ansi[1])).child("red"))
                            .child(div().text_color(rgb(theme.ansi[2])).child("green"))
                            .child(div().text_color(rgb(theme.ansi[4])).child("blue")),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(rgb(theme.ansi[8]))
                            .child("PTY renderer, agent stream and approval card land in separately owned phases."),
                    ),
            )
            .child(ci_panel)
            .child(
                div()
                    .text_sm()
                    .text_color(rgb(theme.ansi[8]))
                    .child("session: connect through harness client IPC"),
            )
    }
}

impl AgenticTerminalView {
    fn render_ci_panel(&self, theme: TerminalTheme, cx: &mut Context<Self>) -> AnyElement {
        if !self.ci_visible {
            return div()
                .id("open-ci-panel")
                .px_3()
                .py_2()
                .rounded_md()
                .bg(rgb(theme.selection))
                .text_color(rgb(theme.foreground))
                .cursor_pointer()
                .child("CI · hidden — click to reopen")
                .on_click(cx.listener(|this, _, _, cx| {
                    this.ci_visible = true;
                    cx.notify();
                }))
                .into_any_element();
        }

        let mut rows = Vec::<AnyElement>::new();
        if let Some(pipeline) = &self.ci_pipeline {
            rows.push(
                div()
                    .text_sm()
                    .text_color(rgb(theme.ansi[6]))
                    .child(pipeline_heading(pipeline))
                    .into_any_element(),
            );
            let mut index = 0;
            for stage in &pipeline.stages {
                rows.push(
                    div()
                        .mt_2()
                        .text_xs()
                        .text_color(rgb(theme.ansi[8]))
                        .child(stage.name.to_uppercase())
                        .into_any_element(),
                );
                for job in &stage.jobs {
                    let selected = index == self.selected_ci_job;
                    let job_index = index;
                    index += 1;
                    let background = if selected {
                        theme.selection
                    } else {
                        theme.background
                    };
                    rows.push(
                        div()
                            .id(format!("ci-job-{job_index}"))
                            .flex()
                            .justify_between()
                            .px_2()
                            .py_1()
                            .rounded_sm()
                            .bg(rgb(background))
                            .text_color(rgb(ci_job_color(job.status, theme)))
                            .cursor_pointer()
                            .child(format!(
                                "{}{} {}",
                                if selected { "> " } else { "  " },
                                ci_job_icon(job.status),
                                job.name
                            ))
                            .child(format_duration(job.duration))
                            .on_click(
                                cx.listener(move |this, _, _, cx| {
                                    this.select_ci_job(job_index, cx)
                                }),
                            )
                            .into_any_element(),
                    );
                }
            }
            rows.push(
                div()
                    .mt_2()
                    .text_sm()
                    .text_color(rgb(theme.ansi[8]))
                    .child(pipeline_summary(pipeline))
                    .into_any_element(),
            );
        } else {
            rows.push(
                div()
                    .text_sm()
                    .text_color(rgb(theme.ansi[8]))
                    .child(
                        "No CI result yet. The project configuration is never copied or rewritten.",
                    )
                    .into_any_element(),
            );
        }

        let details = self.render_ci_details(theme);
        div()
            .id("ci-panel")
            .bg(rgb(theme.ansi[0]))
            .p_4()
            .flex()
            .flex_col()
            .gap_2()
            .text_color(rgb(theme.foreground))
            .child(
                div()
                    .flex()
                    .justify_between()
                    .items_center()
                    .child(div().text_lg().child("CI"))
                    .child(
                        div()
                            .flex()
                            .gap_2()
                            .child(ci_button(
                                "Run local",
                                theme,
                                cx.listener(|this, _, _, cx| this.start_local(cx)),
                            ))
                            .child(ci_button(
                                "Remote status",
                                theme,
                                cx.listener(|this, _, _, cx| this.refresh_remote(cx)),
                            )),
                    ),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(rgb(theme.ansi[6]))
                    .child(self.ci_status.clone()),
            )
            .children(rows)
            .child(self.render_ci_output_preview(theme))
            .child(details)
            .child(
                div()
                    .mt_2()
                    .text_xs()
                    .text_color(rgb(theme.ansi[8]))
                    .child("↑↓ select · Enter details/full log · r refresh · l full log · q close"),
            )
            .into_any_element()
    }

    fn render_ci_details(&self, theme: TerminalTheme) -> AnyElement {
        let Some(job) = self.selected_ci_job() else {
            return div().into_any_element();
        };
        match self.ci_details {
            CiDetails::Hidden => div().into_any_element(),
            CiDetails::Summary => div()
                .mt_2()
                .p_3()
                .rounded_md()
                .bg(rgb(theme.selection))
                .text_sm()
                .text_color(rgb(theme.foreground))
                .child(format!("▼ {}", job.name))
                .child(
                    job.error_summary
                        .clone()
                        .unwrap_or_else(|| "No compact error fragment is available yet.".into()),
                )
                .into_any_element(),
            CiDetails::FullLog => {
                let log = job.log.clone().unwrap_or_else(|| self.full_ci_log());
                let log = if log.is_empty() {
                    "The selected job has not emitted a log yet.".into()
                } else if self.ci_log_was_trimmed && self.ci_source == Some(CiSource::Local) {
                    format!(
                        "Visible local log is bounded to {MAX_VISIBLE_CI_LOG_LINES} lines.\n\n{log}"
                    )
                } else {
                    log
                };
                div()
                    .mt_2()
                    .p_3()
                    .rounded_md()
                    .bg(rgb(theme.selection))
                    .text_sm()
                    .text_color(rgb(theme.foreground))
                    .child(format!("▼ {} · raw log", job.name))
                    .child(log)
                    .into_any_element()
            }
        }
    }

    fn render_ci_output_preview(&self, theme: TerminalTheme) -> AnyElement {
        let mut lines = Vec::<AnyElement>::new();
        if self.ci_log.is_empty() {
            lines.push(
                div()
                    .text_xs()
                    .text_color(rgb(theme.ansi[8]))
                    .child("Live command output will appear here.")
                    .into_any_element(),
            );
        } else {
            if self.ci_log_was_trimmed {
                lines.push(
                    div()
                        .text_xs()
                        .text_color(rgb(theme.ansi[8]))
                        .child(format!(
                            "… older output omitted · keeping last {MAX_VISIBLE_CI_LOG_LINES} lines"
                        ))
                        .into_any_element(),
                );
            }
            let start = self
                .ci_log
                .len()
                .saturating_sub(MAX_CI_OUTPUT_PREVIEW_LINES);
            lines.extend(
                self.ci_log
                    .iter()
                    .skip(start)
                    .enumerate()
                    .map(|(index, line)| {
                        div()
                            .id(format!("ci-output-line-{index}"))
                            .text_xs()
                            .text_color(rgb(ci_output_color(ci_output_tone(line), theme)))
                            .child(line.clone())
                            .into_any_element()
                    }),
            );
        }

        div()
            .id("ci-output-preview")
            .mt_2()
            .p_3()
            .rounded_md()
            .bg(rgb(theme.background))
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(theme.ansi[8]))
                    .child("LIVE OUTPUT · command / success / warning / error"),
            )
            .children(lines)
            .into_any_element()
    }
}

fn ci_button(
    label: &'static str,
    theme: TerminalTheme,
    on_click: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    div()
        .id(label)
        .px_2()
        .py_1()
        .rounded_sm()
        .bg(rgb(theme.selection))
        .text_sm()
        .text_color(rgb(theme.foreground))
        .cursor_pointer()
        .hover(move |style| style.bg(rgb(theme.cursor)))
        .child(label)
        .on_click(on_click)
}

fn ci_job_icon(status: JobStatus) -> &'static str {
    match status {
        JobStatus::Success => "✓",
        JobStatus::Failed => "✕",
        JobStatus::Running => "●",
        JobStatus::Pending => "○",
        JobStatus::Skipped => "-",
        JobStatus::Canceled => "⊘",
        JobStatus::Unknown => "?",
    }
}

fn ci_job_color(status: JobStatus, theme: TerminalTheme) -> u32 {
    match status {
        JobStatus::Success => theme.ansi[2],
        JobStatus::Failed => theme.ansi[1],
        JobStatus::Running => theme.ansi[6],
        JobStatus::Pending | JobStatus::Skipped | JobStatus::Canceled | JobStatus::Unknown => {
            theme.foreground
        }
    }
}

fn ci_output_tone(line: &str) -> CiOutputTone {
    let normalized = line.trim().to_ascii_lowercase();
    if normalized.is_empty() {
        CiOutputTone::Muted
    } else if normalized.contains("error")
        || normalized.contains("fatal")
        || normalized.contains("panic")
        || normalized.starts_with("fail ")
        || normalized.contains(" failed")
    {
        CiOutputTone::Failure
    } else if normalized.contains("warning") || normalized.contains("warn ") {
        CiOutputTone::Warning
    } else if normalized.contains("success")
        || normalized.contains("passed")
        || normalized.starts_with("pass ")
        || normalized.contains(" completed")
    {
        CiOutputTone::Success
    } else if normalized.starts_with('$')
        || normalized.starts_with('>')
        || normalized.contains("running")
    {
        CiOutputTone::Accent
    } else {
        CiOutputTone::Normal
    }
}

fn ci_output_color(tone: CiOutputTone, theme: TerminalTheme) -> u32 {
    match tone {
        CiOutputTone::Normal => theme.foreground,
        CiOutputTone::Muted => theme.ansi[8],
        CiOutputTone::Accent => theme.ansi[6],
        CiOutputTone::Success => theme.ansi[2],
        CiOutputTone::Warning => theme.ansi[3],
        CiOutputTone::Failure => theme.ansi[1],
    }
}

fn format_duration(duration: Option<Duration>) -> String {
    let Some(duration) = duration else {
        return String::new();
    };
    format!("{:.1}s", duration.as_secs_f64())
}

fn pipeline_heading(pipeline: &Pipeline) -> String {
    let source = pipeline
        .id
        .as_deref()
        .map(|id| format!("pipeline #{id}"))
        .unwrap_or_else(|| "local pipeline".into());
    format!("CI · {source} · {}", pipeline.branch)
}

fn pipeline_summary(pipeline: &Pipeline) -> String {
    let mut passed = 0;
    let mut running = 0;
    let mut waiting = 0;
    let mut failed = 0;
    for job in pipeline.stages.iter().flat_map(|stage| stage.jobs.iter()) {
        match job.status {
            JobStatus::Success => passed += 1,
            JobStatus::Running => running += 1,
            JobStatus::Pending | JobStatus::Unknown => waiting += 1,
            JobStatus::Failed => failed += 1,
            JobStatus::Skipped | JobStatus::Canceled => {}
        }
    }
    if failed > 0 {
        format!("{passed} passed · {failed} failed · {running} running · {waiting} waiting")
    } else {
        format!("{passed} passed · {running} running · {waiting} waiting")
    }
}

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();
    application().run(|cx: &mut App| {
        cx.bind_keys([
            KeyBinding::new("up", CiSelectPrevious, Some("AgenticTerminal")),
            KeyBinding::new("down", CiSelectNext, Some("AgenticTerminal")),
            KeyBinding::new("enter", CiShowDetails, Some("AgenticTerminal")),
            KeyBinding::new("r", CiRefreshRemote, Some("AgenticTerminal")),
            KeyBinding::new("l", CiShowFullLog, Some("AgenticTerminal")),
            KeyBinding::new("q", CiClose, Some("AgenticTerminal")),
        ]);
        let bounds = Bounds::centered(None, size(px(980.0), px(680.0)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            |window, cx| cx.new(|cx| AgenticTerminalView::new(window, cx)),
        )
        .expect("open macOS window");
        cx.activate(true);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ci_output_tone_keeps_semantic_log_states_readable() {
        assert_eq!(
            ci_output_tone("$ cargo test --workspace"),
            CiOutputTone::Accent
        );
        assert_eq!(ci_output_tone("test result: ok"), CiOutputTone::Normal);
        assert_eq!(
            ci_output_tone("warning: unused import"),
            CiOutputTone::Warning
        );
        assert_eq!(ci_output_tone("error: build failed"), CiOutputTone::Failure);
        assert_eq!(
            ci_output_tone("pipeline completed successfully"),
            CiOutputTone::Success
        );
    }
}
