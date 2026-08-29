//! Standalone mock ACP agent binary для ручного тестирования gateway.
//!
//! Запускается через AcpGateway, отвечает на JSON-RPC через stdin/stdout.

use impetus_acp_gateway::mock::{JsonRpcMessage, JsonRpcRequest, MockAgent};
use std::io::{self, BufRead, Write};

fn main() -> anyhow::Result<()> {
    eprintln!("Mock ACP agent starting (stderr for logs, stdout for JSON-RPC)");

    let mut agent = MockAgent::new();
    let stdin = io::stdin();
    let mut stdout = io::stdout();

    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }

        eprintln!("Received: {}", line);

        // Parse JSON-RPC request
        let request: JsonRpcRequest = match serde_json::from_str(&line) {
            Ok(req) => req,
            Err(e) => {
                eprintln!("Parse error: {}", e);
                continue;
            }
        };

        // Handle request
        let response = match agent.handle_request(request) {
            Ok(resp) => resp,
            Err(e) => {
                eprintln!("Handler error: {}", e);
                continue;
            }
        };

        // Send response to stdout
        let json = serde_json::to_string(&JsonRpcMessage::Response(response))?;
        writeln!(stdout, "{}", json)?;
        stdout.flush()?;

        eprintln!("Sent response");
    }

    eprintln!("Mock ACP agent exiting");
    Ok(())
}
