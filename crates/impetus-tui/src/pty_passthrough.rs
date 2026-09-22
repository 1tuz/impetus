//! Terminal pass-through attach for daemon-owned PTY sessions.
//!
//! Leaves Ratatui / alternate screen, forwards stdin→`PtyInput` and
//! `PtyOutput`→stdout as raw bytes, and restores the TUI on detach / exit /
//! error. **Not** an ANSI terminal emulator.

use std::io::{Write, stdout};
use std::time::Duration;

use anyhow::{Context, Result};
use crossterm::event::{
    Event as TerminalEvent, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, poll, read,
};
use crossterm::terminal::size as terminal_size;
use uuid::Uuid;

use crate::backend::UiBackend;
use crate::terminal::TerminalSession;

/// Open PTY pass-through from the TUI (free of Ctrl+F / Ctrl+B / Ctrl+R).
pub const OPEN_HINT: &str = "Ctrl+\\ / /pty";

/// Detach combo while attached (telnet-style). Documented in help + banner.
pub const DETACH_HINT: &str = "Ctrl+]";

/// Why the pass-through loop ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PassthroughEnd {
    /// User pressed the detach combo; daemon PTY stays live (Detached).
    Detached { pty_id: u64 },
    /// PTY reported EOF (child exited).
    Exited { pty_id: u64 },
    /// Transport / IPC failure after start (best-effort detach already tried).
    Failed {
        pty_id: Option<u64>,
        message: String,
    },
}

/// Default interactive shell for `/pty` with no args.
pub fn default_shell() -> (String, Vec<String>) {
    let command = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_owned());
    (command, Vec::new())
}

/// True when `key` is the documented detach combo (Ctrl+]).
pub fn is_detach_key(key: KeyEvent) -> bool {
    key.kind == KeyEventKind::Press
        && key.modifiers.contains(KeyModifiers::CONTROL)
        && matches!(key.code, KeyCode::Char(']'))
}

/// Encode a crossterm key into bytes a PTY slave expects (no emulator).
pub fn encode_key(key: KeyEvent) -> Option<Vec<u8>> {
    if key.kind != KeyEventKind::Press {
        return None;
    }

    match key.code {
        KeyCode::Char(ch) => {
            if key.modifiers.contains(KeyModifiers::CONTROL) {
                let ctrl = match ch {
                    'a'..='z' => (ch as u8 - b'a') + 1,
                    'A'..='Z' => (ch as u8 - b'A') + 1,
                    '@' => 0x00,
                    '[' => 0x1b,
                    '\\' => 0x1c,
                    ']' => 0x1d,
                    '^' => 0x1e,
                    '_' | '-' => 0x1f,
                    '?' => 0x7f,
                    _ => return Some(ch.to_string().into_bytes()),
                };
                Some(vec![ctrl])
            } else if key.modifiers.contains(KeyModifiers::ALT) {
                let mut out = vec![0x1b];
                out.extend(ch.to_string().into_bytes());
                Some(out)
            } else {
                Some(ch.to_string().into_bytes())
            }
        }
        KeyCode::Enter => Some(vec![b'\r']),
        KeyCode::Tab => Some(vec![b'\t']),
        KeyCode::Backspace => Some(vec![0x7f]),
        KeyCode::Esc => Some(vec![0x1b]),
        KeyCode::Delete => Some(b"\x1b[3~".to_vec()),
        KeyCode::Home => Some(b"\x1b[H".to_vec()),
        KeyCode::End => Some(b"\x1b[F".to_vec()),
        KeyCode::PageUp => Some(b"\x1b[5~".to_vec()),
        KeyCode::PageDown => Some(b"\x1b[6~".to_vec()),
        KeyCode::Up => Some(b"\x1b[A".to_vec()),
        KeyCode::Down => Some(b"\x1b[B".to_vec()),
        KeyCode::Right => Some(b"\x1b[C".to_vec()),
        KeyCode::Left => Some(b"\x1b[D".to_vec()),
        KeyCode::F(n) if (1..=12).contains(&n) => {
            let seq = match n {
                1 => "\x1bOP",
                2 => "\x1bOQ",
                3 => "\x1bOR",
                4 => "\x1bOS",
                5 => "\x1b[15~",
                6 => "\x1b[17~",
                7 => "\x1b[18~",
                8 => "\x1b[19~",
                9 => "\x1b[20~",
                10 => "\x1b[21~",
                11 => "\x1b[23~",
                12 => "\x1b[24~",
                _ => return None,
            };
            Some(seq.as_bytes().to_vec())
        }
        _ => None,
    }
}

/// Suspend Ratatui, run PTY I/O until detach/exit, then resume Ratatui.
pub async fn run(
    backend: &dyn UiBackend,
    terminal: &mut TerminalSession,
    session_id: Uuid,
    command: String,
    args: Vec<String>,
) -> Result<PassthroughEnd> {
    terminal
        .suspend_for_passthrough()
        .context("suspend TUI for PTY passthrough")?;

    let outcome = match run_attached(backend, session_id, command, args).await {
        Ok(end) => end,
        Err(error) => PassthroughEnd::Failed {
            pty_id: None,
            message: error.to_string(),
        },
    };

    // Always restore Ratatui even when IPC failed mid-session.
    terminal
        .resume_from_passthrough()
        .context("resume TUI after PTY passthrough")?;

    Ok(outcome)
}

async fn run_attached(
    backend: &dyn UiBackend,
    session_id: Uuid,
    command: String,
    args: Vec<String>,
) -> Result<PassthroughEnd> {
    let (cols, rows) = terminal_size().unwrap_or((80, 24));
    let session = backend
        .pty_start(
            session_id,
            command,
            args,
            None,
            Some(cols.max(1)),
            Some(rows.max(1)),
        )
        .await
        .context("PtyStart")?;
    let pty_id = session.pty_id;

    write_banner(pty_id)?;

    let end = match drive_io(backend, pty_id).await {
        Ok(end) => end,
        Err(error) => {
            let _ = backend.pty_detach(pty_id).await;
            PassthroughEnd::Failed {
                pty_id: Some(pty_id),
                message: error.to_string(),
            }
        }
    };

    // Drain any leftover event-queue noise before Ratatui takes stdin again.
    drain_pending_terminal_events();
    Ok(end)
}

async fn drive_io(backend: &dyn UiBackend, pty_id: u64) -> Result<PassthroughEnd> {
    let mut out = stdout();
    loop {
        let chunk = backend
            .pty_output(pty_id, Some(16 * 1024))
            .await
            .context("PtyOutput")?;
        if !chunk.data.is_empty() {
            out.write_all(&chunk.data)
                .context("write PTY output to stdout")?;
            out.flush().context("flush PTY stdout")?;
        }
        if chunk.eof {
            let _ = writeln!(out, "\r\n[impetus] PTY {pty_id} exited");
            let _ = out.flush();
            return Ok(PassthroughEnd::Exited { pty_id });
        }

        // Poll local keys while not blocking the async runtime for long.
        let deadline = std::time::Instant::now() + Duration::from_millis(16);
        while std::time::Instant::now() < deadline {
            if !poll(Duration::from_millis(0)).context("poll terminal input")? {
                break;
            }
            match read().context("read terminal input")? {
                TerminalEvent::Key(key) if is_detach_key(key) => {
                    backend.pty_detach(pty_id).await.context("PtyDetach")?;
                    let _ = writeln!(out, "\r\n[impetus] detached PTY {pty_id} ({DETACH_HINT})");
                    let _ = out.flush();
                    return Ok(PassthroughEnd::Detached { pty_id });
                }
                TerminalEvent::Key(key) => {
                    if let Some(bytes) = encode_key(key) {
                        backend
                            .pty_input(pty_id, &bytes)
                            .await
                            .context("PtyInput")?;
                    }
                }
                TerminalEvent::Paste(text) => {
                    backend
                        .pty_input(pty_id, text.as_bytes())
                        .await
                        .context("PtyInput paste")?;
                }
                TerminalEvent::Resize(cols, rows) => {
                    backend
                        .pty_resize(pty_id, cols.max(1), rows.max(1))
                        .await
                        .context("PtyResize")?;
                }
                _ => {}
            }
        }

        if chunk.data.is_empty() {
            tokio::time::sleep(Duration::from_millis(8)).await;
        }
    }
}

fn write_banner(pty_id: u64) -> Result<()> {
    let mut out = stdout();
    writeln!(
        out,
        "\r\n[impetus] PTY {pty_id} attached — {DETACH_HINT} returns to TUI\r"
    )
    .context("write PTY banner")?;
    out.flush().context("flush PTY banner")?;
    Ok(())
}

fn drain_pending_terminal_events() {
    while poll(Duration::from_millis(0)).unwrap_or(false) {
        let _ = read();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detach_key_is_ctrl_right_bracket() {
        let key = KeyEvent::new(KeyCode::Char(']'), KeyModifiers::CONTROL);
        assert!(is_detach_key(key));
        assert!(!is_detach_key(KeyEvent::new(
            KeyCode::Char('\\'),
            KeyModifiers::CONTROL
        )));
    }

    #[test]
    fn encode_ctrl_c_and_arrows() {
        assert_eq!(
            encode_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            Some(vec![0x03])
        );
        assert_eq!(
            encode_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE)),
            Some(b"\x1b[A".to_vec())
        );
        assert_eq!(
            encode_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            Some(vec![b'\r'])
        );
    }

    #[test]
    fn encode_detach_byte_matches_ctrl_bracket() {
        assert_eq!(
            encode_key(KeyEvent::new(KeyCode::Char(']'), KeyModifiers::CONTROL)),
            Some(vec![0x1d])
        );
    }
}
