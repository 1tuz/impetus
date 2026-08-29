# Client UX: Zap, CLI и optional GPUI

## Главный принцип

Harness не владеет окном. Он отдаёт versioned typed events и принимает typed user decisions. Zap — основной терминальный клиент; headless CLI — обязательный reference client; GPUI app — optional экспериментальная поверхность. Любой подключённый к harness клиент показывает одинаковую session truth и не реализует собственную policy state machine; текущий GPUI CI preview к harness session ещё не подключён.

Прямая shell-команда в обычной Zap tab остаётся командой терминала и не попадает в harness audit. Harness task начинается только после явного запуска/attach CLI или structured client request.

## Уровни интеграции Zap

### 1. Baseline без форка

Пользователь запускает harness CLI в Zap. В v0.2 CLI показывает session status, durable event stream и bounded read-only tool results. Diff и approval prompt относятся к structured IPC extension v0.3. Zap отвечает за PTY/ANSI/tabs/scrollback; harness — за task lifecycle, provider, tools, policy и durable history.

### 2. Structured adapter или личный fork

Zap подключается к local IPC и отображает typed Blocks. Клиент может менять layout, темы и навигацию, но не получает SQLite connection, Keychain bytes или raw capability handle. Approval отправляет `request_id + revision + decision`; устаревший target инвалидирует решение.

### 3. Optional GPUI reference client

Существующий native preview остаётся площадкой для contract prototyping, themes и GitLab CI pane. Наличие visual placeholder не означает готовый harness или terminal implementation.

## Компактная GitLab CI-панель

CI pane — уже добавленный независимый client experiment. Его кнопки являются прямыми user actions вне harness task/audit: **Run local** запускает `gitlab-ci-local`, **Remote status** читает structured JSON через авторизованный `glab`. Pane не владеет SQLite или policy state. Обе ветки строят одинаковые stage/job rows: `✓ success`, `✕ failed`, `● running`, `○ pending`, `- skipped`, `? unknown`.

`↑`/`↓` выбирают job, `Enter` показывает compact error, повторный `Enter` или `l` раскрывает log, `r` обновляет remote status, `q` закрывает панель. Во время local run последние 12 строк имеют semantic color: command/running, success, warning и failure; raw log остаётся доступен отдельно. `retry/cancel` отсутствуют до exact approval. Live buffer в 600 строк — временная client-защита, не часть harness protocol и не gate v0.2.

## Общая информационная модель

Каждый клиент должен отвечать на четыре вопроса:

1. **Где:** workspace, local/remote profile, session ID.
2. **Что происходит:** текущий run/step, streaming или waiting.
3. **Что требует человека:** exact diff/command/host/file и reason.
4. **Что произошло:** durable terminal outcome, evidence и source range.

Ни spinner, ни цвет не являются единственным сигналом. Неизвестный outcome после crash показывается как `Interrupted — outcome unknown`, не `Completed`.

## Blocks как protocol projection

| Kind | Содержимое | Типичные действия |
| --- | --- | --- |
| Intent | фраза пользователя + workspace | retry/fork |
| Plan | шаги, scope, status | evidence/cancel |
| Tool | capability, target, bounded result | inspect output |
| Approval | exact diff/command/host/file + revision | approve once/reject |
| Agent | streaming/final response | copy/fork |
| Notice | disconnect, recovery, completion | attach/open session |
| Summary | compaction source range + version | show sources |

Block IDs и состояния приходят из harness. Клиент не создаёт параллельную историю и не переименовывает `failed/interrupted` в success.

## Рекомендуемый structured layout

Это контракт для Zap fork/adapter или optional GPUI client, не требование строить новый terminal emulator:

```text
┌ Client toolbar ────────────────────────────────────────────────────────────┐
│ Workspace · Session · Backend · Manual/Safe Auto · Connection            │
├──────────── Existing terminal ───────────┬──────── Harness Blocks ────────┤
│ Zap shell / selected client context      │ Plan                           │
│                                          │ Tool · read-only               │
│                                          │ Approval · exact target        │
│                                          │ [Inspect] [Approve] [Reject]   │
├──────────────────────────────────────────┴────────────────────────────────┤
│ Destination: Harness   Ask…                                  Send/Cancel  │
└───────────────────────────────────────────────────────────────────────────┘
```

На узкой ширине Blocks могут быть overlay или отдельной surface. Эти breakpoint/layout решения принадлежат клиенту и не входят в IPC.

## Composer и attachments

Destination всегда виден: `Harness`, `Command palette` или `Terminal paste`. Обычный natural-language text создаёт harness intent. Многострочная shell-вставка не исполняется как side effect обычной кнопкой Send: показываются preview, target и отдельное user action.

File/paste image/capture сначала создают attachment refs с preview, типом, размером, scan state и exact backend. Send недоступен до negotiation. Фоновый capture и автоматическое добавление найденного агентом файла запрещены.

## Approval UX

- Показывать action kind, exact target, revision и причину policy.
- `Enter` сам по себе не подтверждает рискованный action; нужен explicit control/command.
- Изменившийся target инвалидирует старое approval.
- Client timeout/disconnect не означает approve или reject; harness сохраняет `pending` либо переводит в documented terminal state.
- Agent output не может программно получить focus/selection подтверждающего control.

CLI обязан предоставлять те же данные в текстовом виде; красивый Block не является условием безопасности.

## Состояния

| Поверхность | Обязательные состояния |
| --- | --- |
| Client connection | connecting, connected, incompatible, disconnected, reconnecting |
| Harness run | planning, streaming, waiting approval, cancelling, cancelled, completed, failed, interrupted |
| Approval | pending, inspecting, approved, rejected, expired, invalidated |
| Safe Auto | manual, active, checking, blocked, paused, reviewer unavailable |
| Attachment | inspecting, ready, unsupported, sensitive, oversize, uploading, sent, failed, removed |
| Backend profile | connected, needs login, unavailable, incompatible, crashed |
| Block output | empty, partial, complete, redacted, truncated + durable source ref |

## Focus и keyboard boundary

Для integrated terminal client:

1. `Ctrl-C` в terminal focus уходит PTY; в harness composer вызывает typed cancel только отдельным mapped action.
2. `Esc` закрывает client overlay; terminal получает Escape byte только при terminal focus.
3. Approval не крадёт focus и не перехватывает обычный terminal Enter.
4. Agent не может переводить focus в approval или terminal input.
5. Client обязан различать `send prompt`, `paste terminal` и `approve effect`.

## Темы

Существующий GPUI preview содержит 10 terminal palettes. Они сохраняются как client asset/reference, но не являются deliverable harness v0.2. В Zap темами управляет Zap. Policy states используют semantic labels и не зависят от ANSI red/green.

## Что переиспользовать

| Поверхность | Основа | Решение |
| --- | --- | --- |
| основной terminal UX | Zap | использовать напрямую; при необходимости личный fork |
| contract/recovery client | headless Rust CLI | реализовать первым в v0.2 |
| base client transport | versioned local IPC | v0.2, client-independent |
| structured approval/diff/attachment states | IPC extension | v0.3 |
| external coding-agents | ACP Rust SDK | adapter внутри harness |
| optional native prototype | текущий GPUI-CE app | не расширять до PTY без go/no-go |
| local secrets | macOS Keychain adapter | harness хранит opaque reference |
| terminal emulator fallback | `portable-pty` + проверенный ANSI engine | только optional backlog после доказанного gap |

## Проверяемость clients

Сначала тестируется event → projection и IPC transcript, затем конкретный UI:

- CLI golden tests: stream, cancel, approval, reconnect, incompatible protocol;
- client contract tests: одинаковые Block IDs/status после reconnect;
- stale approval/revision rejection;
- accessibility/keyboard/screenshot tests только для изменяемого GUI client;
- terminal renderer tests не входят в harness suite.
