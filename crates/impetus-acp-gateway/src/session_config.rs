//! Map Impetus session model/reasoning selections onto ACP
//! `session/set_config_option` selectors (vendor-neutral categories).

use agent_client_protocol::schema::v1::{
    SessionConfigKind, SessionConfigOption, SessionConfigOptionCategory, SessionConfigSelect,
    SessionConfigSelectOptions,
};

/// Desired Impetus-side selection to push into an ACP session.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionLaunchOptions {
    /// Model id / select value to apply when the agent advertises a Model selector.
    pub model_id: Option<String>,
    /// Reasoning / thought-level value when the agent advertises ThoughtLevel.
    pub reasoning_effort: Option<String>,
    /// When true, missing selectors/values are errors. When false, skip quietly
    /// (profile-default path on agents without config_options).
    pub strict: bool,
}

/// One `session/set_config_option` call to issue before `session/prompt`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigOptionSet {
    pub config_id: String,
    pub value_id: String,
    pub category: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SessionConfigApplyError {
    #[error("ACP agent does not advertise session config options; cannot apply {what}")]
    NoConfigOptions { what: String },
    #[error("ACP agent has no {category} config option; cannot apply `{value}`")]
    MissingCategory { category: String, value: String },
    #[error(
        "ACP {category} option `{config_id}` has no value matching `{wanted}` (advertised: {advertised})"
    )]
    ValueNotAdvertised {
        category: String,
        config_id: String,
        wanted: String,
        advertised: String,
    },
}

fn flatten_select_options(
    options: &SessionConfigSelectOptions,
) -> Vec<&agent_client_protocol::schema::v1::SessionConfigSelectOption> {
    match options {
        SessionConfigSelectOptions::Ungrouped(opts) => opts.iter().collect(),
        SessionConfigSelectOptions::Grouped(groups) => {
            groups.iter().flat_map(|g| g.options.iter()).collect()
        }
        _ => Vec::new(),
    }
}

/// Extract select value ids from a config option (empty if not a select).
pub fn select_value_ids(option: &SessionConfigOption) -> Vec<String> {
    match &option.kind {
        SessionConfigKind::Select(SessionConfigSelect { options, .. }) => {
            flatten_select_options(options)
                .into_iter()
                .map(|opt| opt.value.0.to_string())
                .collect()
        }
        SessionConfigKind::Boolean(_) => Vec::new(),
        _ => Vec::new(),
    }
}

fn option_matches_category(
    option: &SessionConfigOption,
    wanted: &SessionConfigOptionCategory,
) -> bool {
    match &option.category {
        Some(cat) => cat == wanted,
        None => false,
    }
}

fn find_by_category<'a>(
    options: &'a [SessionConfigOption],
    category: &SessionConfigOptionCategory,
) -> Option<&'a SessionConfigOption> {
    options
        .iter()
        .find(|opt| option_matches_category(opt, category))
}

/// Resolve `value` against a select option: exact value id, else case-insensitive name.
pub fn resolve_select_value(option: &SessionConfigOption, wanted: &str) -> Option<String> {
    let SessionConfigKind::Select(select) = &option.kind else {
        return None;
    };
    let wanted_trim = wanted.trim();
    if wanted_trim.is_empty() {
        return None;
    }
    let flat = flatten_select_options(&select.options);
    for opt in &flat {
        if opt.value.0.as_ref() == wanted_trim {
            return Some(opt.value.0.to_string());
        }
    }
    let lower = wanted_trim.to_ascii_lowercase();
    for opt in &flat {
        if opt.name.eq_ignore_ascii_case(wanted_trim) || opt.name.to_ascii_lowercase() == lower {
            return Some(opt.value.0.to_string());
        }
    }
    None
}

fn advertised_list(option: &SessionConfigOption) -> String {
    let ids = select_value_ids(option);
    if ids.is_empty() {
        "(none)".into()
    } else {
        ids.join(", ")
    }
}

/// Build the set of ACP config-option writes required for `desired`.
///
/// Strict mode: Impetus-requested model/reasoning must match an advertised
/// selector value (no silent fallback). Non-strict: skip when the agent has
/// no matching selector (agents without `config_options`).
pub fn plan_config_option_sets(
    advertised: &[SessionConfigOption],
    desired: &SessionLaunchOptions,
) -> Result<Vec<ConfigOptionSet>, SessionConfigApplyError> {
    let mut out = Vec::new();

    if let Some(model) = desired
        .model_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        if advertised.is_empty() {
            if desired.strict {
                return Err(SessionConfigApplyError::NoConfigOptions {
                    what: format!("model `{model}`"),
                });
            }
        } else if let Some(option) =
            find_by_category(advertised, &SessionConfigOptionCategory::Model)
        {
            match resolve_select_value(option, model) {
                Some(value_id) => out.push(ConfigOptionSet {
                    config_id: option.id.0.to_string(),
                    value_id,
                    category: "model",
                }),
                None if desired.strict => {
                    return Err(SessionConfigApplyError::ValueNotAdvertised {
                        category: "model".into(),
                        config_id: option.id.0.to_string(),
                        wanted: model.to_owned(),
                        advertised: advertised_list(option),
                    });
                }
                None => {}
            }
        } else if desired.strict {
            return Err(SessionConfigApplyError::MissingCategory {
                category: "model".into(),
                value: model.to_owned(),
            });
        }
    }

    if let Some(effort) = desired
        .reasoning_effort
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        if advertised.is_empty() {
            if desired.strict {
                return Err(SessionConfigApplyError::NoConfigOptions {
                    what: format!("reasoning_effort `{effort}`"),
                });
            }
        } else if let Some(option) =
            find_by_category(advertised, &SessionConfigOptionCategory::ThoughtLevel)
        {
            match resolve_select_value(option, effort) {
                Some(value_id) => out.push(ConfigOptionSet {
                    config_id: option.id.0.to_string(),
                    value_id,
                    category: "thought_level",
                }),
                None if desired.strict => {
                    return Err(SessionConfigApplyError::ValueNotAdvertised {
                        category: "thought_level".into(),
                        config_id: option.id.0.to_string(),
                        wanted: effort.to_owned(),
                        advertised: advertised_list(option),
                    });
                }
                None => {}
            }
        } else if desired.strict {
            return Err(SessionConfigApplyError::MissingCategory {
                category: "thought_level".into(),
                value: effort.to_owned(),
            });
        }
    }

    Ok(out)
}

/// Model ids advertised via a Model-category select (for catalog enrichment).
pub fn advertised_model_ids(options: &[SessionConfigOption]) -> Vec<String> {
    find_by_category(options, &SessionConfigOptionCategory::Model)
        .map(select_value_ids)
        .unwrap_or_default()
}

/// Thought/reasoning levels advertised via ThoughtLevel select.
pub fn advertised_thought_levels(options: &[SessionConfigOption]) -> Vec<String> {
    find_by_category(options, &SessionConfigOptionCategory::ThoughtLevel)
        .map(select_value_ids)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_client_protocol::schema::v1::{SessionConfigOption, SessionConfigSelectOption};

    fn model_and_thought() -> Vec<SessionConfigOption> {
        vec![
            SessionConfigOption::select(
                "model",
                "Model",
                "gpt-4.1",
                vec![
                    SessionConfigSelectOption::new("gpt-4.1", "GPT-4.1"),
                    SessionConfigSelectOption::new("o3", "o3"),
                ],
            )
            .category(SessionConfigOptionCategory::Model),
            SessionConfigOption::select(
                "effort",
                "Reasoning",
                "medium",
                vec![
                    SessionConfigSelectOption::new("low", "Low"),
                    SessionConfigSelectOption::new("medium", "Medium"),
                    SessionConfigSelectOption::new("xhigh", "Extra high"),
                    SessionConfigSelectOption::new("max", "Max"),
                ],
            )
            .category(SessionConfigOptionCategory::ThoughtLevel),
        ]
    }

    #[test]
    fn plans_model_and_thought_sets() {
        let plan = plan_config_option_sets(
            &model_and_thought(),
            &SessionLaunchOptions {
                model_id: Some("o3".into()),
                reasoning_effort: Some("xhigh".into()),
                strict: true,
            },
        )
        .unwrap();
        assert_eq!(
            plan,
            vec![
                ConfigOptionSet {
                    config_id: "model".into(),
                    value_id: "o3".into(),
                    category: "model",
                },
                ConfigOptionSet {
                    config_id: "effort".into(),
                    value_id: "xhigh".into(),
                    category: "thought_level",
                },
            ]
        );
    }

    #[test]
    fn rejects_unknown_effort_without_fallback() {
        let err = plan_config_option_sets(
            &model_and_thought(),
            &SessionLaunchOptions {
                model_id: None,
                reasoning_effort: Some("ultra".into()),
                strict: true,
            },
        )
        .unwrap_err();
        assert!(matches!(
            err,
            SessionConfigApplyError::ValueNotAdvertised { .. }
        ));
    }

    #[test]
    fn empty_desired_needs_no_writes() {
        let plan = plan_config_option_sets(&model_and_thought(), &SessionLaunchOptions::default())
            .unwrap();
        assert!(plan.is_empty());
    }

    #[test]
    fn override_without_advertised_options_fails_when_strict() {
        let err = plan_config_option_sets(
            &[],
            &SessionLaunchOptions {
                model_id: Some("o3".into()),
                reasoning_effort: None,
                strict: true,
            },
        )
        .unwrap_err();
        assert!(matches!(
            err,
            SessionConfigApplyError::NoConfigOptions { .. }
        ));
    }

    #[test]
    fn non_strict_skips_when_agent_has_no_options() {
        let plan = plan_config_option_sets(
            &[],
            &SessionLaunchOptions {
                model_id: Some("o3".into()),
                reasoning_effort: Some("high".into()),
                strict: false,
            },
        )
        .unwrap();
        assert!(plan.is_empty());
    }
}
