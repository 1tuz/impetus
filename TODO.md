# Что сейчас делать

Подробный план: [docs/ROADMAP.md](docs/ROADMAP.md). Этот файл — только
очередь ближайших работ.

## Статус продукта

| Версия | Состояние | Что это значит |
| --- | --- | --- |
| v0.1 | Фундамент готов | events, SQLite, policy, approvals, mock runtime и базовый GPUI preview уже есть |
| v0.2 | В работе | доводим самостоятельный headless harness до реального provider и безопасного execution path |

> Native-window smoke для GPUI на чистом Mac остаётся открытым техническим
> хвостом v0.1. Он не блокирует текущую работу v0.2.

## Текущий релиз: v0.2 — standalone harness

### Шаг 1 из 3 — подключить модель

- [x] Local/no-secret OpenAI-compatible streaming adapter: durable chunks,
  cancellation, retry budget и provider health state.
- [x] Opaque reference на macOS Keychain для HTTPS provider profile; raw token
  не добавлять в config, IPC или events.
- [x] Redaction/export tests: raw token, OAuth callback и credential bytes не
  попадают в SQLite, events, logs, tracing или fixtures.
- [x] Проверить `task verify`, `task ci:list`, `task ci:local`, `task security`.

### Затем, шаг 2 из 3 — закрыть путь выполнения

- [x] Normalized effect только через Policy → Approval → Sandbox → Capability
  → Execution. Для mutating effect остаётся durable approval → execution path
  через versioned client IPC; не включать capability до отдельного этапа.
- [x] Exact action fingerprint/revision; stale approval reject; unavailable
  sandbox fail closed для durable approval → execution path следующего этапа.
- [x] macOS sandbox spike: Seatbelt proof ограничивает child canonical
  workspace; write/process/network capabilities не включены.

### Затем, шаг 3 из 3 — зафиксировать пределы

- [x] RSS, queue, artifact/output bytes, restart/cancel latency и
  context/token baseline.
- [x] Headless dependency graph без GPUI, Metal, PTY и ANSI renderer.

## Не сейчас

- GPUI native-window smoke и CI pane smoke — отдельные client checks.
- Zap structured path, ACP и terminal/TUI — только после v0.2 и Zap decision.
