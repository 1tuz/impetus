//! Standalone terminal client for the Impetus harness.
//!
//! The crate is deliberately presentation-only. Durable state, policy,
//! approvals, tools and model execution stay in `impetusd`; this client talks
//! to them through `HarnessClient` and renders typed events.

mod app;
mod backend;
mod command;
mod composer;
mod markdown;
mod model;
mod render;
mod terminal;
mod theme;

use anyhow::Result;
use std::sync::Arc;

pub use model::RunOptions;

/// Launch the production TUI. Set `IMPETUS_TUI_DEMO=1` to run the same UI
/// against the deterministic in-process demonstration backend.
pub async fn run(socket_path: &str) -> Result<()> {
    run_with_options(socket_path, RunOptions::from_env()).await
}

pub async fn run_with_options(socket_path: &str, options: RunOptions) -> Result<()> {
    let backend: Arc<dyn backend::UiBackend> = if options.demo {
        Arc::new(backend::mock::MockBackend::new())
    } else {
        Arc::new(backend::impetus::ImpetusBackend::connect(socket_path).await?)
    };

    app::run(backend, options).await
}
