# ACP Gateway Testing

## Способ 1: Unit-тесты (уже работают)

```bash
cargo test -p agentic-terminal-acp-gateway
```

10 тестов покрывают:
- Profile validation
- Gateway lifecycle (start/stop)
- Mock agent JSON-RPC protocol (initialize/session/cancel/exit)

## Способ 2: Mock agent binary (ручное тестирование)

### Сборка

```bash
cargo build --example mock_agent_bin -p agentic-terminal-acp-gateway
```

### Ручной запуск

```bash
./target/debug/examples/mock_agent_bin
```

Агент читает JSON-RPC из stdin, отвечает в stdout, логи в stderr:

```bash
# Initialize
echo '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}' | ./target/debug/examples/mock_agent_bin

# Session create
echo '{"jsonrpc":"2.0","id":2,"method":"session/create","params":{}}' | ./target/debug/examples/mock_agent_bin

# Exit
echo '{"jsonrpc":"2.0","id":3,"method":"exit","params":{}}' | ./target/debug/examples/mock_agent_bin
```

### Через AcpGateway

```rust
use agentic_terminal_acp_gateway::{AcpGateway, AcpProfile};
use std::path::PathBuf;

#[tokio::main]
async fn main() {
    let profile = AcpProfile::manual_executable(
        "mock",
        "Mock Agent",
        PathBuf::from("./target/debug/examples/mock_agent_bin"),
    );

    let mut gateway = AcpGateway::new(profile).unwrap();
    gateway.start().await.unwrap();
    
    // TODO: отправить JSON-RPC через gateway.child.stdin
    
    gateway.stop().await.unwrap();
}
```

## Способ 3: Настоящий ACP agent (когда появится)

1. Установить любой ACP-compatible agent (например `jcode-acp`)
2. Создать profile config:

```json
{
  "id": "jcode",
  "display_name": "JCode ACP Agent",
  "command": "/usr/local/bin/jcode-acp",
  "args": ["--stdio"],
  "credential_strategy": {"kind": "agent_owned"}
}
```

3. Запустить через gateway:

```rust
let profile: AcpProfile = serde_json::from_str(&config)?;
let mut gateway = AcpGateway::new(profile)?;
gateway.start().await?;
```

## Интеграционный тест

```bash
# Сначала собрать binary
cargo build --example mock_agent_bin -p agentic-terminal-acp-gateway

# Запустить интеграционный тест
cargo test -p agentic-terminal-acp-gateway --test integration_test -- --ignored
```

## Что НЕ делать

❌ Не искать чужие API keys в других репо  
❌ Не хранить raw credentials в profile JSON  
❌ Не коммитить секреты в config examples  

Для real provider используй:
- macOS Keychain (после Auth Center v0.3 шаг 4)
- System browser OAuth (после Auth Center v0.3 шаг 4)
- Local no-secret profile для localhost endpoints
