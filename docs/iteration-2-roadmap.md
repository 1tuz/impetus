# Roadmap второй итерации

Этот документ детализирует [основной roadmap](ROADMAP.md). Активен один этап: `v0.2 — standalone harness core`. Работы идут вертикальными gates; следующий gate не начинается, пока предыдущий не имеет deterministic tests и явного failure state.

## Приоритеты

- **P0:** архитектурные границы, durable state, IPC, policy и безопасный execution seam.
- **P1:** bounded artifacts, context/token efficiency и измеримость.
- **P2:** recovery/fork/checkpoints и repo intelligence.
- **P3:** advanced agents после стабильного single-agent lifecycle.
- **P4:** SSH/tmux/SFTP capabilities после доказанного local effect path.
- **P5:** optional terminal rewrite только после benchmark и зафиксированного gap.

## P0 — текущий критический путь

### Gate 0.1 — typed events и replay

Результат:

- `EventEnvelope { schema_version, event_id, session_id, sequence, timestamp, payload }`;
- exhaustive typed payloads для session/run/intent/plan/tool/approval/agent/notice;
- terminal outcomes: `completed`, `failed`, `cancelled`, `interrupted_unknown`;
- pure `reduce(events) -> SessionProjection` без GPUI типов;
- migration старого `kind_json + body_json` без потери исходных rows.

Готово, когда одинаковый event stream всегда даёт одинаковую projection; неизвестная версия не интерпретируется как success; malformed payload возвращает typed error; SQLite reopen/replay tests проходят.

### Gate 0.2 — durable sessions и supervisor

Результат:

- durable session/run metadata и create/attach/list/status;
- sequence allocation после restart без дубликатов;
- один managed async runtime;
- mock streaming provider;
- start, stream, soft interrupt, hard cancel, failure и bounded restart;
- pending approval восстанавливается из events, а не из in-memory map.

Готово, когда kill/restart восстанавливает session и projection; provider restart не дублирует chunks; cancel имеет измеримый deadline; disconnect клиента не меняет outcome.

### Gate 0.3 — local daemon, Unix socket и CLI

Результат:

- отдельный headless Harness process без GPUI/Metal dependencies;
- versioned handshake и capability negotiation по Unix domain socket;
- typed prompt/stream/status/cancel/attach messages;
- CLI reference client для Zap и Terminal.app;
- client не получает SQLite connection, credential bytes или execution handle.

Готово, когда CLI можно закрыть и повторно attach-нуть к продолжающейся task; mismatch даёт `Incompatible`; socket permissions проверены; два clients видят одинаковые Block IDs/status.

### Gate 0.4 — read-only tools и bounded artifacts

Результат:

- `list/read/search` tools с canonical workspace scope и provenance;
- effect normalization до policy;
- bounded channel и chunk sizes;
- disk-backed artifact primitive с hash, metadata, range-read и search;
- deterministic reducer отправляет модели compact result, оригинал остаётся доступен;
- `DENIED` возвращается как typed tool result, supervisor продолжает безопасный plan.

Готово, когда «объясни репозиторий» работает без mutation; symlink/path escape получает deny; большой output не растит RAM и model context линейно; truncation всегда содержит artifact reference.

### Gate 0.5 — direct provider и credential boundary

Результат:

- один OpenAI-compatible streaming adapter;
- local/no-secret и Keychain-reference profiles;
- timeout, cancellation, retry budget и provider health state;
- stable serialized prompt prefix;
- redaction/export tests без token, callback и raw credential.

Готово, когда DeepSeek/OpenRouter/custom/local endpoint выбирается явным profile; secret отсутствует в SQLite/log/export; adapter не имеет filesystem/process permissions.

### Gate 0.6 — единый execution seam

Этот gate фиксирует границу, но не открывает unrestricted effects.

Результат:

- `NormalizedEffect -> Policy -> Allow | NeedsApproval | Deny`;
- approval связан с exact action fingerprint и revision;
- `Sandbox -> Capability -> Execution` — единственный путь process/file/network effect;
- direct CI experiment либо использует этот путь, либо остаётся явно изолированным и недоступным из agent loop;
- OS sandbox spike доказывает доступный macOS mechanism до включения agent-initiated writes/processes.

Готово, когда ни client, ни provider не могут подделать `origin=user`; stale approval отклоняется; unavailable sandbox закрывает effect; start/output/finish/failure записываются durable events.

## P1 — context, token и cost efficiency

Порядок после Gate 0.4:

1. Budgeted context builder: HOT recent events, WARM summaries, COLD event/artifact refs.
2. Tool-output reducers для test/build/git/search/logs с fail-open-to-original через artifact ref.
3. Stable prompt prefix и метрики cache read/write; порядок rules/tools детерминирован.
4. Delta context только при доказанной base revision; иначе полный bounded source.
5. Lazy tool definitions и lifecycle manager для MCP processes.
6. Task telemetry: logical/sent/tool/reasoning tokens, model calls, latency, artifact bytes, queue depth и RSS delta.

Gate: long output не увеличивает sent context линейно; исходник доступен; claims об экономии публикуются только относительно сохранённого baseline.

## P2 — recovery, checkpoints и repo intelligence

1. Versioned compaction с source event range и prompt/version.
2. Session DAG с immutable parent prefix и явным fork point.
3. Turn checkpoint: before revision, changed-files manifest, after revision.
4. Undo/fork только для локальных файлов; внешние side effects не обещаются как reversible.
5. Budgeted Repo Map: git state, tree, symbols, imports, dependencies и already-read files.
6. LSP запускается лениво только для точного definition/reference/type запроса.

Gate: restart и fork дают deterministic projections; child не меняет parent; Repo Map соблюдает token budget; checkpoint не маскирует внешние effects.

## P3 — advanced agents

Начинается только после single-agent cancel/restart/checkpoint gates.

- scoped subagent session как отдельная ветка DAG;
- shared immutable context prefix и общий repo index;
- отдельный checkpoint и capability scope;
- conflict notification по changed-files manifests;
- single agent остаётся default; fan-out требует независимых subtasks и budget.

Gate: параллельные agents не разделяют mutable approval state, не дублируют MCP/runtime и не перезаписывают конфликтующие файлы молча.

## P4 — SSH/tmux/SFTP capabilities

- именованный profile и Keychain reference;
- host-key verification и explicit first-connect;
- read-only remote diagnostics как первый vertical slice;
- controlled process/PTY, tmux и SFTP после общего policy/sandbox gate;
- production profile имеет более строгую policy, но не отдельный execution bypass.

Gate: модель не выбирает произвольный host; host-key change блокирует connection; transfer требует file-level target и approval; credentials не покидают Mac.

## P5 — optional terminal investigation

Не писать terminal emulator до Zap integration/benchmark gate.

Сначала измерить Zap: cold start, idle, 1/10/20 tabs, panes, large scrollback/Blocks и 1/5/20 harness sessions. Метрики: RAM, idle/active CPU, GPU memory, startup и frame latency.

Собственный PTY/ANSI client допускается только при записанном неудовлетворённом requirement и сравнении стоимости adapter/fork. GPUI preview остаётся reference client.

## 10 изменений с максимальным эффектом

1. Типизировать event payloads и добавить schema version.
2. Ввести pure projection и replay tests.
3. Сделать session identity, sequence и approvals восстанавливаемыми.
4. Выделить supervisor в headless crate/process.
5. Добавить mock provider с stream/cancel/restart.
6. Зафиксировать Unix socket protocol и CLI attach/reconnect.
7. Реализовать read-only tools через normalized policy path.
8. Добавить bounded artifact primitive до новых output-producing tools.
9. Перенести все process effects за единый capability execution seam.
10. Зафиксировать RAM/queue/output/token baselines до оптимизаций.

## Ближайший исполнимый slice

Только Gate 0.1:

1. RFC уровня code comments/tests для `EventEnvelope` и payload enum.
2. Pure projection с `Intent`, `Plan`, `Tool`, `Agent`, `Approval`, `Notice`.
3. SQLite migration/read compatibility.
4. Tests: round-trip, deterministic replay, malformed/unknown version, reopen.
5. Обязательный `task verify`.

Не входит: daemon, provider API, IPC, sandbox implementation, checkpoints, index, swarm или новый UI.
