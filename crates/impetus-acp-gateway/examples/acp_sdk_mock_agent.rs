//! ACP SDK mock agent that advertises Model + ThoughtLevel config options
//! and records `session/set_config_option` for integration tests.
//!
//! When `IMPETUS_ACP_MOCK_PERMISSION=1`, each prompt issues a deterministic
//! `session/request_permission` (Edit + allow-once) and records the client's
//! outcome under `$IMPETUS_ACP_MOCK_RECORD`.
//!
//! Build: `cargo build -p impetus-acp-gateway --example acp_sdk_mock_agent`
//! Stdout = ACP JSON-RPC; stderr = logs. Last applied config / permission
//! outcome written to `$IMPETUS_ACP_MOCK_RECORD` (JSON) when set.

use agent_client_protocol::schema::v1::{
    AgentCapabilities, Implementation, InitializeRequest, InitializeResponse, NewSessionRequest,
    NewSessionResponse, PermissionOption, PermissionOptionKind, PromptRequest, PromptResponse,
    RequestPermissionOutcome, RequestPermissionRequest, SessionConfigKind, SessionConfigOption,
    SessionConfigOptionCategory, SessionConfigOptionValue, SessionConfigSelectOption,
    SessionConfigValueId, SessionId, SetSessionConfigOptionRequest, SetSessionConfigOptionResponse,
    StopReason, ToolCallLocation, ToolCallUpdate, ToolCallUpdateFields, ToolKind,
};
use agent_client_protocol::{Agent, Result, Stdio};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct Applied {
    sets: Vec<(String, String)>,
    permission_outcomes: Vec<String>,
}

fn initial_config_options() -> Vec<SessionConfigOption> {
    vec![
        SessionConfigOption::select(
            "model",
            "Model",
            "mock-default",
            vec![
                SessionConfigSelectOption::new("mock-default", "Mock default"),
                SessionConfigSelectOption::new("mock-fast", "Mock fast"),
                SessionConfigSelectOption::new("mock-smart", "Mock smart"),
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

fn record_path() -> Option<PathBuf> {
    std::env::var_os("IMPETUS_ACP_MOCK_RECORD").map(PathBuf::from)
}

fn permission_enabled() -> bool {
    matches!(
        std::env::var("IMPETUS_ACP_MOCK_PERMISSION").as_deref(),
        Ok("1") | Ok("true") | Ok("yes")
    )
}

fn permission_target() -> PathBuf {
    std::env::var_os("IMPETUS_ACP_MOCK_TARGET")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("e2e-acp-edit.txt"))
}

fn flush_record(applied: &Applied) {
    let Some(path) = record_path() else {
        return;
    };
    let payload = serde_json::json!({
        "sets": applied
            .sets
            .iter()
            .map(|(id, value)| serde_json::json!({"config_id": id, "value": value}))
            .collect::<Vec<_>>(),
        "permission_outcomes": applied.permission_outcomes,
    });
    if let Err(error) = std::fs::write(&path, payload.to_string()) {
        eprintln!("failed to write mock record: {error}");
    }
}

fn outcome_label(outcome: &RequestPermissionOutcome) -> String {
    match outcome {
        RequestPermissionOutcome::Selected(selected) => {
            format!("selected:{}", selected.option_id.0)
        }
        RequestPermissionOutcome::Cancelled => "cancelled".into(),
        _ => "other".into(),
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    eprintln!("acp_sdk_mock_agent starting");
    let applied = Arc::new(Mutex::new(Applied::default()));
    let options = Arc::new(Mutex::new(initial_config_options()));

    Agent
        .builder()
        .name("impetus-acp-sdk-mock")
        .on_receive_request(
            async move |initialize: InitializeRequest, responder, _connection| {
                responder.respond(
                    InitializeResponse::new(initialize.protocol_version)
                        .agent_capabilities(AgentCapabilities::new())
                        .agent_info(Implementation::new("impetus-acp-sdk-mock", "0.1.0")),
                )?;
                Ok(())
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            {
                let options = Arc::clone(&options);
                async move |_req: NewSessionRequest, responder, _connection| {
                    let opts = options.lock().expect("options").clone();
                    responder.respond(
                        NewSessionResponse::new(SessionId::new(uuid::Uuid::new_v4().to_string()))
                            .config_options(opts),
                    )?;
                    Ok(())
                }
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            {
                let applied = Arc::clone(&applied);
                let options = Arc::clone(&options);
                async move |req: SetSessionConfigOptionRequest, responder, _connection| {
                    let config_id = req.config_id.to_string();
                    let value = match &req.value {
                        SessionConfigOptionValue::ValueId { value } => value.to_string(),
                        SessionConfigOptionValue::Boolean { value } => value.to_string(),
                        _ => "unknown".into(),
                    };
                    eprintln!("set_config_option {config_id}={value}");
                    {
                        let mut guard = applied.lock().expect("applied");
                        guard.sets.push((config_id.clone(), value.clone()));
                        flush_record(&guard);
                    }
                    {
                        let mut opts = options.lock().expect("options");
                        if let Some(opt) = opts.iter_mut().find(|o| o.id.to_string() == config_id)
                            && let SessionConfigKind::Select(select) = &mut opt.kind
                        {
                            select.current_value = SessionConfigValueId::new(value.as_str());
                        }
                    }
                    let current = options.lock().expect("options").clone();
                    responder.respond(SetSessionConfigOptionResponse::new(current))?;
                    Ok(())
                }
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            {
                let applied = Arc::clone(&applied);
                async move |req: PromptRequest, responder, connection| {
                    if permission_enabled() {
                        let target = permission_target();
                        eprintln!("request_permission edit {}", target.display());
                        let tool_call = ToolCallUpdate::new(
                            "tc-edit-1",
                            ToolCallUpdateFields::new()
                                .kind(ToolKind::Edit)
                                .title("Edit file")
                                .locations(vec![ToolCallLocation::new(target)]),
                        );
                        let options = vec![
                            PermissionOption::new(
                                "allow-once",
                                "Allow once",
                                PermissionOptionKind::AllowOnce,
                            ),
                            PermissionOption::new(
                                "reject-once",
                                "Reject once",
                                PermissionOptionKind::RejectOnce,
                            ),
                        ];
                        let perm = RequestPermissionRequest::new(
                            req.session_id.clone(),
                            tool_call,
                            options,
                        );
                        let response = connection.send_request(perm).block_task().await?;
                        let label = outcome_label(&response.outcome);
                        eprintln!("permission_outcome {label}");
                        let mut guard = applied.lock().expect("applied");
                        guard.permission_outcomes.push(label);
                        flush_record(&guard);
                    }
                    responder.respond(PromptResponse::new(StopReason::EndTurn))?;
                    Ok(())
                }
            },
            agent_client_protocol::on_receive_request!(),
        )
        .connect_to(Stdio::new())
        .await
}
