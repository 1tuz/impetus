//! Daemon provider catalog projection for the TUI model picker.
//!
//! Rows come from `ListProviders` only — no hard-coded vendor lists.
//! Options are string keys/values from catalog metadata (`service_tiers`,
//! `provider_options`); no invented enums. Applying `provider_options` through
//! `SetSessionModel` waits on #328 — UI keeps a local draft for display.

use impetus_client::protocol::{
    ModelAvailability, ModelProviderHealthLabel, ModelProviderStatus, SessionModelSelection,
};

/// Wizard step: Provider → Model → Reasoning → Options.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModelPickerStep {
    Provider,
    Model,
    Reasoning,
    Options,
}

#[derive(Clone, Debug)]
pub struct ModelPickerState {
    pub step: ModelPickerStep,
    pub selected: usize,
    pub query: String,
    pub draft_provider_id: Option<String>,
    pub draft_model_id: Option<String>,
    pub draft_reasoning: Option<String>,
    /// Local options draft (JSON object). Not sent until IPC gains the field.
    pub draft_options: Option<serde_json::Value>,
    /// True when the wizard visited the Reasoning step (for Esc/Left back).
    pub visited_reasoning: bool,
}

impl Default for ModelPickerState {
    fn default() -> Self {
        Self {
            step: ModelPickerStep::Provider,
            selected: 0,
            query: String::new(),
            draft_provider_id: None,
            draft_model_id: None,
            draft_reasoning: None,
            draft_options: None,
            visited_reasoning: false,
        }
    }
}

impl ModelPickerState {
    pub fn fresh(selection: Option<&SessionModelSelection>) -> Self {
        let mut state = Self::default();
        if let Some(sel) = selection {
            state.draft_provider_id = Some(sel.provider_id.clone());
            state.draft_model_id = Some(sel.model_id.clone());
            state.draft_reasoning = sel.reasoning_effort.clone();
        }
        state
    }

    /// Back one step; returns false when already on Provider (caller closes).
    pub fn step_back(&mut self) -> bool {
        match self.step {
            ModelPickerStep::Provider => false,
            ModelPickerStep::Model => {
                self.step = ModelPickerStep::Provider;
                self.draft_model_id = None;
                self.draft_reasoning = None;
                self.draft_options = None;
                self.visited_reasoning = false;
                self.selected = 0;
                self.query.clear();
                true
            }
            ModelPickerStep::Reasoning => {
                self.step = ModelPickerStep::Model;
                self.draft_reasoning = None;
                self.draft_options = None;
                self.visited_reasoning = false;
                self.selected = 0;
                self.query.clear();
                true
            }
            ModelPickerStep::Options => {
                self.step = if self.visited_reasoning {
                    ModelPickerStep::Reasoning
                } else {
                    ModelPickerStep::Model
                };
                self.draft_options = None;
                self.selected = 0;
                self.query.clear();
                true
            }
        }
    }
}

/// Unique provider ids in catalog order (first row wins display name).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderChoice {
    pub provider_id: String,
    pub display_name: String,
    /// True when at least one model row for this provider is selectable.
    pub selectable: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CatalogOptionChoice {
    pub key: String,
    pub value: String,
    pub label: String,
}

/// Unavailable health/availability → not selectable. Unknown stays selectable.
pub fn row_is_selectable(row: &ModelProviderStatus) -> bool {
    if matches!(row.availability, ModelAvailability::Unavailable) {
        return false;
    }
    !matches!(row.health, ModelProviderHealthLabel::Unavailable { .. })
}

pub fn provider_choices(catalog: &[ModelProviderStatus]) -> Vec<ProviderChoice> {
    let mut out: Vec<ProviderChoice> = Vec::new();
    for row in catalog {
        if let Some(existing) = out.iter_mut().find(|p| p.provider_id == row.provider_id) {
            if row_is_selectable(row) {
                existing.selectable = true;
            }
            continue;
        }
        out.push(ProviderChoice {
            display_name: row
                .provider_display_name
                .clone()
                .unwrap_or_else(|| row.provider_id.clone()),
            selectable: row_is_selectable(row),
            provider_id: row.provider_id.clone(),
        });
    }
    out
}

pub fn models_for_provider<'a>(
    catalog: &'a [ModelProviderStatus],
    provider_id: &str,
) -> Vec<&'a ModelProviderStatus> {
    catalog
        .iter()
        .filter(|row| row.provider_id == provider_id)
        .collect()
}

pub fn find_row<'a>(
    catalog: &'a [ModelProviderStatus],
    provider_id: &str,
    model_id: &str,
) -> Option<&'a ModelProviderStatus> {
    catalog
        .iter()
        .find(|row| row.provider_id == provider_id && row.model_id == model_id)
}

/// Flatten catalog option metadata into selectable string pairs (no enums).
pub fn option_choices(row: &ModelProviderStatus) -> Vec<CatalogOptionChoice> {
    let mut out = Vec::new();
    for tier in &row.service_tiers {
        out.push(CatalogOptionChoice {
            key: "service_tier".to_owned(),
            value: tier.clone(),
            label: format!("service_tier · {tier}"),
        });
    }
    match &row.provider_options {
        serde_json::Value::Object(map) => {
            for (key, value) in map {
                push_option_values(&mut out, key, value);
            }
        }
        serde_json::Value::Array(items) => {
            for (idx, value) in items.iter().enumerate() {
                push_option_values(&mut out, &format!("option_{idx}"), value);
            }
        }
        serde_json::Value::String(s) if !s.is_empty() => {
            out.push(CatalogOptionChoice {
                key: "provider_options".to_owned(),
                value: s.clone(),
                label: s.clone(),
            });
        }
        _ => {}
    }
    out
}

fn push_option_values(out: &mut Vec<CatalogOptionChoice>, key: &str, value: &serde_json::Value) {
    match value {
        serde_json::Value::String(s) if !s.is_empty() => {
            out.push(CatalogOptionChoice {
                key: key.to_owned(),
                value: s.clone(),
                label: format!("{key} · {s}"),
            });
        }
        serde_json::Value::Array(items) => {
            for item in items {
                match item {
                    serde_json::Value::String(s) if !s.is_empty() => {
                        out.push(CatalogOptionChoice {
                            key: key.to_owned(),
                            value: s.clone(),
                            label: format!("{key} · {s}"),
                        });
                    }
                    serde_json::Value::Object(obj) => {
                        let id = obj
                            .get("id")
                            .or_else(|| obj.get("value"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("");
                        if id.is_empty() {
                            continue;
                        }
                        let label = obj
                            .get("label")
                            .or_else(|| obj.get("name"))
                            .and_then(|v| v.as_str())
                            .unwrap_or(id);
                        out.push(CatalogOptionChoice {
                            key: key.to_owned(),
                            value: id.to_owned(),
                            label: format!("{key} · {label}"),
                        });
                    }
                    _ => {}
                }
            }
        }
        serde_json::Value::Bool(b) => {
            out.push(CatalogOptionChoice {
                key: key.to_owned(),
                value: b.to_string(),
                label: format!("{key} · {b}"),
            });
        }
        serde_json::Value::Number(n) => {
            out.push(CatalogOptionChoice {
                key: key.to_owned(),
                value: n.to_string(),
                label: format!("{key} · {n}"),
            });
        }
        _ => {}
    }
}

pub fn merge_option_choice(
    current: Option<serde_json::Value>,
    choice: &CatalogOptionChoice,
) -> serde_json::Value {
    let mut obj = match current {
        Some(serde_json::Value::Object(map)) => map,
        _ => serde_json::Map::new(),
    };
    obj.insert(
        choice.key.clone(),
        serde_json::Value::String(choice.value.clone()),
    );
    serde_json::Value::Object(obj)
}

pub fn filter_by_query<'a, T>(
    items: &'a [T],
    query: &str,
    label: impl Fn(&T) -> String,
) -> Vec<(usize, &'a T)> {
    let q = query.trim().to_ascii_lowercase();
    items
        .iter()
        .enumerate()
        .filter(|(_, item)| {
            if q.is_empty() {
                return true;
            }
            label(item).to_ascii_lowercase().contains(&q)
        })
        .collect()
}

pub fn session_model_label(
    selection: Option<&SessionModelSelection>,
    options: Option<&serde_json::Value>,
) -> String {
    let Some(sel) = selection else {
        return "model —".to_owned();
    };
    let mut parts = vec![format!("{}/{}", sel.provider_id, sel.model_id)];
    if let Some(effort) = sel.reasoning_effort.as_deref().filter(|s| !s.is_empty()) {
        parts.push(effort.to_owned());
    }
    if let Some(serde_json::Value::Object(map)) = options {
        for (k, v) in map {
            if let Some(s) = v.as_str() {
                parts.push(format!("{k}={s}"));
            }
        }
    }
    parts.join(" · ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn healthy(provider: &str, model: &str) -> ModelProviderStatus {
        ModelProviderStatus::basic(provider, model, ModelProviderHealthLabel::Healthy, false)
    }

    #[test]
    fn provider_choices_group_without_hardcoded_vendors() {
        let catalog = vec![
            healthy("alpha", "a1"),
            healthy("alpha", "a2"),
            healthy("beta", "b1"),
        ];
        let providers = provider_choices(&catalog);
        assert_eq!(providers.len(), 2);
        assert_eq!(providers[0].provider_id, "alpha");
        assert_eq!(providers[1].provider_id, "beta");
        assert!(providers[0].selectable);
    }

    #[test]
    fn unavailable_row_not_selectable() {
        let mut row = healthy("p", "m");
        row.availability = ModelAvailability::Unavailable;
        assert!(!row_is_selectable(&row));

        let mut down = healthy("p", "m2");
        down.health = ModelProviderHealthLabel::Unavailable {
            last_error_redacted: "down".into(),
        };
        assert!(!row_is_selectable(&down));
    }

    #[test]
    fn option_choices_from_service_tiers_and_json_no_enums() {
        let mut row = healthy("p", "m");
        row.service_tiers = vec!["flex".into(), "priority".into()];
        row.provider_options = serde_json::json!({
            "region": ["us", "eu"],
            "flag": true
        });
        let choices = option_choices(&row);
        assert!(
            choices
                .iter()
                .any(|c| c.key == "service_tier" && c.value == "flex")
        );
        assert!(choices.iter().any(|c| c.key == "region" && c.value == "eu"));
        assert!(choices.iter().any(|c| c.key == "flag" && c.value == "true"));
    }

    #[test]
    fn step_back_from_model_returns_to_provider() {
        let mut state = ModelPickerState {
            step: ModelPickerStep::Model,
            draft_provider_id: Some("p".into()),
            ..Default::default()
        };
        assert!(state.step_back());
        assert_eq!(state.step, ModelPickerStep::Provider);
        assert!(!state.step_back());
    }
}
