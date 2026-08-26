# ACP и авторизация моделей/агентов

## Что добавлено в архитектуру

В проект добавлен обязательный слой **ACP Gateway + Auth Center**. Его задача — подключать внешние coding-agents как Blocks в общей сессии и давать человеку один экран подключения. Он не подменяет провайдера и не копирует чужие логины.

```text
Auth Center
 ├── Direct provider: Keychain API key / system-browser OAuth / local model
 └── ACP external agent: установленный CLI владеет своим login
                              ↓
                    ACP Gateway (stdio JSON-RPC)
                              ↓
              Codex / Claude / Cursor / Gemini / Qwen / …
```

ACP стандартизирует диалог клиента и coding-agent: сессии, prompts, tool updates, permission requests, terminal и auth interaction. Он **не** является единым API к любой модели и не позволяет безопасно извлечь или «перенести» токен из Codex/Cursor/Claude.

## Три честных способа подключения

| Режим | Для чего | Где секрет | Примеры |
| --- | --- | --- | --- |
| ACP agent-owned | полноценный внешний coding-agent внутри Blocks | его собственный CLI/OS keychain | Codex CLI, Claude Agent, Cursor, Gemini CLI, Qwen Code, Copilot — если установленная версия есть в ACP registry |
| Direct provider | собственный model adapter runtime | reference на запись в macOS Keychain | OpenAI-compatible: DeepSeek, OpenRouter, локальный gateway; отдельные native adapters Anthropic/Gemini |
| Local model | офлайн/local inference | отсутствует | Ollama, LM Studio или корпоративный endpoint через явный typed profile |

«Любая нейронка» означает: любой агент, реализующий ACP, либо любой провайдер с проверенным adapter-ом. Нельзя обещать login к закрытому продукту, который не даёт ACP, API или разрешённый OAuth flow.

## UX Auth Center

1. Пользователь открывает `Settings → Agents & Models → Add`.
2. Выбирает **External ACP Agent**, **Provider API**, **Browser OAuth** или **Local model**.
3. Для ACP приложение скачивает лишь metadata из registry/выбирает локальный executable, показывает название, publisher, version, источник/checksum, requested capabilities и полный command с аргументами. Запуск — отдельное подтверждение; registry metadata не является разрешением на install/launch.
4. Для API-key пользователь вставляет ключ в нативный защищённый control; сразу после сохранения он уходит в Keychain. В SQLite остаётся только opaque reference.
5. Для OAuth открывается системный браузер по явно показанному URL. WebView запрещён. Callback привязывается к создаваемому profile и не попадает в prompt/event body.
6. После проверки UI показывает `Connected`, `Needs login`, `Unavailable` или `Version incompatible`; никакого ложного «подключено».

ACP поддерживает structured elicitation, включая URL mode для OAuth. Это идеально подходит к нашему Auth Center: credential не проходит ни через LLM, ни через ACP transcript.

## Реализация ACP Gateway

### v0.3a: внешний ACP backend

- Использовать официальный Rust SDK crate `agent-client-protocol` major `2`. Это версия SDK crate, а не обещание draft wire protocol v2: по умолчанию используется стабильная согласованная protocol surface; feature `unstable_protocol_v2` не входит в MVP без отдельного RFC и compatibility tests.
- Один дочерний процесс на выбранный external-agent profile; stdout зарезервирован для ACP JSON-RPC, логи — stderr.
- Runtime маппит ACP session на внутреннюю Session и превращает update/tool/permission/auth события в наши durable Blocks.
- Process lifecycle, cancel, reconnect и exit code записываются отдельными событиями.
- Permission, file read/write и terminal requests превращаются в `origin=agent` actions и возвращаются в наш `Policy → Allow | NeedsApproval | Deny → Sandbox`; ACP backend не обходит этот путь и не может послать bytes в пользовательский PTY как `origin=user`.
- В `Safe Auto` ACP permission request не auto-accept-ится на уровне протокольного client callback: он превращается в typed action и только затем проходит hard deny, human-only gate и отдельный reviewer. Пример ACP client, который выбирает первый permission option, является демонстрацией API, но запрещённой архитектурой для продукта.

Локальный референс `/Users/antony/Documents/Codex/2026-08-13/referenced-chatgpt-conversation-this-is-an-2/outputs/jcode-acp` полезен именно для Rust stdio/session/cancel mapping. Его не копируем как универсальный backend: он корректно оставляет модели и credentials у jcode, поэтому не умеет быть логином для Cursor/Claude/Codex.

### v0.3b: registry и discovery

- Registry — источник обновляемых metadata, а не доверенная программа: перед install/launch показать publisher, version, checksum/источник и command.
- Сначала поддержать ручной local executable profile; auto-install добавить только после signature/update/rollback design.
- Проверять capability negotiation на старте сессии; не включать file/terminal/auth возможности, которых не заявили обе стороны.
- Для image prompt требуется ACP `image` capability, для embedded file context — `embeddedContext`; unsupported content не кодируется молча в text/base64. Resource link допустим лишь при уже одобренном доступе agent к URI. См. [ATTACHMENTS.md](ATTACHMENTS.md).

## Модель данных без секретов

```text
BackendProfile
  id, display_name, kind, endpoint_or_command, capability_snapshot
  credential_strategy: agent_owned | keychain_ref | browser_oauth | none
  credential_ref: optional opaque Keychain label
  status, verified_at, version, last_error_redacted
```

`credential_ref` — не token. При export сессии выносятся только provider/agent name, версия и redacted status.

Loopback HTTP и Unix socket — отдельные endpoint scopes, а не исключение из policy. Профиль хранит нормализованный transport/host/socket; `127.0.0.1` не включает общий network access, redirect на другой host повторно проходит policy.

## Критерии готовности

- ACP smoke с mock agent: initialize, new session, streaming text, cancel, structured approval, malformed stdout, stderr flood и process exit.
- Auth Center успешно различает `agent-owned`, `Keychain`, OAuth и local; token отсутствует в SQLite/event/export/log.
- Один ACP agent с уже выполненным native login появляется как Block и способен создать сессию.
- OAuth URL не открывается автоматически и показывается пользователю перед переходом.
- Переключение/падение external agent не портит общую session history.
- Typed event payload и redaction/export tests не допускают credential, OAuth callback data и raw secret-bearing terminal chunks.

## Источники

- ACP registry и список поддерживаемых agents: <https://agentclientprotocol.com/get-started/registry>
- ACP agents: <https://agentclientprotocol.com/get-started/agents>
- Официальный Rust SDK и сведения о stable/draft surfaces: <https://github.com/agentclientprotocol/rust-sdk>
- Migration SDK crate 2.x: <https://github.com/agentclientprotocol/rust-sdk/blob/main/md/migration_v2.0.md>
- ACP URL-mode elicitation/auth: <https://agentclientprotocol.com/rfds/elicitation>
- ACP v1 content blocks: <https://agentclientprotocol.com/protocol/v1/content>
