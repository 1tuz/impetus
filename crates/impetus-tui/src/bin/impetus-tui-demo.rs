use anyhow::Result;
use impetus_tui::{RunOptions, run_with_options};

#[tokio::main]
async fn main() -> Result<()> {
    let mut options = RunOptions::from_env();
    options.demo = true;
    run_with_options("", options).await
}
