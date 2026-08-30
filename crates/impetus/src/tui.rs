use anyhow::Result;

/// Launch the first-class standalone TUI client.
///
/// The UI remains a thin projection over `HarnessClient`; durable state,
/// policy, approvals and execution stay in `impetusd`.
pub async fn run(socket_path: &str) -> Result<()> {
    impetus_tui::run(socket_path).await
}
