use agentic_terminal_core::{
    AgentRuntime, CapabilityRegistry, EventStore, PolicyEngine, SandboxScope, SqliteEventStore,
};
use gpui::{
    App, Bounds, Context, IntoElement, Render, SharedString, Window, WindowBounds, WindowOptions,
    div, prelude::*, px, rgb, size,
};
use gpui_platform::application;
use std::{path::PathBuf, sync::Arc};
use tracing_subscriber::EnvFilter;

mod terminal_themes;

use terminal_themes::{STANDARD_TERMINAL_THEMES, TerminalTheme, theme_by_id};

struct AgenticTerminalView {
    headline: SharedString,
    status: SharedString,
    runtime: AgentRuntime,
    selected_theme_id: &'static str,
}

impl AgenticTerminalView {
    fn new() -> Self {
        let capability_count =
            CapabilityRegistry::from_json(include_str!("../../../config/capabilities.json"))
                .expect("validate bundled capability catalog")
                .all()
                .count();
        let runtime = AgentRuntime::new(
            open_event_store(),
            PolicyEngine::new(SandboxScope::local_workspace(".")),
        );
        Self {
            headline: "Agentic Terminal — local-first".into(),
            status: format!(
                "Durable runtime online · policy gate enabled · {capability_count} planned capabilities"
            )
            .into(),
            runtime,
            selected_theme_id: "dracula",
        }
    }

    fn selected_theme(&self) -> TerminalTheme {
        theme_by_id(self.selected_theme_id).expect("default terminal theme is registered")
    }

    fn select_theme(&mut self, theme_id: &'static str, cx: &mut Context<Self>) {
        self.selected_theme_id = theme_id;
        cx.notify();
    }
}

fn open_event_store() -> Arc<dyn EventStore> {
    let data_root = std::env::var_os("AGENTIC_TERMINAL_DATA_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").expect("HOME is set on macOS"))
                .join("Library/Application Support/Agentic Terminal")
        });
    std::fs::create_dir_all(&data_root).expect("create Agentic Terminal data directory");
    SqliteEventStore::open(data_root.join("events.sqlite3"))
        .expect("open durable SQLite event store")
}

impl Render for AgenticTerminalView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let session = self.runtime.session_id().to_string();
        let theme = self.selected_theme();
        let theme_choices = STANDARD_TERMINAL_THEMES.into_iter().map(|choice| {
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
        });

        div()
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
                    .child("$ describe a task in natural language (input control is v0.2)"),
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
            .child(
                div()
                    .text_sm()
                    .text_color(rgb(theme.ansi[8]))
                    .child(format!("session: {session}")),
            )
    }
}

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();
    application().run(|cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(980.0), px(680.0)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            |_, cx| cx.new(|_| AgenticTerminalView::new()),
        )
        .expect("open macOS window");
        cx.activate(true);
    });
}
