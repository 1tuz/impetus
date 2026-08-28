# Что сейчас делать

Подробный план: [docs/ROADMAP.md](docs/ROADMAP.md). Этот файл — только
очередь ближайших работ.

## Статус продукта

| Версия | Состояние | Что это значит |
| --- | --- | --- |
| v0.1 | Фундамент готов | events, SQLite, policy, approvals, mock runtime и базовый GPUI preview уже есть |
| v0.2 | Готов | standalone headless harness с real provider и безопасным execution path |
| v0.3 | В работе | structured clients и external agents |

> Native-window smoke для GPUI на чистом Mac остаётся открытым техническим
> хвостом v0.1. Он не блокирует текущую работу v0.2.

## Текущий релиз: v0.3 — structured clients и external agents

### Шаг 1 из 4 — IPC extension

- [x] Typed approval payload: diff preview, affected files, estimated scope.
- [x] Attachment references: artifact/output content по ID, не inline dump.
- [x] Backend/auth state events: provider health, keychain availability, token
  expiry warning.
- [x] Negotiated `Incompatible`: client/harness version mismatch handling с
  explicit fallback или upgrade prompt.

### Затем, шаг 2 из 4 — Zap structured adapter

- [x] CLI baseline (create/stream/cancel) работает.
- [x] Zap adapter binary: подписывается на harness events через polling, рендерит typed blocks.
- [x] OSC escape sequences: harness → Zap notification hooks.
- [x] Structured blocks protocol: diff с syntax highlighting, approval buttons, live agent chunks, attachments.
- [x] Live session status bar: Running / Idle / NeedsApproval.

### Затем, шаг 3 из 4 — ACP gateway

- [x] Manual executable profile: user указывает путь к agent CLI.
- [x] Mock agent: initialize/session/stream/cancel/permission/exit smoke.
- [ ] Agent-owned login: ACP backend не хранит credentials, только forwards
  prompts.

### Затем, шаг 4 из 4 — Auth Center contract

- [ ] Keychain reference profile для API keys.
- [ ] System-browser OAuth: URL открывается действием пользователя, callback
  handling.
- [ ] Local no-secret profile для localhost/mock providers.

### Дополнительно: Per-agent budget и compaction (OpenClaude референс)

- [x] BudgetConfig и BudgetState типы (max_turns, max_tokens, max_wall_time, reasoning_effort).
- [x] BudgetChecker enforcement (turn/token/wall time limits).
- [x] Unit-тесты budget logic.
- [ ] Интеграция budget в SessionSupervisor.
- [ ] Budget state events в IPC (для TUI/Zap live display).
- [ ] CompactionPolicy и separate compaction model.
- [ ] Auto-compaction на token threshold.

## Не сейчас

- GPUI native-window smoke и CI pane smoke — отдельные client checks.
- Custom terminal/TUI — только после Zap decision и зафиксированного
  неудовлетворённого requirement.
- v0.2 завершён: provider, execution seam, resource baselines, headless graph.
