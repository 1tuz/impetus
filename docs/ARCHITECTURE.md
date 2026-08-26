# Архитектура Agentic Terminal

## Суть

Нативный UI на macOS передаёт долговременному локальному runtime типизированное действие с обязательным `origin`: `user` или `agent`. Runtime проверяет policy и получает ровно один исход: `Allow`, `NeedsApproval` или `Deny`. Только `Allow` либо принятое человеком approval продолжаются через sandbox и capability. В будущем `Safe Auto` сможет разрешать узкий класс `NeedsApproval` через отдельный fail-closed reviewer, но не сможет переопределить hard deny или human-only gate. Обратный путь — append-only события, из которых UI строит Blocks.

Открой [architecture.html](architecture.html): это редактируемая SVG-схема, не Mermaid и не скриншот. Схема показывает два разных входа — прямой пользовательский PTY и агентский/ACP intent — и точку, где их policy-пути расходятся. Отдельная схема [safe-auto-architecture.html](safe-auto-architecture.html) раскрывает hard-deny, human-only, reviewer и output-probe ветки.

## Текущий и целевой контур

| Контур | Уже в v0.1 | Появится позже |
| --- | --- | --- |
| GPUI app | native window, темы preview, durable SQLite store | terminal tabs, Blocks, Auth Center |
| core | events, action origin, file-scope policy, approvals, manifest validation | projections, Safe Auto reviewer contract, attachment refs, sandbox enforcement, execution lifecycle, resume/fork |
| capabilities | только валидируемые `planned` manifests | PTY v0.2, ACP v0.3, local effects v0.5, SSH/tmux/SFTP v0.6 |

Наличие типа, event kind или manifest не означает готовую capability. До появления implementation runtime обязан безопасно отказать, а UI — показывать `Unavailable`, не «Loaded».

## Слои и владельцы

| Слой | Отвечает за | Не должен делать |
| --- | --- | --- |
| `agentic-terminal-app` | GPUI entities, фокус/ввод, panes, виртуализацию Blocks, view models, создание узкого event-store adapter | принимать policy-решения, передавать SQLite connection во view, запускать shell/SSH |
| `agentic-terminal-core` | события, сессии, планы, approvals, policy, sandbox-contract, SQLite repositories | зависеть от GPUI или Metal |
| `terminal.pty` | PTY, процесс, ANSI-байты, scrollback chunks | общаться с моделью напрямую |
| `ssh.manager` | выбранный профиль, host-key, один transport | выполнять строку `ssh` из текста модели |
| `tmux` | список/attach/create в выбранном transport | автоматически подключаться к неизвестному host |
| `sftp` | list/upload/download с file-level approval | выполнять широкую синхронизацию по умолчанию |
| model adapter | streaming, cancellation, negotiated input modalities, provider-specific auth reference | доступ к fs/process/network, принятие policy-решений |
| safety reviewer | проверить только `AutoReviewable` action по typed/redacted snapshot | видеть secret bytes/raw tool output, переопределять hard deny/human-only |
| attachment pipeline | native selection, immutable blob, scan/redaction, capability negotiation | скрытый screen capture, implicit upload, хранение bytes в events |

## Два входа и один контролируемый эффект

### Команда человека в локальном terminal tab

1. Человек явно создаёт local tab; действие получает `origin=user`.
2. Policy разрешает создание локального PTY только внутри выбранного workspace/session scope. Отдельная approval-card не нужна: создание tab уже является прямым действием человека.
3. Клавиши и shell-команды идут в этот PTY, не в модель. Агент не может незаметно инжектировать туда bytes.
4. Lifecycle процесса и ограниченный scrollback записываются событиями; секретный terminal output не попадает в model context без явного preview.

### Фраза человека агенту

1. Пользователь: «найди, почему nginx не стартует, и предложи безопасную правку».
2. UI записывает `IntentCreated`; runtime создаёт видимый Plan Block и помечает предложенные действия `origin=agent`.
3. Policy детерминированно возвращает:
   - `Deny` — поток останавливается;
   - `Allow` — только для безопасного действия с проверенным target внутри scope;
   - `NeedsApproval` — для write/process/network/SSH/SFTP/tmux.
4. При `NeedsApproval` человек видит точный command/diff/host/file и подтверждает либо отклоняет его.
5. `Allow` или принятое approval создаёт узкий `SandboxScope`; затем выбирается только capability, чьи manifest permissions укладываются в этот scope.
6. Capability добавляет durable start/output/finish/failure events. UI строит из них Blocks.

Канонический поток: `Origin → Policy → Deny | Allow | NeedsApproval → Manual approval или Safe Auto review → Sandbox → Capability → Execution`. Ветка `Allow` пропускает human gate, ветка `Deny` не достигает sandbox. В `Safe Auto` hard-deny останавливается до reviewer, human-only остаётся у человека, а timeout/invalid verdict означает block. Постоянное разрешение — пользовательское ограниченное правило с истечением срока, не предпочтение модели. Полный контракт: [SAFE_AUTO_MODE.md](SAFE_AUTO_MODE.md).

## Безопасный model context

Результаты tools, web, terminal, ACP/MCP и files являются недоверенным вводом. Перед попаданием в agent context они проходят input-provenance/probe layer; safety reviewer получает только user intent, typed action и redacted snapshot, но не raw tool output. Это разделяет проверку того, **что агент прочитал**, и того, **что он собирается сделать**.

Файлы и скриншоты появляются как immutable local blobs с typed metadata и SHA-256. Events содержат только references. Отправка требует preview, exact backend и negotiated ACP/provider capability; `Safe Auto` не может сам выбрать, снять или отправить новый attachment. Контракт и lifecycle: [ATTACHMENTS.md](ATTACHMENTS.md).

## Local persistence

SQLite в WAL-режиме — источник истины для append-only событий. Состояние UI — пересчитываемая projection.

| Данные | Где хранятся | Правило |
| --- | --- | --- |
| сессии, Blocks, tool events, approvals | SQLite | локально; экспорт/удаление по сессии |
| compaction summary | SQLite event | сохраняет диапазон исходных событий и версию prompt |
| terminal scrollback | chunk store на диске | ограничен по байтам/возрасту; не растёт в RAM бесконечно |
| API key / SSH-key | macOS Keychain | в БД лишь стабильная reference-метка |
| SSH profile и fingerprint | SQLite | профиль, label и audit trail видимы человеку |
| attachment bytes | локальный bounded blob store | immutable original/transform; в events только reference/hash/metadata |

В v0.1 приложение открывает store в `~/Library/Application Support/Agentic Terminal/events.sqlite3`; `AGENTIC_TERMINAL_DATA_DIR` существует только для изолированного smoke/test запуска. UI получает runtime, но не `rusqlite::Connection`. Уникальность `(session_id, sequence)` защищает порядок новых баз; reopen проверяется отдельным тестом.

`Event.body` пока является v0.1 JSON-envelope, а не разрешением писать произвольные данные. До подключения ACP/provider streaming в v0.3 должны появиться typed payloads и единая redaction/export-проверка; raw credential и неотфильтрованный terminal transcript запрещены уже сейчас.

## Память и back-pressure

- Виртуализируются Blocks и terminal rows; не хранить GPUI Element на каждую строку.
- PTY пишет fixed-size чанки на диск, горячее окно на tab не больше 8 MiB.
- Tokio channels ограничены; при отставании UI второстепенный progress coalescing, а не бесконечная очередь.
- Один общий Tokio runtime; blocking workers — лишь для PTY/crypto, если это измерено.
- Compaction ограничивает model context по бюджету, но не удаляет исходные события.
- В diagnostics должны быть RSS, hot-terminal bytes, queued events, Blocks и compaction ratio.

Измеримые gates заданы в roadmap: для одного terminal tab RSS после прогрева не должен превышать idle baseline более чем на 128 MiB, а рост между 5-й и 30-й минутами soak — 32 MiB. Это первоначальные инженерные границы, которые меняются только вместе со сценарием и новым baseline.

## Перевод идей референсов в контракт

| Референс | Контракт проекта |
| --- | --- |
| persistent runtime / jcode-подход | Tokio session supervisor и durable event log, а не одноразовый prompt executor |
| DeepSeek Harness | capability — явный заменяемый seam с manifest, availability, permission и lifecycle; конфигурация не превращает `planned` capability в доступную |
| Claude Code compaction | summary — версия события с source range, никогда не скрытое удаление истории |
| Qwen Code fork/cache | child session хранит immutable parent prefix + fork point; cache key — только hint провайдеру |
| Codex-style safety | deterministic policy до approval, execution только в scope |
| Claude Code Auto mode | отдельные input probe и action reviewer; raw tool outputs исключены; no verdict fail-closed |
| ACP content blocks | image/resource передаются только после capability negotiation и outbound policy |
| Zap/Warp Blocks | plan, terminal, tool, approval, notice и ответ — самостоятельные объекты общей ленты |

## Remote safety

SSH/SFTP не являются «инструментом shell для модели». Человек выбирает profile, на первом соединении сверяет fingerprint, затем одобряет конкретное действие. tmux доступен только внутри уже одобренного transport. В audit log записываются profile ID, host label и решение — не пароль, private key и не секретный terminal output.

Loopback provider (`127.0.0.1`/Unix socket) в v0.3 получает отдельный typed endpoint scope. Он не должен неявно включать общий `allow_network` и не считается «локальным» только из-за строки URL.
