use anyhow::{Context, Result};
use crossterm::{
    cursor::{Hide, Show},
    event::{DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{Frame, Terminal, TerminalOptions, Viewport, backend::CrosstermBackend};
use std::io::{Stdout, stdout};
use std::sync::atomic::{AtomicBool, Ordering};

use crate::model::RunOptions;

/// Set while a `TerminalSession` owns raw/alternate-screen modes.
/// Panic hook and `restore()` race on this flag so only one path restores.
static RESTORE_PENDING: AtomicBool = AtomicBool::new(false);
static PANIC_HOOK_INSTALLED: AtomicBool = AtomicBool::new(false);

pub struct TerminalSession {
    terminal: Terminal<CrosstermBackend<Stdout>>,
    inline: bool,
    mouse: bool,
    restored: bool,
}

impl TerminalSession {
    pub fn enter(options: &RunOptions) -> Result<Self> {
        ensure_panic_hook();
        enable_raw_mode().context("enable terminal raw mode")?;
        // Armed before alternate-screen setup so a panic mid-enter still restores.
        arm_restore();
        match create_terminal(options) {
            Ok(terminal) => Ok(Self {
                terminal,
                inline: options.inline,
                mouse: options.mouse,
                restored: false,
            }),
            Err(error) => {
                // Disarm panic hook ownership; this path restores itself.
                let _ = claim_restore();
                let mut output = stdout();
                if options.mouse {
                    let _ = execute!(output, DisableMouseCapture);
                }
                if options.inline {
                    let _ = execute!(output, DisableBracketedPaste, Show);
                } else {
                    let _ = execute!(output, DisableBracketedPaste, LeaveAlternateScreen, Show);
                }
                let _ = disable_raw_mode();
                Err(error)
            }
        }
    }

    pub fn draw<F>(&mut self, render: F) -> Result<()>
    where
        F: FnOnce(&mut Frame),
    {
        self.terminal.draw(render).context("draw TUI frame")?;
        Ok(())
    }

    pub fn restore(&mut self) -> Result<()> {
        if self.restored {
            return Ok(());
        }

        // Mark first so Drop never loops through a partially failed restore.
        self.restored = true;
        if !claim_restore() {
            // Panic hook already restored the terminal.
            return Ok(());
        }

        let mut first_error = None;
        let backend = self.terminal.backend_mut();

        if self.mouse
            && let Err(error) = execute!(backend, DisableMouseCapture)
        {
            first_error = Some(anyhow::Error::new(error).context("disable mouse capture"));
        }

        let mode_result = if self.inline {
            execute!(backend, DisableBracketedPaste, Show)
        } else {
            execute!(backend, DisableBracketedPaste, LeaveAlternateScreen, Show)
        };
        if let Err(error) = mode_result
            && first_error.is_none()
        {
            first_error = Some(anyhow::Error::new(error).context("restore terminal modes"));
        }

        if let Err(error) = self.terminal.show_cursor()
            && first_error.is_none()
        {
            first_error = Some(anyhow::Error::new(error).context("show terminal cursor"));
        }
        if let Err(error) = disable_raw_mode()
            && first_error.is_none()
        {
            first_error = Some(anyhow::Error::new(error).context("disable terminal raw mode"));
        }

        match first_error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
}

fn create_terminal(options: &RunOptions) -> Result<Terminal<CrosstermBackend<Stdout>>> {
    let mut output = stdout();
    if options.inline {
        execute!(output, EnableBracketedPaste, Hide).context("enable inline TUI terminal modes")?;
    } else {
        execute!(output, EnterAlternateScreen, EnableBracketedPaste, Hide)
            .context("enter alternate terminal screen")?;
    }
    if options.mouse {
        execute!(output, EnableMouseCapture).context("enable mouse capture")?;
    }

    let backend = CrosstermBackend::new(output);
    let mut terminal = if options.inline {
        Terminal::with_options(
            backend,
            TerminalOptions {
                viewport: Viewport::Inline(options.inline_rows),
            },
        )
        .context("create inline Ratatui terminal")?
    } else {
        Terminal::new(backend).context("create Ratatui terminal")?
    };
    if !options.inline {
        terminal.clear().context("clear terminal")?;
    }
    Ok(terminal)
}

fn arm_restore() {
    RESTORE_PENDING.store(true, Ordering::SeqCst);
}

fn claim_restore() -> bool {
    RESTORE_PENDING.swap(false, Ordering::SeqCst)
}

fn ensure_panic_hook() {
    if PANIC_HOOK_INSTALLED.swap(true, Ordering::SeqCst) {
        return;
    }
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        emergency_restore();
        previous(info);
    }));
}

/// Best-effort restore used by the panic hook (no `TerminalSession` available).
fn emergency_restore() {
    if !claim_restore() {
        return;
    }
    let mut output = stdout();
    let _ = execute!(
        output,
        DisableMouseCapture,
        DisableBracketedPaste,
        LeaveAlternateScreen,
        Show
    );
    let _ = disable_raw_mode();
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        let _ = self.restore();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claim_restore_is_single_winner() {
        arm_restore();
        assert!(claim_restore());
        assert!(!claim_restore());
    }

    #[test]
    fn emergency_restore_is_noop_when_disarmed() {
        let _ = claim_restore();
        emergency_restore();
        assert!(!claim_restore());
    }
}
