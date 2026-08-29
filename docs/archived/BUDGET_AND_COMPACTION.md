# Per-Agent Budget и Compaction Model

Референс: OpenClaude per-agent step budget, separate compaction model, reasoning effort.

## Проблема

Без явных лимитов agent может:
- Исчерпать весь доступный context window
- Бесконечно цикличить на одной задаче
- Превысить cost budget без предупреждения
- Заблокировать другие sessions

## Архитектура

```text
Session
  ├─ session_id
  ├─ BudgetConfig
  │   ├─ max_turns: u32           ← per-session turn limit
  │   ├─ max_tokens: u64          ← total token budget
  │   ├─ max_wall_time: Duration  ← wall-clock timeout
  │   └─ reasoning_effort: Low | Medium | High
  │
  ├─ BudgetState (runtime)
  │   ├─ turns_used: u32
  │   ├─ tokens_used: u64
  │   ├─ started_at: Instant
  │   └─ compaction_count: u32
  │
  └─ CompactionPolicy
      ├─ trigger: TokenThreshold | TurnCount | Manual
      ├─ model: ModelRef (может отличаться от основной)
      └─ strategy: Summarize | DropOld | Selective
```

## Budget Config

### Defaults (наследуются из profile)

```rust
pub struct BudgetConfig {
    pub max_turns: Option<u32>,       // None = unlimited
    pub max_tokens: Option<u64>,      // None = unlimited
    pub max_wall_time: Option<Duration>, // None = unlimited
    pub reasoning_effort: ReasoningEffort,
}

pub enum ReasoningEffort {
    Low,    // fast, cheap iterations
    Medium, // balanced
    High,   // extended thinking (o1/o3-like)
}
```

### Per-session override

```rust
// Client IPC v0.3+
{
  "method": "session/create",
  "params": {
    "prompt": "...",
    "budget": {
      "max_turns": 10,
      "max_tokens": 100000,
      "max_wall_time": "5m",
      "reasoning_effort": "medium"
    }
  }
}
```

## Budget Enforcement

### Turn limit

```rust
impl Session {
    fn check_turn_budget(&self) -> Result<(), BudgetError> {
        if let Some(max) = self.budget_config.max_turns {
            if self.budget_state.turns_used >= max {
                return Err(BudgetError::TurnLimitExceeded {
                    limit: max,
                    used: self.budget_state.turns_used,
                });
            }
        }
        Ok(())
    }
}
```

### Token limit

```rust
impl Session {
    fn check_token_budget(&self, request_tokens: u64) -> Result<(), BudgetError> {
        if let Some(max) = self.budget_config.max_tokens {
            let projected = self.budget_state.tokens_used + request_tokens;
            if projected > max {
                return Err(BudgetError::TokenLimitExceeded {
                    limit: max,
                    used: self.budget_state.tokens_used,
                    requested: request_tokens,
                });
            }
        }
        Ok(())
    }
}
```

### Wall time limit

```rust
impl Session {
    fn check_wall_time_budget(&self) -> Result<(), BudgetError> {
        if let Some(max) = self.budget_config.max_wall_time {
            let elapsed = self.budget_state.started_at.elapsed();
            if elapsed > max {
                return Err(BudgetError::WallTimeExceeded {
                    limit: max,
                    elapsed,
                });
            }
        }
        Ok(())
    }
}
```

## Compaction Model

### Trigger

Automatic compaction срабатывает когда:
1. Context приближается к window limit (80% порог)
2. Каждые N turns (configurable, default 50)
3. Manual trigger через IPC

### Separate model

Compaction может использовать другую модель:
- Основная session: o1-preview (дорогая, extended thinking)
- Compaction: claude-3-5-sonnet (быстрая, дешёвая)

```rust
pub struct CompactionPolicy {
    pub trigger: CompactionTrigger,
    pub model: Option<ModelRef>, // None = use session model
    pub strategy: CompactionStrategy,
}

pub enum CompactionTrigger {
    TokenThreshold { percent: u8 }, // 80 = compact at 80% full
    TurnCount(u32),
    Manual,
}

pub enum CompactionStrategy {
    Summarize,    // LLM-generated summary
    DropOld,      // drop oldest N turns
    Selective,    // keep important events, summarize noise
}
```

### Implementation sketch

```rust
async fn compact_context(
    session: &mut Session,
    policy: &CompactionPolicy,
) -> Result<CompactedContext, CompactionError> {
    // 1. Collect events for compaction
    let events_to_compact = session.events_since_last_compaction();

    // 2. Choose model
    let model = policy.model.as_ref().unwrap_or(&session.model);

    // 3. Generate summary
    let summary = match policy.strategy {
        CompactionStrategy::Summarize => {
            llm_summarize(model, &events_to_compact).await?
        }
        CompactionStrategy::DropOld => {
            // Just drop, no LLM call
            format!("Dropped {} old events", events_to_compact.len())
        }
        CompactionStrategy::Selective => {
            selective_summarize(model, &events_to_compact).await?
        }
    };

    // 4. Record compaction event
    session.record_event(Event::ContextCompacted {
        events_compacted: events_to_compact.len(),
        summary: summary.clone(),
        model: model.clone(),
        timestamp: Utc::now(),
    });

    Ok(CompactedContext { summary })
}
```

## Context/Token TUI UX

Zap adapter и CLI показывают live budget state:

```
Session abc123 [Turn 7/10] [Tokens 45k/100k] [Time 2m/5m]
  Reasoning: Medium
  Compactions: 1 (last: 2m ago)
  
  [████████████████████░░░░] 80% context used
  
  > prompt here
```

IPC event:

```json
{
  "method": "session/budget_update",
  "params": {
    "session_id": "abc123",
    "turns_used": 7,
    "turns_limit": 10,
    "tokens_used": 45000,
    "tokens_limit": 100000,
    "wall_time_elapsed": "2m",
    "wall_time_limit": "5m",
    "compaction_count": 1,
    "context_used_percent": 80
  }
}
```

## Deterministic Session Reports

После завершения session (success/timeout/budget exceeded) генерируется report:

```json
{
  "session_id": "abc123",
  "outcome": "budget_exceeded",
  "reason": "turn_limit",
  "budget": {
    "max_turns": 10,
    "max_tokens": 100000,
    "max_wall_time": "5m"
  },
  "usage": {
    "turns_used": 10,
    "tokens_used": 87000,
    "wall_time": "3m42s",
    "compactions": 2
  },
  "model_calls": [
    {"model": "o1-preview", "turns": 8, "tokens": 78000},
    {"model": "claude-3-5-sonnet", "turns": 2, "tokens": 9000, "role": "compaction"}
  ]
}
```

Report сохраняется в SQLite и доступен через IPC/CLI.

## Persistent External Worker via tmux/Channels

Референс: OpenClaude использует tmux для долгоживущих agent sessions.

**Мы НЕ берём это сейчас**, потому что:
- v0.2 уже имеет durable sessions через SQLite
- Zap является primary client
- tmux integration — optional transport для remote/SSH (v0.6+)

Но architecture позволяет позднее:
- Запускать harness в tmux session
- Attach/detach через IPC socket
- Reconnect без потери state

## Roadmap

### v0.3 (текущий)
- [ ] BudgetConfig и BudgetState в Session
- [ ] Budget enforcement (turn/token/wall time)
- [ ] Budget exceeded outcome в IPC
- [ ] Budget state events для TUI

### v0.4 (после Auth Center)
- [ ] CompactionPolicy и CompactionTrigger
- [ ] Separate compaction model
- [ ] Auto-compaction на token threshold
- [ ] Compaction event в trace

### v0.5+
- [ ] Deterministic session reports
- [ ] Cost tracking per model
- [ ] Budget alerts/warnings

### v0.6+ (remote transport)
- [ ] tmux/screen integration как transport
- [ ] Persistent worker pattern для SSH

## Compatibility

- Client IPC v0.2: игнорирует `budget` params (graceful degradation)
- Client IPC v0.3+: поддерживает budget config и state events
- ACP gateway: budget не форвардится external agent (его собственная ответственность)

## Testing

```rust
#[tokio::test]
async fn session_respects_turn_budget() {
    let config = BudgetConfig {
        max_turns: Some(3),
        ..Default::default()
    };
    let mut session = Session::new_with_budget(config);

    session.turn().await.unwrap(); // turn 1
    session.turn().await.unwrap(); // turn 2
    session.turn().await.unwrap(); // turn 3
    
    let err = session.turn().await.unwrap_err();
    assert!(matches!(err, BudgetError::TurnLimitExceeded { .. }));
}
```

## Источники

- OpenClaude: per-agent budget, compaction model (референс UX и patterns)
- Qwen Code: immutable fork и shared cache prefix
- Claude Code: auto-mode reasoning effort
- ACP: negotiated capabilities и token usage reporting
