use anyhow::{Context, Result};
use crossterm::{
    cursor::{Hide, Show},
    event::{DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{Frame, Terminal, TerminalOptions, Viewport, backend::CrosstermBackend};
use std::io::{Stdout, stdout};

use crate::model::RunOptions;

pub struct TerminalSession {
    terminal: Terminal<CrosstermBackend<Stdout>>,
    inline: bool,
    mouse: bool,
    restored: bool,
}

impl TerminalSession {
    pub fn enter(options: &RunOptions) -> Result<Self> {
        enable_raw_mode().context("enable terminal raw mode")?;
        match create_terminal(options) {
            Ok(terminal) => Ok(Self {
                terminal,
                inline: options.inline,
                mouse: options.mouse,
                restored: false,
            }),
            Err(error) => {
                // `create_terminal` may already have entered the alternate screen
                // or enabled bracketed paste before a later step failed. Restore
                // every mode we may own before returning the original error.
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

impl Drop for TerminalSession {
    fn drop(&mut self) {
        let _ = self.restore();
    }
}
