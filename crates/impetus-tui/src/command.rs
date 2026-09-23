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
        description: "create session (path prompt for workspace root)",
        shortcut: "N in picker",
    },
    CommandSpec {
        name: "fork",
        aliases: &[],
        description: "fork active session at tip (or /fork <seq>)",
        shortcut: "Ctrl+Shift+K",
    },
    CommandSpec {
        name: "checkpoint",
        aliases: &["cp", "savepoint"],
        description: "create named checkpoint (/checkpoint <name>)",
        shortcut: "",
    },
    CommandSpec {
        name: "checkpoints",
        aliases: &["restore"],
        description: "list checkpoints; Enter restores as new branch",
        shortcut: "F7",
    },
    CommandSpec {
        name: "resume",
        aliases: &["open"],
        description: "open the session picker or attach a UUID",
        shortcut: "F2 / Ctrl+O",
    },
    CommandSpec {
        name: "sessions",
        aliases: &["ls"],
        description: "show durable sessions with workspace and state",
        shortcut: "F2 / Ctrl+O",
    },
    CommandSpec {
        name: "mode",
        aliases: &["permissions"],
        description: "choose ASK, PLAN, ACCEPT EDITS, AUTO, or BYPASS (daemon IPC)",
        shortcut: "F4 / Shift+Tab",
    },
    CommandSpec {
        name: "model",
        aliases: &["provider", "providers"],
        description: "Provider → Model → Reasoning → options from daemon catalog",
        shortcut: "F8",
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
        name: "auto",
        aliases: &["auto-safe"],
        description: "policy-allowed autonomy; risky paths approval-gated",
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
        name: "files",
        aliases: &["tree", "workspace"],
        description: "browse workspace files via harness IPC",
        shortcut: "Ctrl+F",
    },
    CommandSpec {
        name: "attach",
        aliases: &["artifact", "file"],
        description: "attach local file via chunked artifact_upload (ArtifactRef in composer)",
        shortcut: "Ctrl+Shift+A",
    },
    CommandSpec {
        name: "review",
        aliases: &["changes", "git-diff"],
        description: "review changed files + diffs via harness Git IPC",
        shortcut: "F6 / Ctrl+R",
    },
    CommandSpec {
        name: "pty",
        aliases: &["shell", "term"],
        description: "attach daemon PTY pass-through (Ctrl+] detaches; no emulator)",
        shortcut: "Ctrl+\\",
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
        shortcut: "F1 / ?",
    },
    CommandSpec {
        name: "theme",
        aliases: &["themes", "colors"],
        description: "pick a TUI theme (Impetus neon / geek pack)",
        shortcut: "Ctrl+Shift+T",
    },
    CommandSpec {
        name: "prompt",
        aliases: &[],
        description: "composer sends baseline Prompt intent",
        shortcut: "Ctrl+Shift+P",
    },
    CommandSpec {
        name: "steer",
        aliases: &[],
        description: "composer sends Steer intent (active run required)",
        shortcut: "Ctrl+T",
    },
    CommandSpec {
        name: "follow-up",
        aliases: &["followup", "follow"],
        description: "composer sends FollowUp intent (enqueue after turn)",
        shortcut: "Ctrl+Shift+P",
    },
    CommandSpec {
        name: "children",
        aliases: &["child", "subagents"],
        description: "list durable child-run results for the active session",
        shortcut: "",
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
    ModelPicker,
    SetMode(ExecutionMode),
    SetPromptIntent(impetus_client::protocol::UserPromptIntent),
    ShowDiff,
    ToggleInspector,
    Files,
    /// Attach local filesystem path via durable artifact upload.
    Attach {
        path: Option<String>,
    },
    Review,
    /// Fork active session; `None` = tip (`last_sequence`).
    Fork(Option<u64>),
    /// Create checkpoint; empty → name prompt.
    Checkpoint(Option<String>),
    /// List / restore checkpoints overlay.
    Checkpoints,
    /// Daemon PTY pass-through; `command` None → `$SHELL`.
    PtyPassthrough {
        command: Option<String>,
        args: Vec<String>,
    },
    Status,
    Diagnostics,
    Cancel,
    ClearViewport,
    Help,
    ThemePicker,
    SetTheme(String),
    CycleTheme,
    ListChildren,
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
        "fork" => {
            if argument.is_empty() {
                CommandAction::Fork(None)
            } else {
                match argument.parse::<u64>() {
                    Ok(seq) => CommandAction::Fork(Some(seq)),
                    Err(_) => CommandAction::Unknown(format!(
                        "`/fork` expects optional sequence number, got `{argument}`"
                    )),
                }
            }
        }
        "checkpoint" => {
            if argument.is_empty() {
                CommandAction::Checkpoint(None)
            } else {
                CommandAction::Checkpoint(Some(argument))
            }
        }
        "checkpoints" => CommandAction::Checkpoints,
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
        "model" => CommandAction::ModelPicker,
        "mode" => match argument.to_ascii_lowercase().as_str() {
            "" => CommandAction::ModePicker,
            "plan" => CommandAction::SetMode(ExecutionMode::Plan),
            "ask" | "accept" => CommandAction::SetMode(ExecutionMode::Ask),
            "auto" | "auto-safe" => CommandAction::SetMode(ExecutionMode::Auto),
            "accept-edits" | "edits" => CommandAction::SetMode(ExecutionMode::AcceptEdits),
            "bypass" | "full-auto" | "full" => CommandAction::SetMode(ExecutionMode::Bypass),
            other => CommandAction::Unknown(format!("unknown execution mode `{other}`")),
        },
        "plan" => CommandAction::SetMode(ExecutionMode::Plan),
        "ask" => CommandAction::SetMode(ExecutionMode::Ask),
        "auto" | "auto-safe" => CommandAction::SetMode(ExecutionMode::Auto),
        "diff" => CommandAction::ShowDiff,
        "details" => CommandAction::ToggleInspector,
        "files" => CommandAction::Files,
        "attach" => {
            if argument.is_empty() {
                CommandAction::Attach { path: None }
            } else {
                CommandAction::Attach {
                    path: Some(argument),
                }
            }
        }
        "review" => CommandAction::Review,
        "pty" | "shell" | "term" => {
            if argument.is_empty() {
                CommandAction::PtyPassthrough {
                    command: None,
                    args: Vec::new(),
                }
            } else {
                let mut parts = argument.split_whitespace();
                let cmd = parts.next().unwrap_or("sh").to_owned();
                let args = parts.map(str::to_owned).collect();
                CommandAction::PtyPassthrough {
                    command: Some(cmd),
                    args,
                }
            }
        }
        "status" => CommandAction::Status,
        "doctor" => CommandAction::Diagnostics,
        "cancel" => CommandAction::Cancel,
        "clear" => CommandAction::ClearViewport,
        "help" => CommandAction::Help,
        "theme" => match argument.to_ascii_lowercase().as_str() {
            "" => CommandAction::ThemePicker,
            "next" | "cycle" => CommandAction::CycleTheme,
            other => {
                if crate::theme::theme_meta(other).is_some() {
                    CommandAction::SetTheme(other.to_owned())
                } else {
                    CommandAction::Unknown(format!(
                        "unknown theme `{other}` — try /theme or /theme next"
                    ))
                }
            }
        },
        "prompt" => {
            CommandAction::SetPromptIntent(impetus_client::protocol::UserPromptIntent::Prompt)
        }
        "steer" => {
            CommandAction::SetPromptIntent(impetus_client::protocol::UserPromptIntent::Steer)
        }
        "follow-up" => {
            CommandAction::SetPromptIntent(impetus_client::protocol::UserPromptIntent::FollowUp)
        }
        "children" => CommandAction::ListChildren,
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
    fn aliases_resolve_to_core_modes() {
        assert_eq!(
            parse_command("/auto"),
            Some(CommandAction::SetMode(ExecutionMode::Auto))
        );
        assert_eq!(
            parse_command("/auto-safe"),
            Some(CommandAction::SetMode(ExecutionMode::Auto))
        );
        assert_eq!(
            parse_command("/accept"),
            Some(CommandAction::SetMode(ExecutionMode::Ask))
        );
        assert_eq!(
            parse_command("/mode bypass"),
            Some(CommandAction::SetMode(ExecutionMode::Bypass))
        );
    }

    #[test]
    fn intent_commands_set_prompt_steer_follow_up() {
        use impetus_client::protocol::UserPromptIntent;
        assert_eq!(
            parse_command("/steer"),
            Some(CommandAction::SetPromptIntent(UserPromptIntent::Steer))
        );
        assert_eq!(
            parse_command("/follow-up"),
            Some(CommandAction::SetPromptIntent(UserPromptIntent::FollowUp))
        );
        assert_eq!(
            parse_command("/followup"),
            Some(CommandAction::SetPromptIntent(UserPromptIntent::FollowUp))
        );
        assert_eq!(
            parse_command("/prompt"),
            Some(CommandAction::SetPromptIntent(UserPromptIntent::Prompt))
        );
    }

    #[test]
    fn theme_commands_parse() {
        assert_eq!(parse_command("/theme"), Some(CommandAction::ThemePicker));
        assert_eq!(parse_command("/model"), Some(CommandAction::ModelPicker));
        assert_eq!(
            parse_command("/providers"),
            Some(CommandAction::ModelPicker)
        );
        assert_eq!(
            parse_command("/theme next"),
            Some(CommandAction::CycleTheme)
        );
        assert_eq!(
            parse_command("/theme impetus-stars"),
            Some(CommandAction::SetTheme("impetus-stars".into()))
        );
        assert!(matches!(
            parse_command("/theme nope"),
            Some(CommandAction::Unknown(_))
        ));
    }

    #[test]
    fn attach_command_parses_path_or_prompt() {
        assert_eq!(
            parse_command("/attach"),
            Some(CommandAction::Attach { path: None })
        );
        assert_eq!(
            parse_command("/attach /tmp/notes.txt"),
            Some(CommandAction::Attach {
                path: Some("/tmp/notes.txt".into())
            })
        );
        assert_eq!(
            parse_command("/artifact ./foo.rs"),
            Some(CommandAction::Attach {
                path: Some("./foo.rs".into())
            })
        );
        assert_eq!(parse_command("/resume"), Some(CommandAction::Resume(None)));
    }

    #[test]
    fn fork_and_checkpoint_commands_parse() {
        assert_eq!(parse_command("/fork"), Some(CommandAction::Fork(None)));
        assert_eq!(
            parse_command("/fork 12"),
            Some(CommandAction::Fork(Some(12)))
        );
        assert_eq!(
            parse_command("/checkpoint stable"),
            Some(CommandAction::Checkpoint(Some("stable".into())))
        );
        assert_eq!(
            parse_command("/checkpoint"),
            Some(CommandAction::Checkpoint(None))
        );
        assert_eq!(
            parse_command("/checkpoints"),
            Some(CommandAction::Checkpoints)
        );
        assert_eq!(parse_command("/restore"), Some(CommandAction::Checkpoints));
        assert_eq!(
            parse_command("/cp mid"),
            Some(CommandAction::Checkpoint(Some("mid".into())))
        );
    }
}
