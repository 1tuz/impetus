use uuid::Uuid;

use crate::model::ExecutionMode;

#[derive(Clone, Copy, Debug)]
pub struct CommandSpec {
    pub name: &'static str,
    pub aliases: &'static [&'static str],
    pub description: &'static str,
    pub shortcut: &'static str,
}

pub const COMMANDS: &[CommandSpec] = &[
    CommandSpec {
        name: "new",
        aliases: &["create"],
        description: "create and attach a durable session",
        shortcut: "",
    },
    CommandSpec {
        name: "resume",
        aliases: &["attach"],
        description: "open the session picker or attach a UUID",
        shortcut: "F2",
    },
    CommandSpec {
        name: "sessions",
        aliases: &["ls"],
        description: "show durable sessions with workspace and state",
        shortcut: "F2",
    },
    CommandSpec {
        name: "mode",
        aliases: &["permissions"],
        description: "choose Plan, Ask, Auto-Safe or a server-backed scope",
        shortcut: "F4",
    },
    CommandSpec {
        name: "plan",
        aliases: &[],
        description: "switch to non-mutating planning mode",
        shortcut: "",
    },
    CommandSpec {
        name: "ask",
        aliases: &["accept"],
        description: "show every daemon-requested approval",
        shortcut: "",
    },
    CommandSpec {
        name: "auto-safe",
        aliases: &["auto"],
        description: "continue safe work; keep mutations approval-gated",
        shortcut: "",
    },
    CommandSpec {
        name: "diff",
        aliases: &[],
        description: "open the selected approval or tool diff/details",
        shortcut: "Ctrl+D",
    },
    CommandSpec {
        name: "details",
        aliases: &["inspect"],
        description: "toggle the structured inspector pane",
        shortcut: "F3",
    },
    CommandSpec {
        name: "status",
        aliases: &["usage"],
        description: "show connection, run, context and token status",
        shortcut: "",
    },
    CommandSpec {
        name: "doctor",
        aliases: &["diagnostics"],
        description: "open redacted harness diagnostics",
        shortcut: "",
    },
    CommandSpec {
        name: "cancel",
        aliases: &["stop"],
        description: "request cancellation at the next safe boundary",
        shortcut: "Ctrl+C",
    },
    CommandSpec {
        name: "clear",
        aliases: &[],
        description: "clear only the local viewport, never durable history",
        shortcut: "Ctrl+L",
    },
    CommandSpec {
        name: "help",
        aliases: &["keys"],
        description: "show keymap and execution-mode semantics",
        shortcut: "F1",
    },
    CommandSpec {
        name: "quit",
        aliases: &["exit", "q"],
        description: "close the client; daemon sessions keep running",
        shortcut: "Ctrl+Q",
    },
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CommandAction {
    NewSession,
    Resume(Option<Uuid>),
    Sessions,
    ModePicker,
    SetMode(ExecutionMode),
    ShowDiff,
    ToggleInspector,
    Status,
    Diagnostics,
    Cancel,
    ClearViewport,
    Help,
    Quit,
    Unknown(String),
}

pub fn parse_command(input: &str) -> Option<CommandAction> {
    let trimmed = input.trim();
    let command_line = trimmed.strip_prefix('/')?;
    let mut parts = command_line.split_whitespace();
    let name = parts.next().unwrap_or_default().to_ascii_lowercase();
    let argument = parts.collect::<Vec<_>>().join(" ");

    let canonical = canonical_name(&name).unwrap_or(name.as_str());
    let action = match canonical {
        "new" => CommandAction::NewSession,
        "resume" => {
            if argument.is_empty() {
                CommandAction::Resume(None)
            } else {
                match Uuid::parse_str(&argument) {
                    Ok(id) => CommandAction::Resume(Some(id)),
                    Err(_) => CommandAction::Unknown(format!(
                        "`/resume` expects a full session UUID, got `{argument}`"
                    )),
                }
            }
        }
        "sessions" => CommandAction::Sessions,
        "mode" => match argument.to_ascii_lowercase().as_str() {
            "" => CommandAction::ModePicker,
            "plan" => CommandAction::SetMode(ExecutionMode::Plan),
            "ask" | "accept" => CommandAction::SetMode(ExecutionMode::Ask),
            "auto" | "auto-safe" => CommandAction::SetMode(ExecutionMode::AutoSafe),
            "accept-edits" | "edits" => CommandAction::SetMode(ExecutionMode::AcceptEdits),
            "full-auto" | "full" => CommandAction::SetMode(ExecutionMode::FullAuto),
            other => CommandAction::Unknown(format!("unknown execution mode `{other}`")),
        },
        "plan" => CommandAction::SetMode(ExecutionMode::Plan),
        "ask" => CommandAction::SetMode(ExecutionMode::Ask),
        "auto-safe" => CommandAction::SetMode(ExecutionMode::AutoSafe),
        "diff" => CommandAction::ShowDiff,
        "details" => CommandAction::ToggleInspector,
        "status" => CommandAction::Status,
        "doctor" => CommandAction::Diagnostics,
        "cancel" => CommandAction::Cancel,
        "clear" => CommandAction::ClearViewport,
        "help" => CommandAction::Help,
        "quit" => CommandAction::Quit,
        _ => CommandAction::Unknown(format!("unknown command `/{name}`")),
    };
    Some(action)
}

pub fn suggestions(query: &str) -> Vec<&'static CommandSpec> {
    let query = query
        .trim_start()
        .trim_start_matches('/')
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let mut ranked = COMMANDS
        .iter()
        .filter_map(|command| {
            let mut score = fuzzy_score(&query, command.name);
            for alias in command.aliases {
                score = score.max(fuzzy_score(&query, alias));
            }
            (score >= 0).then_some((score, command))
        })
        .collect::<Vec<_>>();
    ranked.sort_by(|(left_score, left), (right_score, right)| {
        right_score
            .cmp(left_score)
            .then_with(|| left.name.cmp(right.name))
    });
    ranked.into_iter().map(|(_, command)| command).collect()
}

fn canonical_name(name: &str) -> Option<&'static str> {
    COMMANDS.iter().find_map(|command| {
        (command.name == name || command.aliases.contains(&name)).then_some(command.name)
    })
}

fn fuzzy_score(needle: &str, haystack: &str) -> i32 {
    if needle.is_empty() {
        return 0;
    }
    if haystack == needle {
        return 1_000;
    }
    if haystack.starts_with(needle) {
        return 700 - haystack.len() as i32;
    }
    let mut score = 0i32;
    let mut cursor = 0usize;
    let chars = haystack.chars().collect::<Vec<_>>();
    for needle_char in needle.chars() {
        let Some(relative) = chars[cursor..]
            .iter()
            .position(|candidate| candidate.eq_ignore_ascii_case(&needle_char))
        else {
            return -1;
        };
        cursor += relative + 1;
        score += 20 - relative.min(19) as i32;
    }
    score - haystack.len() as i32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aliases_resolve_to_safe_mode() {
        assert_eq!(
            parse_command("/auto"),
            Some(CommandAction::SetMode(ExecutionMode::AutoSafe))
        );
        assert_eq!(
            parse_command("/accept"),
            Some(CommandAction::SetMode(ExecutionMode::Ask))
        );
    }

    #[test]
    fn palette_prefers_prefix_matches() {
        let results = suggestions("/ses");
        assert_eq!(results.first().map(|item| item.name), Some("sessions"));
    }
}
