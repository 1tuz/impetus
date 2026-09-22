//! ACP SDK mock agent that advertises Model + ThoughtLevel config options
//! and records `session/set_config_option` for integration tests.
//!
//! Build: `cargo build -p impetus-acp-gateway --example acp_sdk_mock_agent`
//! Stdout = ACP JSON-RPC; stderr = logs. Last applied config written to
//! `$IMPETUS_ACP_MOCK_RECORD` (JSON) when set.

use agent_client_protocol::schema::v1::{
    AgentCapabilities, Implementation, InitializeRequest, InitializeResponse, NewSessionRequest,
    NewSessionResponse, PromptRequest, PromptResponse, SessionConfigKind, SessionConfigOption,
    SessionConfigOptionCategory, SessionConfigOptionValue, SessionConfigSelectOption,
    SessionConfigValueId, SessionId, SetSessionConfigOptionRequest, SetSessionConfigOptionResponse,
    StopReason,
};
use agent_client_protocol::{Agent, Result, Stdio};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct Applied {
    sets: Vec<(String, String)>,
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
    });
    if let Err(error) = std::fs::write(&path, payload.to_string()) {
        eprintln!("failed to write mock record: {error}");
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
            async move |_req: PromptRequest, responder, _connection| {
                responder.respond(PromptResponse::new(StopReason::EndTurn))?;
                Ok(())
            },
            agent_client_protocol::on_receive_request!(),
        )
        .connect_to(Stdio::new())
        .await
}
