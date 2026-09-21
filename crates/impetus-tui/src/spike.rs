//! Minimal Ratatui + Crossterm evaluation surface (#137).
//!
//! Production UI lives in `app` / `render` / `composer`. This module only
//! documents stack fitness with headless smoke tests (no TTY required).
//!
//! Boundary reminder: TUI talks to the harness through `UiBackend` →
//! `HarnessClient` only — never import `impetus-core`.

use crossterm::event::Event as TerminalEvent;
use ratatui::{
    Terminal,
    backend::TestBackend,
    layout::{Constraint, Direction, Layout},
    widgets::{Block, Borders, Paragraph},
};

use crate::composer::Composer;

/// Pins recorded by the #137 evaluation (must match `Cargo.toml` / lock).
pub const SPIKE_RATATUI: &str = "0.30.2";
pub const SPIKE_CROSSTERM: &str = "0.29.0";

/// Draw a one-frame composer stub into a TestBackend terminal.
///
/// Proves Ratatui layout + widget path compile and render without a real
/// Crossterm alternate screen. Used by unit tests; not a product entrypoint.
pub fn render_composer_stub(width: u16, height: u16, composer: &Composer) -> String {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).expect("test terminal");
    terminal
        .draw(|frame| {
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Min(1), Constraint::Length(3)])
                .split(frame.area());

            let status = Paragraph::new("impetus-tui spike · HarnessClient boundary")
                .block(Block::default().borders(Borders::ALL).title("status"));
            frame.render_widget(status, chunks[0]);

            let body = composer.text();
            let input = Paragraph::new(if body.is_empty() { ">" } else { body }).block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("composer stub"),
            );
            frame.render_widget(input, chunks[1]);
        })
        .expect("draw stub");

    let buffer = terminal.backend().buffer().clone();
    let area = buffer.area();
    let mut out = String::new();
    for y in 0..area.height {
        for x in 0..area.width {
            out.push_str(buffer[(x, y)].symbol());
        }
        out.push('\n');
    }
    out
}

/// True when Crossterm exposes Paste / Resize variants used by the app loop.
pub fn paste_and_resize_hooks_available() -> bool {
    // Discriminant smoke: constructing events proves the API surface we rely on.
    let paste = TerminalEvent::Paste("hello".into());
    let resize = TerminalEvent::Resize(80, 24);
    matches!(paste, TerminalEvent::Paste(_)) && matches!(resize, TerminalEvent::Resize(_, _))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pins_match_declared_spike_versions() {
        // Keep in sync with crates/impetus-tui/Cargo.toml (no `latest`).
        assert_eq!(SPIKE_RATATUI, "0.30.2");
        assert_eq!(SPIKE_CROSSTERM, "0.29.0");
    }

    #[test]
    fn composer_stub_renders_under_test_backend() {
        let mut composer = Composer::default();
        composer.insert_str("spike");
        let frame = render_composer_stub(40, 8, &composer);
        assert!(frame.contains("composer stub"), "{frame}");
        assert!(frame.contains("spike"), "{frame}");
        assert!(frame.contains("HarnessClient"), "{frame}");
    }

    #[test]
    fn bracketed_paste_and_resize_events_exist() {
        assert!(paste_and_resize_hooks_available());
    }
}
