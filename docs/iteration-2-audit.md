# Аудит второй итерации

Дата среза: 2026-08-26. Аудит описывает текущее рабочее дерево, включая незакоммиченные изменения. Это не отчёт о готовности будущих компонентов: наличие типа, документа или manifest означает только зафиксированный seam, пока нет исполнимого теста.

## Краткая карта текущей архитектуры

```text
agentic-terminal-app (GPUI process)
  ├── создаёт AgentRuntime
  ├── открывает SQLite event store
  ├── вызывает PolicyEngine
  └── напрямую запускает GitLab CI backends

orbit-core
  ├── Event { kind, свободный JSON body }
  ├── AgentRuntime с in-memory approvals и sequence
  ├── PolicyEngine с file/network checks
  ├── SQLite append/list
  ├── capability manifest validation
  └── GitLab CI model/backends
```

Целевой критический путь:

```text
Zap / CLI / optional GPUI client
  └── versioned typed IPC по Unix domain socket
        └── один долгоживущий Harness process
              ├── session supervisor + durable projections
              ├── provider loop + context builder
              ├── Policy + approval + sandboxed capability execution
              └── SQLite events + bounded artifacts
```

## Состояние по функциям

| Feature | Current implementation | Target architecture | Gap | Priority | Risk | Complexity |
| --- | --- | --- | --- | --- | --- | --- |
| Разделение `core` / client | `orbit-core` не зависит от GPUI; GPUI app зависит от core | Harness и clients — отдельные процессы/crates | GPUI app всё ещё владеет runtime и SQLite | P0 | высокий | M |
| Append-only event log | SQLite WAL, уникальный `(session_id, sequence)`, reopen test | Versioned typed events, replay и schema migration | `body` — свободный JSON; нет event schema version | P0 | высокий | M |
| Typed lifecycle events | Есть `EventKind` для части lifecycle | Exhaustive payloads для intent/plan/run/tool/approval/agent/notice | `kind` и `body` могут расходиться; нет terminal outcomes | P0 | высокий | M |
| Projection | Отсутствует | Pure reducer `events -> SessionProjection` | Client читает runtime state напрямую | P0 | высокий | M |
| Durable session identity | `AgentRuntime::new` всегда создаёт новый UUID | Create/attach/list/recover session | Нельзя открыть прежнюю session | P0 | высокий | M |
| Sequence recovery | Счётчик начинается с `1` в памяти | Следующий sequence восстанавливается транзакционно | Restart существующей session приведёт к конфликту | P0 | высокий | M |
| Approvals | Policy decision и request/resolution events есть; pending map живёт в RAM | Durable approval state с revision/fingerprint | Restart теряет pending state; нет invalidated/expired | P0 | высокий | M |
| Persistent Harness | Отсутствует | Один долгоживущий process, несколько дешёвых sessions | Сейчас lifecycle равен lifecycle GPUI window | P0 | критический | L |
| Client IPC | Контракт описан только в документах | Versioned Unix socket protocol и capability negotiation | Нет transport, handshake, attach, reconnect | P0 | критический | L |
| Session supervisor | `AgentRuntime` умеет записать intent и policy result | start/stream/soft-interrupt/cancel/failure/restart | Нет run state machine и provider ownership | P0 | критический | L |
| Policy origin boundary | `ActionOrigin` и `Allow/NeedsApproval/Deny` реализованы | Origin определяется доверенной IPC/capability boundary | Caller может сконструировать `origin=user`; нет revision binding | P0 | критический | M |
| System sandbox | `SandboxScope` проверяет пути и network flag | OS-enforced process/file/network isolation | Текущее имя не означает реальную системную изоляцию | P0 | критический | L |
| Capability manifests | Статический JSON валидируется; все реализации `planned` | Узкий typed capability host | Нет lifecycle, availability probe и execution gate | P0 | высокий | L |
| Read-only workspace tools | `crate::tools::ReadOnlyTools`: list/read/search с provenance, bounded output и disk-backed artifact store; deny при escape | `list/read/search` с provenance и limits | Нет первого безопасного evidence vertical slice | P0 | средний | M |
| Provider loop | Есть только example profiles | Mock provider, затем один streaming adapter | Нет stream/cancel/retry/profile validation | P0 | высокий | L |
| Credential boundary | В документах и example config только opaque refs | macOS Keychain adapter и local/no-secret profile | Нет runtime adapter и redaction test | P0 | критический | M |
| Bounded output / artifacts | GPUI ограничивает видимый CI log 600 строками | Disk-backed artifact store + bounded hot window | CI channel unbounded; local run копит полный `String`; нет source ref | P0/P1 | высокий | M |
| Context optimizer | Только архитектурные требования | Stable prefix, budgeted context, reducers, HOT/WARM/COLD | Нет context builder и telemetry | P1 | средний | L |
| Session DAG / compaction | Есть только будущие `EventKind` | Immutable parent prefix, versioned summary/source range | Нет session metadata, fork model и compactor | P2 | высокий | L |
| Workspace checkpoints | Отсутствуют | Turn-level changed-files manifest и reversible local checkpoint | Нет executor и transaction boundary | P2 | высокий | L |
| Repo intelligence | Отсутствует | Дешёвый tree/symbol/git map; LSP lazy | Нет budgeted retrieval layer | P2 | средний | L |
| Multi-agent work | Отсутствует | Scoped branches/checkpoints поверх общей infrastructure | Нет single-agent supervisor; начинать рано | P3 | высокий | L |
| Resource budgets | Есть v0.1 RSS baseline и 600-line UI limit | Harness RSS, queue/artifact/cache ceilings и eviction | Нет daemon benchmark и owner/TTL metrics | P0/P1 | высокий | M |
| Zap integration | Запланирован CLI baseline и structured path | Zap остаётся terminal frontend; Harness — source of truth | Нет headless client и IPC smoke | P0 | высокий | L |
| Optional GPUI client | Native preview, темы и CI pane работают как эксперимент | Тонкий diagnostic client поверх IPC | Сейчас владеет runtime/DB и direct effects | P0 boundary | высокий | M |

## Уже реализовано и сохраняется

- независимый от GPUI crate `orbit-core`;
- append-only SQLite WAL store с уникальным sequence и reopen test;
- явные `origin`, policy decision и базовый approval event;
- file-scope deny и local-only network deny;
- статическая валидация capability manifests;
- native GPUI reference client и набор тем;
- общий `Pipeline -> Stage -> Job` для экспериментального CI slice;
- Rust toolchain и обязательные repository checks.

## Реализовано частично

- `AgentRuntime` — синхронный in-process фасад, не supervisor и не daemon;
- события — durable envelope, но payload не типизирован;
- approvals — событие сохраняется, актуальное pending-state нет;
- sandbox — логическая scope-проверка, не системная изоляция;
- output bounding — ограничена только видимая клиентская очередь;
- backend profiles — безопасные примеры config, не исполнимая интеграция;
- Zap/ACP/auth — документационные контракты без runtime.

## Конфликты с целевой архитектурой

1. GPUI client создаёт `AgentRuntime` и открывает SQLite. Закрытие окна завершает владельца session state.
2. CI client помечает process action как `origin=user`, затем backend запускает process напрямую. Нет единого `Policy -> Sandbox -> Capability -> Execution -> durable event` path.
3. `std::sync::mpsc::channel()` в local CI не ограничен. Видимый лимит 600 строк не ограничивает producer queue или полный накопленный log.
4. `SandboxScope` может создать ложное ощущение OS sandbox. До реальной изоляции capability должна явно сообщать `Unavailable`.
5. `terminal.pty` всё ещё помечен в capability catalog как `v0.2`, хотя текущий roadmap вынес собственный terminal emulator из критического пути.
6. `EventKind` содержит будущие compaction/fork markers, но без typed payload и replay invariant они не являются готовой функцией.

## Вывод

Первый исполнимый delta — не новый UI и не provider API. Сначала нужны typed event contract, pure projection и безопасная storage migration. На них затем строятся recoverable supervisor, daemon и Unix socket client. До этого любое расширение GPUI, CI effects, swarm, index или model routing увеличит будущий перенос состояния.
