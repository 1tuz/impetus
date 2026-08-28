# Client IPC, ACP и авторизация

## Три разные границы

Не смешивать протокол клиента, ACP и provider API:

```text
Zap / CLI / IDE / GPUI
          │ versioned local client IPC
          ▼
       HARNESS
       ├── Direct provider adapter ── Keychain ref / browser OAuth / local
       └── ACP Gateway ─────────────── external coding-agent CLI owns login
```

- **Client IPC** связывает наши клиенты с нашим harness. В v0.2 это sessions, prompt, stream, status и cancel; approvals, diffs и attachment refs добавляются в v0.3.
- **ACP** связывает harness с внешним coding-agent: initialize, session, updates, permission/auth interaction.
- **Provider API** даёт model inference, но не tools/policy/session permissions.

Ни один из этих слоёв не является универсальным способом извлечь или перенести token из Codex/Cursor/Claude.

## Client IPC v0.2 и расширение v0.3

Base protocol v0.2 локальный, client-independent; текущая wire schema —
`IPC_VERSION=2`:

- обязательный hello/version handshake и пересечение requested/supported capabilities;
- session create/attach/list/status;
- prompt и streaming typed events;
- cancel и terminal outcome;

В v0.3 тот же protocol расширяется без смены владельца state:

- diffs и attachment refs;
- approval inspect/approve/reject с `request_id` и target revision;
- backend/auth states;
- `Unavailable`, `Incompatible`, `Interrupted — outcome unknown`.

Harness остаётся source of truth. Клиент не получает SQLite connection, Keychain bytes, provider secret или capability implementation. Disconnect клиента не равен cancel и не уничтожает session.

## Zap integration

### Baseline

Headless CLI запускается в обычной Zap tab. Это даёт полноценный terminal UX без собственного PTY/ANSI renderer и не требует патча Zap.

### Structured path

Adapter или личный fork Zap подключается к client IPC и отображает typed Blocks/diff/approval. OSC/notification events можно использовать для совместимости и уведомлений, но не для достоверного permission/lifecycle contract.

Если Zap позднее поддержит подходящий стандартный client protocol, отдельный adapter можно заменить. До подтверждённой совместимости не обещать ACP support со стороны Zap.

## Способы подключения backend

| Режим | Для чего | Где секрет |
| --- | --- | --- |
| ACP agent-owned | внешний полноценный coding-agent | в его CLI/OS keychain |
| Direct provider | собственный agent loop harness-а | opaque reference на macOS Keychain entry |
| Browser OAuth | разрешённый provider flow | browser/provider store; callback не в events |
| Local model | offline/local inference | секрет отсутствует |

«Любая нейронка» означает: ACP-compatible agent либо provider с проверенным adapter/API/OAuth. Закрытый продукт без такого interface не становится поддержанным от одной записи в config.

## ACP Gateway v0.3

- Использовать официальный Rust SDK crate `agent-client-protocol` major `2`; `unstable_protocol_v2` не включать без RFC и compatibility tests.
- Один дочерний process на выбранный external-agent profile; stdout зарезервирован под ACP JSON-RPC, logs — stderr.
- ACP session маппится на внутреннюю durable Session.
- Update/tool/permission/auth нормализуются в typed events/actions.
- Permission request никогда не auto-accept-ится в protocol callback: он проходит `Policy → Allow | NeedsApproval | Deny` внутри harness.
- ACP backend не может отправить bytes в пользовательский Zap PTY или присвоить `origin=user`.
- Capability negotiation проверяется при initialize; unsupported modality получает явный отказ.

Локальный `jcode-acp` остаётся полезным reference для Rust stdio/session/cancel mapping, но не универсальным backend/login.

## Auth profiles

```text
BackendProfile
  id, display_name, kind, endpoint_or_command
  protocol/capability snapshot
  credential_strategy: agent_owned | keychain_ref | browser_oauth | none
  credential_ref: optional opaque label
  status, verified_at, version, last_error_redacted
```

Правила:

- raw API key не проходит через Zap/CLI IPC после сохранения;
- в SQLite/export/log только reference/status/version;
- OAuth URL показывается полностью и открывается только user action в system browser;
- callback привязан к создаваемому profile и не попадает в prompt/event body;
- manual executable profile идёт раньше auto-install;
- registry metadata не является разрешением на install/launch;
- loopback/Unix socket — отдельный endpoint scope, не общий `allow_network`.

## Auth Center как projection

Auth Center не обязан быть отдельным GPUI screen. Его states доступны через CLI и любой structured client:

- `Connected`;
- `Needs login`;
- `Unavailable`;
- `Incompatible`;
- `Crashed`;
- `Credential reference missing`.

Zap fork может дать native settings UI, но validation, storage и status принадлежат harness.

## Критерии готовности

- Два клиента attach-ятся к одной session и видят одинаковые Block IDs/status.
- Disconnect/reconnect не дублирует history и не меняет outcome.
- Protocol version mismatch даёт явный `Incompatible`.
- Mock ACP проходит initialize/session/stream/cancel/permission/malformed stdout/stderr flood/exit.
- Один external agent с agent-owned login создаёт session без копирования credential.
- Approval из client/ACP связан с exact revision и проходит policy.
- OAuth не открывается автоматически.
- Secret, callback data и raw attachment bytes отсутствуют в SQLite/export/log/tracing.

## Источники

- [ACP registry](https://agentclientprotocol.com/get-started/registry)
- [ACP agents](https://agentclientprotocol.com/get-started/agents)
- [Rust SDK](https://github.com/agentclientprotocol/rust-sdk)
- [SDK 2.x migration](https://github.com/agentclientprotocol/rust-sdk/blob/main/md/migration_v2.0.md)
- [ACP URL-mode elicitation](https://agentclientprotocol.com/rfds/elicitation)
- [ACP v1 content blocks](https://agentclientprotocol.com/protocol/v1/content)
- [Zap](https://github.com/zerx-lab/zap)
- [Zap harness-first roadmap](https://github.com/zerx-lab/zap/blob/main/docs/roadmap.md)
