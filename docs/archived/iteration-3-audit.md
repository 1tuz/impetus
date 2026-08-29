# Аудит третьей итерации

Дата среза: 2026-08-26. Аудит описывает фактическое рабочее дерево после завершения Gate 0.1–0.4 второй итерации (commits до `923d133`). Iteration 3 превращает headless Harness в самостоятельный coding-agent продукт с полноценным TUI, профилями/SOUL, Failure Learning и нормальным UX для session DAG, checkpoints, swarm, risk и token metrics. Главное правило: TUI — только клиент; runtime/state/safety/context/learning остаются единственной собственностью harnessd.

## Метод

Проверено: workspace Cargo.toml, Cargo.toml каждого crate, исходники `crates/*/src`, наличие TUI-зависимостей (ratatui/crossterm) в графе, доступность исходников jcode для reuse, существующие события и client surface (IPC/CLI), текущая документация client boundary (root [ARCHITECTURE.md](../ARCHITECTURE.md), [GUI_UX.md](GUI_UX.md)).

## Ключевые факты

- **TUI в терминале отсутствует.** В графе зависимостей нет `ratatui`, `crossterm`, `tui`, `termion` или `ncurses`. GPUI app (`impetus-app`) — отдельный native reference client на Metal; он *владеет* `AgentRuntime` и SQLite напрямую (анти-паттерн, который Iteration 3 устраняет).
- **Client abstraction отсутствует.** Есть только низкоуровневый `IpcRequest`/`IpcResponse` (handshake + 7 команд) и простой CLI. Единого typed `HarnessClient` контракта (create/resume/subscribe/get_dag/get_checkpoints/get_usage/get_risk/get_profiles/set_profile/get_learning) нет.
- **Event model богатый и готов к UI.** `EventPayload` покрывает Session/Run/Intent/Plan/Tool/Agent/Approval/Notice; `SessionProjection` считает `last_sequence`, `tool_summaries`, `agent_output`, `pending_approvals`, `active_run_id`, `outcome`. Это готовый фундамент для transcript/diff/risk-представления.
- **jcode исходников в доступных путях нет.** На диске — только установленный бинарник, кэши и `.jcode` конфиги (`/opt/homebrew/Cellar/jcode`, `~/Library/...`, `/Users/antony/.jcode`). Прямое переиспользование Rust/Ratatui TUI-компонентов jcode невозможно без их исходников. Решение по п.2: использовать `ratatui`+`crossterm` напрямую и реализовать минимальный недостающий слой.
- **DAG/fork/checkpoint/subagent/swarm/token-cost/risk-state/profile/SOUL/memory/skill/failure-learning/retry/fingerprint/lesson** в коде отсутствуют. Зачатки telemetry: `RunEvent` несёт outcome (completed/failed/cancelled/interrupted_unknown), `ToolEvent::Finished` несёт `summary`, `NoticeEvent` несёт policy denied/allowed — достаточно для Risk UX и compact status, но не для token/cost чисел.
- **Client surface**: `Hello`, `CreateSession`, `Attach`, `ListSessions`, `Stream`, `Prompt`, `Cancel`, `Tool`. Нет `subscribe` (push), нет `get_dag`, `get_checkpoints`, `get_usage`, `get_risk_state`, `get_profiles`, `set_profile`, `get_learning_state`.

## Таблица аудита

| Feature | Current implementation | Reusable existing code | Target (Iteration 3) | Gap | Runtime cost | Priority | Risk |
| --- | --- | --- | --- | --- | --- | --- | --- |
| Harness TUI (terminal) | Нет; есть GPUI app, владеющий runtime | нет готового Rust TUI в репозитории | Полноценный TUI поверх typed protocol | Нет transcript/input/streaming/diff/sessions/status в терминале | +ratatui/crossterm render loop; bounded | P0 | средний |
| TUI ↔ runtime boundary | GPUI app держит `AgentRuntime`+SQLite | `IpcRequest/IpcResponse`, `AgentRuntime::attach` (read-only view) | TUI — клиент, не владеет state; reconnect восстанавливает session | Нужен typed client + reconnect + event subscription | низкий при правильной границе | P0 | высокий |
| Harness Client Contract | Только 7 примитивных IPC-команд + raw serde | `ipc.rs` типы, `AgentRuntime` read-API | Единый `HarnessClient`: create/resume/subscribe/list_agents/get_dag/get_diff/get_checkpoints/revert/get_usage/get_risk/get_profiles/set_profile/get_learning | Нет интерфейса и большинства методов | низкий (обёртка над IPC) | P0 | средний |
| Transports | Unix socket (harness CLI), in-memory (тесты) | `harness` daemon `UnixListener`, `cli` `UnixStream` | Unix Domain Socket + in-memory transport за одним контрактом | Нужен transport-neutral trait | низкий | P0 | низкий |
| Event subscription / streaming | `Stream { after_sequence }` pull; CLI читает разницу | `EventStore::list`, `AgentRuntime::attach` | push subscription с reconnect/backfill | Нет push-модели в IPC | низкий (poll/subscribe) | P0 | средний |
| Session DAG | Нет; только линейная `Vec<Event>` | `SessionProjection`, `Event.sequence` | compact tree view: fork/branch/compare/revert, immutable parent prefix | Нет DAG-структуры и fork events | низкий (проекция из events) | P1 | средний |
| Checkpoints | Нет; `RunEvent` лишь outcome | `ToolEvent::Finished` summary | checkpoint events + `/checkpoints`, `/revert N`, `/diff A..B` | Нет checkpoint events и storage | низкий | P1 | средний |
| Swarm / subagents | Нет; один supervisor + mock provider | `SessionSupervisor`, `RunEvent.run_id` | compact swarm view, per-agent transcript/cost/interrupt/cancel | Нет subagent events и scope | средний | P2 | высокий |
| Token / cost telemetry | Нет чисел; только outcome/summary | `RunEvent`, `ToolEvent` | compact status `sent|cached|cost` + `/usage` | Нет token-счётчиков и prompt-prefix метрик | низкий (счётчики в supervisor) | P1 | средний |
| Risk / Auto Mode UX | `NoticeEvent::PolicyDenied/Allowed`, `ApprovalEvent` | `PolicyEngine`, `ApprovalRequest` | ненавязчивые safe ticks + scoped Risk Gate BLOCKED вместо generic allow | Нет risk-state view и scoped capability UX | низкий | P1 | средний |
| Agent Profile / SOUL | Нет; policy статична | `SandboxScope`, `PolicyEngine` | лёгкий `AgentProfile` + `SOUL.md`, иерархия global→workspace→session→agent | Нет профильного модуля | низкий (stable prefix) | P1 | средний |
| Profile inheritance / overrides | Нет | `SandboxScope` (единственный scope) | наследование + overrides без копирования текста | Нет механизма наследования | низкий | P2 | низкий |
| SOUL compatibility | Нет (есть `AGENTS.md` проекта) | `AGENTS.md` уже в репозитории как project behavior | минимальное правило SOUL/AGENTS/SKILLS/MEMORY | Нет loader-а профилей | низкий | P2 | низкий |
| Failure Learning / Self-Repair | Нет | `EventStore` (durable log) | event-driven `FailureLearning`: detector→fingerprint→retry guard→lesson | Нет модуля и событий | ≈0 idle (event-driven) | P1 | высокий |
| Retry Guard (P0) | Нет | `ToolEvent`, `RunEvent`, `NoticeEvent` | обнаружение same tool+normalized input+same failure → `RETRY_BLOCKED` | Нет дедупа по fingerprint | низкий (bounded hot cache) | P0 | средний |
| Failure fingerprints | Нет | tool name, normalized command, exit code, stderr signature | компактное сопоставление без embeddings | нет | низкий | P1 | низкий |
| User correction / revert / deny signals | Нет | `IntentEvent`, `ApprovalEvent::Resolved(rejected)` | correction/revert/deny → learning signals | нет событий связи | низкий | P2 | средний |
| Lesson lifecycle / scope | Нет | `EventStore` | Observed→Candidate→Repeated→Validated→Promoted; scope session/workspace/tool/global | нет | ≈0 idle | P2 | средний |
| Memory overhead | N/A | `SqliteEventStore` + bounded index | event log + SQLite + bounded hot cache; без постоянного observer/embedding | нет | ≈0 | P0 | низкий |
| Keyboard / terminal robustness | Nет TUI | нет | Enter/Esc/Ctrl+C/Ctrl+L/PgUp/PgDn, resize/Unicode/paste/disconnect/reconnect/crash-restore | нет | низкий | P1 | средний |
| Render performance | N/A | `SessionProjection` уже агрегирует | virtualize viewport, bounded markdown/render caches, нет полного перерендера на токен | нет | низкий | P1 | средний |
| Slash commands | CLI: create/list/attach/stream/prompt/cancel/tool | CLI parser | `/help /sessions /new /resume /fork /agents /dag /diff /checkpoints /revert /profile /usage /context /risk /learning /tools /mcp /clear /compact /model` | нет | низкий | P2 | низкий |

## Reuse-решение для TUI (п.2)

Прямое переиспользование Rust/Ratatui TUI-компонентов jcode **невозможно**: исходники jcode недоступны (только бинарник/кэш/конфиги). Заимствовать можно только UX-идеи из описания интерфейсов (jcode/Codex/Kimi Code) как reference, не копируя runtime-код. Поэтому:

1. Используем `ratatui` + `crossterm` напрямую (renderer/viewport/markdown/streaming/tool/diff/input/dialogs/session-picker/permission/usage/status/history/autocomplete/slash — строим поверх них минимальный слой).
2. Не копируем объёмный связанный с jcode runtime ради UI.
3. UI не владеет agent state: всё состояние — в harnessd; TUI держит только bounded projection для видимого viewport.

## Что НЕ трогаем

- GPUI app остаётся optional reference client (его прямое владение runtime вне scope переделки; Iteration 3 добавляет параллельный terminal TUI-клиент, не ломая GPUI).
- Event schema v1, `SessionProjection`, `PolicyEngine`, `SandboxScope`, `AgentRuntime`, supervisor, mock provider, read-only tools, artifact store, Unix IPC — сохраняются как фундамент; добавляем поверх них client contract и недостающие события.
- Safety Policy, sandbox, permission system, credentials — вне зоны Self-Repair.

## Вывод

Iteration 2 дала прочный headless runtime и rich event model, но ни одного terminal TUI и ни одного из требуемых Iteration-3 механизмов (DAG/checkpoints/swarm/token/risk/profile/SOUL/failure-learning). Самое дешёвое и критичное: Phase 3B (единый `HarnessClient` + transports + subscription) и Phase 3F (Retry Guard). Затем 3C (TUI foundation) и 3E/3G (profiles/self-repair). Все новые модули — с ≈0 idle overhead и bounded caches.
