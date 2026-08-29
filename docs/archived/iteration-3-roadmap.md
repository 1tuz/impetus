# Historical audit: идеи третьей итерации

> Этот файл **не active roadmap**. Канонический порядок работ находится в
> [ROADMAP.md](../ROADMAP.md). Он сохраняет полезный аудит идей (client contract,
> TUI, profiles, learning), но не разрешает начать TUI раньше v0.2 provider /
> execution gates и Zap decision.

Опирается на `iteration-3-audit.md`. Iteration 3 делает Harness самостоятельным coding-agent продуктом: полноценный terminal TUI (клиент), единый Harness Client Contract, Session DAG/checkpoints/swarm/risk/token UX, Agent Profile/SOUL и event-driven Failure Learning. Граница неизменна: TUI — только клиент, runtime/state/safety/context/learning принадлежат harnessd.

## Архитектура (цель)

```text
Zap / Terminal.app / iTerm2 / SSH
        │
        ▼
   harness TUI  (ratatui + crossterm, клиент)
        │  Harness Client Contract (typed)
        ▼
     harnessd  (владеет runtime/state/safety/context/learning)
        │
   ┌──────┼───────────┐
   │      │           │
 Agents Context     Safety
   │      │           │
   └── Event Log ─────┘
        │
   Failure Learning (event-driven, ≈0 idle)
```

## Delta относительно требований Iteration 3

- **Есть (переиспользуем):** rich `EventPayload`, `SessionProjection`, `PolicyEngine`/`SandboxScope`, `AgentRuntime` (read view через `attach`), Unix IPC + daemon + CLI, read-only tools + artifact store, mock provider + supervisor, durable SQLite event log.
- **Нет (новое):** terminal TUI; typed `HarnessClient` контракт; push subscription; DAG/fork/checkpoint события; subagent/swarm события; token/cost счётчики; risk-state view; `AgentProfile`/`SOUL.md` loader + иерархия; `FailureLearning` (fingerprint/retry-guard/lessons); slash-команды; keyboard/terminal-robustness; bounded render caches.
- **Reuse-решение:** jcode исходников нет → `ratatui`+`crossterm` напрямую; UX-идеи jcode/Codex/Kimi берём как reference, runtime-код не копируем.

## Фазы

### Phase 3B — Harness Client Contract
- `HarnessClient` trait (transport-neutral): `create_session`, `resume_session`, `send_message`, `soft_interrupt`, `hard_cancel`, `fork`, `subscribe_events`, `list_agents`, `get_dag`, `get_diff`, `get_checkpoints`, `revert`, `get_usage`, `get_risk_state`, `get_profiles`, `set_profile`, `get_learning_state`.
- Транспорты: `UnixSocketTransport` + `InMemoryTransport` за одним контрактом.
- Push subscription: `Subscribe`/`Events` с sequence backfill; reconnect без дублей.
- Расширение `IpcRequest/IpcResponse` новыми методами и `Incompatible` при mismatch версии.
- Критерий: один и тот же контракт используется в TUI, in-memory тестах и будущих Zap/IDE адаптерах.

### Phase 3C — TUI Foundation
- `ratatui`+`crossterm` event loop; layout: header (project/branch/model/context/cost) + transcript + input.
- Transcript: streaming messages, tool calls (`✓ read`, `cargo test FAILED/PASS`), diff presentation, markdown render.
- Input/editor: Enter send, Esc close dialog, Ctrl+C safe cancel, Ctrl+L redraw, PgUp/PgDn history.
- Sessions picker; status overlay; autocomplete + slash-command parser.
- Критерий: нет заметного мерцания; нет полного перерендера на каждый token; bounded caches.

### Phase 3D — Advanced TUI
- Checkpoints view + `/revert N`, `/diff A..B`, scoped destructive confirm.
- Session DAG compact tree (fork/branch/compare/discard/restore); иммутабельный parent prefix.
- Swarm/subagents: compact tree, per-agent transcript/cost/files/checkpoint/interrupt/cancel; не показывать постоянно при одном агенте.
- Usage overlay + `/usage`; Risk Gate `BLOCKED` вместо generic allow.
- Artifacts view; soft-interrupt UX.

### Phase 3E — Profiles
- Лёгкий модуль `AgentProfile` + `SOUL.md` loader; короткий stable prompt prefix (provider cache).
- Иерархия global→workspace→session→agent через наследование + overrides (без копирования текста).
- TUI: `/profile`, `/profile list`, `/profile show`, `/agent profile worker-N <name>`.
- SOUL НЕ управляет sandbox/policy/network/secrets; Safety всегда выше persona.
- Совместимость: минимальное правило SOUL (identity) / AGENTS (project behavior) / SKILLS (capabilities) / MEMORY (knowledge).

### Phase 3F — Self-Repair P0
- `FailureLearning`: detector → fingerprint → retry guard → candidate lesson.
- **Retry Guard:** нормализованный (tool + normalized command + exit code + stderr signature + affected resource + workspace revision); после N идентичных — `RETRY_BLOCKED` с инструкцией сменить стратегию. N configurable. Не блокирует, если relevant state изменился.
- Сигналы: user correction, revert агент-тура/checkpoint, Risk DENIED (не повторять запрещённый путь в рамках session/task).
- Runtime cost ≈0 (только на новых событиях; bounded hot cache).

### Phase 3G — Learning
- Candidate lessons с lifecycle Observed→Candidate→Repeated→Validated→Promoted.
- Scope: session/workspace/tool/language/global; workspace-promotion только для repo-конвенций.
- Post-session on-demand анализ только интересных failure events (failed tools, duplicate retries, corrections, reverts, failed tests, risk denies, abandoned branches). Без интересных событий — не запускать.
- Дешёвая модель только при реальной ценности; deterministic logic первична.
- `/learning`: Lessons/Candidates/Rejected/Evidence.
- Self-Repair НЕ меняет Safety/Permanent/Harness binary; только Improvement Proposal + patch + tests в dev-воркфлоу.

### Phase 3H — Optimization
- Профилирование; memory budgets; bounded markdown/render caches; TUI не держит полный event history как widgets; старые данные — из event store.
- Гарантия: learning/profile idle ≈0; TUI не создаёт второй runtime; session state остаётся в harnessd.

## Список 5–10 изменений с максимальной отдачей

1. **Единый `HarnessClient` + transports (3B).** Развязывает TUI от implementation details harnessd; переиспользуется во всех клиентах. Overhead: низкий (тонкая обёртка над IPC; в памяти — прямой вызов).
2. **Retry Guard + failure fingerprints (3F).** Самая дешёвая и критичная часть; останавливает бесконечный повтор неуспешной стратегии. Overhead: низкий (bounded LRU fingerprints в памяти + запись в event log).
3. **TUI foundation (3C): transcript + streaming + tool/diff + input + sessions.** Делает продукт usable в терминале. Overhead: +ratatui/crossterm render loop; bounded viewport/markdown caches; нет перерендера на токен.
4. **Event subscription / reconnect (3B).** Вместо pull `Stream` — push с backfill; закрытие/краш TUI не теряет session truth. Overhead: низкий (poll/subscribe поверх существующего store).
5. **Token/cost telemetry в supervisor (3D).** Счётчики sent/cached/calls + prompt-prefix cache; compact status + `/usage`. Overhead: низкий (инкременты в `SessionProjection`/runtime counters).
6. **Checkpoints + Session DAG (3D).** Новые события `Checkpoint`/`Fork`; compact tree и `/revert`, `/diff`. Overhead: низкий (проекция из events; storage уже есть).
7. **Risk Gate UX (3D).** Scoped `BLOCKED` вместо generic allow; ненавязчивые safe ticks. Overhead: низкий (на базе `NoticeEvent`/`ApprovalEvent`, уже есть).
8. **AgentProfile/SOUL.md (3E).** Короткий stable prefix; иерархия; TUI selector. Overhead: ≈0 idle (только при смене профиля/agent).
9. **Failure Learning module + `/learning` (3G).** Event-driven, lifecycle lessons, post-session on-demand. Overhead: ≈0 idle; bounded hot cache.
10. **Swarm/subagent view (3D).** Compact tree + per-agent управление; скрыто при одном агенте. Overhead: средний (нужны subagent events + scope), поэтому P2.

## Оценка overhead (сводно)

- **Idle:** TUI ≈0 (не держит runtime; bounded caches); Failure Learning ≈0 (event-driven, без observer/embedding); Profile ≈0 (stable prefix, без отдельного LLM-вызова).
- **Render:** virtualize viewport, bounded markdown/render caches, нет полного перерендера на token.
- **Memory:** все новые caches bounded; полный event history — в harnessd/SQLite; TUI читает видимое окно по требованию.
- **Safety:** Self-Repair не может менять Safety/Permanent/Harness binary; только Improvement Proposal в dev-воркфлоу.

## Acceptance criteria (ссылка)

См. п.40 требований Iteration 3 (23 пункта). Phase 3B покрывает критерии 2,3,4,5; 3C — 1,5,6; 3D — 7,8,9,10,11; 3E — 12,13,14; 3F/3G — 15–20; 3H — 21,22,23 (без регрессии Iteration 2).

## Historical recommendation

Этот порядок был предложением для отдельной третьей итерации, не разрешением
немедленно строить TUI. Если когда-либо начнётся соответствующий scope, сначала
нужны v0.2 provider/execution gates и Zap decision из [ROADMAP.md](../ROADMAP.md).
