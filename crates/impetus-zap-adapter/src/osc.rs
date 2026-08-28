//! OSC escape sequences for Zap notification hooks.
//!
//! This module generates OSC (Operating System Command) sequences to notify
//! Zap terminal about harness state changes, approvals, and agent output.
//!
//! ## OSC Sequence Format
//!
//! OSC sequences follow the format: `ESC ] Ps ; Pt BEL` where:
//! - ESC is `\x1b`
//! - Ps is a numeric parameter (e.g., 0, 9, 777)
//! - Pt is the text parameter
//! - BEL is `\x07` (or alternatively `ESC \` can be used)
//!
//! ## Supported Sequences
//!
//! - **OSC 777** - Custom notifications (used by various modern terminals)
//!   Format: `ESC ] 777 ; notify ; type ; data BEL`
//!
//! - **OSC 9** - iTerm2/Windows Terminal notifications
//!   Format: `ESC ] 9 ; message BEL`
//!
//! - **OSC 0** - Window title (widely supported)
//!   Format: `ESC ] 0 ; title BEL`
//!
//! ## References
//!
//! - XTerm Control Sequences: https://invisible-island.net/xterm/ctlseqs/ctlseqs.html
//! - iTerm2 Proprietary Escape Codes: https://iterm2.com/documentation-escape-codes.html

use std::io::{self, Write};

/// OSC sequence format: ESC ] <code> ; <data> BEL
/// We use OSC 777 for custom notifications (common in modern terminals).
const OSC_START: &str = "\x1b]777;";
const OSC_END: &str = "\x07";

/// OSC 9 prefix for iTerm2/Windows Terminal compatibility
#[allow(dead_code)]
const OSC_9_START: &str = "\x1b]9;";

/// OSC 0 prefix for window title updates
#[allow(dead_code)]
const OSC_0_START: &str = "\x1b]0;";

/// Notification types for Zap terminal hooks
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotificationType {
    StateChange,
    Approval,
    Output,
    Error,
    Warning,
}

impl NotificationType {
    fn as_str(&self) -> &'static str {
        match self {
            NotificationType::StateChange => "state",
            NotificationType::Approval => "approval",
            NotificationType::Output => "output",
            NotificationType::Error => "error",
            NotificationType::Warning => "warning",
        }
    }
}

/// Send OSC notification to Zap terminal using OSC 777
pub fn send_notification(notification_type: NotificationType, data: &str) {
    let escaped = escape_osc_data(data);
    print!(
        "{}notify;{};{}{}",
        OSC_START,
        notification_type.as_str(),
        escaped,
        OSC_END
    );
    io::stdout().flush().unwrap();
}

/// Send a dual notification using both OSC 777 and OSC 9 for broader compatibility
#[allow(dead_code)]
pub fn send_notification_dual(title: &str, message: &str) {
    // OSC 777 - structured notification
    let escaped_msg = escape_osc_data(message);
    print!(
        "{}notify;{};{}{}",
        OSC_START,
        escape_osc_data(title),
        escaped_msg,
        OSC_END
    );

    // OSC 9 - iTerm2/Windows Terminal notification
    print!(
        "{}{}: {}{}",
        OSC_9_START,
        escape_osc_data(title),
        escaped_msg,
        OSC_END
    );

    io::stdout().flush().unwrap();
}

/// Update the window title using OSC 0
#[allow(dead_code)]
pub fn set_window_title(title: &str) {
    let escaped = escape_osc_data(title);
    print!("{}{}{}", OSC_0_START, escaped, OSC_END);
    io::stdout().flush().unwrap();
}

/// Send state change notification (Running / Idle / NeedsApproval)
pub fn send_state(state: &str, detail: Option<&str>) {
    let data = if let Some(detail) = detail {
        format!("{}:{}", state, detail)
    } else {
        state.to_string()
    };
    send_notification(NotificationType::StateChange, &data);
}

/// Send approval request notification with metadata
pub fn send_approval_request(approval_id: &str, summary: &str, affected_files: &[String]) {
    let files = affected_files.join(",");
    let data = format!("{}|{}|{}", approval_id, summary, files);
    send_notification(NotificationType::Approval, &data);
}

/// Send agent output chunk notification
pub fn send_output_chunk(chunk: &str) {
    send_notification(NotificationType::Output, chunk);
}

/// Send error notification
pub fn send_error(message: &str) {
    send_notification(NotificationType::Error, message);
}

/// Send warning notification
pub fn send_warning(message: &str) {
    send_notification(NotificationType::Warning, message);
}

/// Escape special characters in OSC data to prevent injection
fn escape_osc_data(data: &str) -> String {
    data.replace(['\x1b', '\x07', ']'], "")
        .replace(';', ":")
        .chars()
        .filter(|c| !c.is_control() || *c == '\n' || *c == '\t')
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escape_removes_control_sequences() {
        // ESC (\x1b), BEL (\x07), and ] are all removed
        assert_eq!(escape_osc_data("\x1b]foo\x07bar"), "foobar");
        assert_eq!(escape_osc_data("foo;bar"), "foo:bar");
    }

    #[test]
    fn notification_type_serialization() {
        assert_eq!(NotificationType::StateChange.as_str(), "state");
        assert_eq!(NotificationType::Approval.as_str(), "approval");
        assert_eq!(NotificationType::Error.as_str(), "error");
    }

    #[test]
    fn escapes_control_characters() {
        let input = "Test\nWith\rNewlines\x00Null";
        let escaped = escape_osc_data(input);
        assert!(escaped.contains('\n')); // newlines are preserved
        assert!(!escaped.contains('\r')); // carriage return removed
        assert!(!escaped.contains('\x00')); // null removed
    }

    #[test]
    fn escapes_osc_special_chars() {
        assert_eq!(escape_osc_data("foo\x1bbar"), "foobar"); // ESC removed
        assert_eq!(escape_osc_data("foo\x07bar"), "foobar"); // BEL removed
        assert_eq!(escape_osc_data("foo;bar"), "foo:bar"); // semicolon replaced
    }
}
