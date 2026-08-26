# Архитектура Agentic Harness

## Суть

Harness — долговременный local-first Rust process, независимый от terminal emulator и GUI. Zap, headless CLI, будущий IDE adapter и существующий GPUI preview подключаются через versioned client protocol. Harness владеет session state, policy, approvals, capability scopes и audit; клиент только передаёт user intent/решение и отображает durable events.

Канонический effect path: `Typed request → explicit origin → Policy → Deny | Allow | NeedsApproval → human/Safe Auto gate → Sandbox → Capability → Execution → durable event`. Модель не может выдать себе `origin=user`, approval или прямой доступ к host effects.

Открой [architecture.html](architecture.html): редактируемая SVG-схема показывает сменные клиенты, versioned IPC, harness trust boundary и единственный paved road к эффекту. [safe-auto-architecture.html](safe-auto-architecture.html) отдельно раскрывает hard-deny, human-only, reviewer и output-probe ветки.

## Стратегическое разделение

| Понятие | Принадлежит | Входит в критический путь |
| --- | --- | --- |
| agent loop, sessions, tools, providers, policy, audit | harness | да, v0.2 |
| create/attach/prompt/stream/status/cancel protocol | client IPC | да, v0.2 |
| typed approvals/diffs/attachments/backend states | client IPC extension | да, v0.3 |
| terminal emulator, ANSI/VT, tabs, selection, scrollback | Zap или другой client | нет |
| controlled process/PTY для tool execution | capability host | только когда нужен конкретному tool/remote workflow |
| Blocks/diff/approval presentation | Zap adapter/fork, CLI или GPUI client | v0.3, реализация сменная |

Собственный terminal emulator не следует из необходимости запускать shell tool. Harness может исполнять bounded process и возвращать typed output, не владея ANSI renderer или пользовательской terminal tab.

## Текущий и целевой контур

| Контур | Уже есть | Следующий результат |
| --- | --- | --- |
| `agentic-terminal-core` | events, origin, file-scope policy, approvals, SQLite, manifests, CI projection | typed payloads/projections и deterministic replay v0.2 |
| headless harness | supervisor, daemon/IPC, CLI и deterministic mock stream | read-only tools и direct provider v0.2 |
| client protocol | Unix socket base, negotiation и session lifecycle v0.2 | structured extension v0.3 |
| Zap | установленный внешний terminal/agent client | CLI baseline v0.2, затем adapter или личный fork v0.3 |
| GPUI app | native preview, темы, CI pane | optional reference client; не terminal gate |
| ACP/auth | спроектированы | external-agent gateway и profiles v0.3 |
| local/remote effects | planned manifests | sandbox/capabilities v0.5–v0.6 |

Наличие type, event kind или manifest не означает готовую capability. До implementation runtime безопасно отвечает `Unavailable`.

## Клиенты

### Zap — основной путь

Базовая интеграция не требует форка: пользователь запускает headless CLI в обычной Zap tab. Zap владеет terminal rendering, shell lifecycle и своим scrollback; harness владеет только собственной task/session history.

Structured integration добавляется после стабилизации IPC. Adapter или личный fork Zap может отображать Intent/Plan/Tool/Approval/Summary Blocks, но не получает SQLite connection, raw credential или право принимать policy decision. OSC/desktop notifications допустимы как совместимость, но не как источник typed approval или достоверного lifecycle.

### CLI reference client

CLI нужен для contract tests и recovery без GUI: start, attach, stream, status, cancel, inspect approval, approve/reject. Он не реализует terminal emulator.

### GPUI reference client

Существующий `agentic-terminal-app` сохраняется как диагностический клиент и площадка CI preview/themes. Полноценный PTY/ANSI renderer возвращается только после Zap go/no-go и зафиксированного requirement.

## Два независимых пути shell

### Прямая команда пользователя в Zap

Пользовательская команда, введённая в обычный shell Zap вне harness task, исполняется терминалом/OS и не считается audit-событием harness. Harness не перехватывает и не переименовывает её в typed action.

### Tool/effect внутри harness task

1. Клиент отправляет user intent либо явно выбранный user action.
2. Provider/ACP backend предлагает только `origin=agent` actions.
3. Policy возвращает `Deny`, `Allow` или `NeedsApproval`.
4. Клиент показывает exact diff/command/target из typed approval request; решение подписано текущей session/revision.
5. Harness создаёт узкий `SandboxScope` и выбирает capability, permissions которой укладываются в scope.
6. Start/output/finish/failure записываются как durable events; клиент строит projection.

Клиент не может отправить произвольные terminal bytes и объявить их `origin=user`. Прямой shell и controlled harness execution остаются разными каналами.

## Versioned client protocol

Local IPC переносит только typed messages:

- hello/version/capability negotiation;
- session create/attach/list/status;
- prompt и attachment refs;
- streaming Intent/Plan/Tool/Agent/Notice/Summary events;
- cancellation и terminal outcome;
- approval request/inspect/approve/reject с revision binding;
- explicit `Unavailable`, `Incompatible`, `Interrupted — outcome unknown`.

Protocol не переносит SQLite connection, Keychain bytes или raw unrestricted host handle. Disconnect клиента не останавливает run автоматически; reconnect восстанавливает projection из durable source.

## ACP и providers

ACP Gateway — adapter к внешнему coding-agent, а не client protocol harness-а и не универсальный provider API. ACP update/tool/permission/auth события нормализуются во внутренние typed events/actions. Direct provider adapter владеет только streaming/cancellation и opaque credential reference; доступа к fs/process/network у него нет.

## Безопасный model context

Tool output, web/file data, ACP/MCP results и attachments являются недоверенным вводом. До model context они проходят provenance/probe/redaction. Safety reviewer получает user intent, typed action и redacted snapshot, но не raw tool output или credential.

Attachments хранятся как immutable local blobs; events содержат references/hash/metadata. Отправка требует exact backend, preview и negotiated modality.

## Persistence и bounded memory

SQLite WAL — source of truth для append-only events; projections пересчитываются.

| Данные | Где | Правило |
| --- | --- | --- |
| sessions, typed Blocks, tools, approvals | SQLite | durable, replayable, export/delete по session |
| compaction summary | SQLite event | source range + prompt/version |
| bounded tool/process output | chunk/artifact store | byte/age limits; в RAM только hot window |
| API/SSH key | macOS Keychain | в БД только opaque reference |
| attachment bytes | bounded blob store | immutable; events содержат reference/hash |
| terminal scrollback обычной Zap tab | Zap | не копируется в harness автоматически |

Правила back-pressure:

- bounded channels с документированной overflow policy;
- progress coalescing, но terminal result/error не теряется молча;
- один управляемый async runtime;
- model context compaction не удаляет исходные события;
- diagnostics: RSS, queued events, hot output bytes, Blocks, compaction ratio.

## GitLab CI experimental slice

Существующий CI pane — независимый client experiment, не harness stage. `LocalGitlabBackend` использует `gitlab-ci-local`, `RemoteGitlabBackend` — structured JSON `glab`; оба маппятся в `Pipeline → Stage → Job`. Local output не попадает в model context и ограничен 600 строками. После появления общего artifact store временный buffer можно заменить durable bounded output.

Remote mutations `run/retry/cancel` остаются unavailable до exact action/target и approval contract.

## Перевод референсов в контракт

| Референс | Контракт проекта |
| --- | --- |
| Zap roadmap | harness — standalone service; terminal является одним клиентом |
| Zap/Warp Blocks | typed plan/tool/approval/notice — client projection, не UI-owned state machine |
| DeepSeek Harness | capability — заменяемый seam с manifest, availability, permission и lifecycle |
| ACP | adapter к external coding-agent с negotiation, не universal login/provider API |
| Codex-style safety | deterministic policy до approval; execution только в scope |
| Claude Code compaction/fork | summary имеет source range; child хранит immutable parent prefix |

## Remote safety

SSH/SFTP/tmux — capabilities harness-а, а не строковые команды модели. Человек выбирает profile и проверяет first-connect fingerprint; approval содержит profile/host/file target. Представление может жить в Zap fork, CLI или другом client, но transport identity и policy state принадлежат harness.

Loopback provider (`127.0.0.1`/Unix socket) получает отдельный typed endpoint scope и не расширяет общий network access.
