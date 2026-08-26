# Roadmap v0.1 → рабочий harness

Каждый этап имеет собственную демонстрацию и завершается только по указанным критериям. Главная продуктовая линия — переносимый local-first harness. Zap, CLI, IDE и экспериментальный GPUI-клиент являются сменными поверхностями над ним; собственный terminal emulator не является обязательной частью MVP.

Фактический delta текущего рабочего дерева зафиксирован в [аудите второй итерации](iteration-2-audit.md). Исполнимые P0-P5 gates и ближайший slice находятся в [детальном roadmap](iteration-2-roadmap.md).

## v0.1 — безопасный фундамент

**Результат:** независимый Rust core, append-only события, SQLite WAL, policy, approval, capability manifests и диагностический GPUI preview.

**Готово, когда:** форматирование, core tests, `cargo check --workspace` и clippy проходят; agent-write внутри workspace создаёт `ApprovalRequested`; выход file target за workspace и SSH в local-only scope получают `Deny`; записанные SQLite события переживают reopen; зафиксирован baseline диагностического клиента; native Metal window открывается без WebView. Воспроизводимый smoke на чистом Mac остаётся незакрытым последним gate v0.1.

## v0.2 — standalone harness core

**Результат:** долгоживущий headless Harness без зависимости от GPUI и terminal renderer: typed event payloads/projections, recoverable session supervisor, bounded artifacts, read-only tools, один OpenAI-compatible streaming adapter, Unix socket IPC и CLI reference client. Harness запускается из любого обычного терминала, включая Zap, но продолжает session независимо от client process.

Вертикальные gates выполняются по порядку:

1. typed `Intent → Plan → Tool → Agent` events, schema migration и deterministic projection из SQLite;
2. durable session identity/sequence/approvals и mock provider → stream/cancel/error/restart;
3. headless daemon + versioned Unix socket IPC → create/attach/stream/status/cancel;
4. CLI reference client → close/reconnect без остановки run;
5. read-only `list/read/search` tools → provenance, bounded output и artifact references;
6. direct OpenAI-compatible profile → Keychain reference или local/no-secret endpoint;
7. единый normalized policy/capability seam без включения unrestricted effects.

**Готово, когда:** команда из Zap или Terminal.app запускает Harness; закрытие и повторный attach client не останавливают run и не дублируют history; «объясни репозиторий» даёт evidence-backed ответ без mutation; cancel завершается за ограниченное время; provider/Harness restart восстанавливает ту же projection; большой output не растит RAM/context линейно; secret и raw credential отсутствуют в SQLite/export/log; headless dependency graph не содержит GPUI, Metal, PTY или ANSI renderer.

Компактный GitLab CI frontend уже существует как независимый экспериментальный клиентский срез. Его native smoke полезен, но не блокирует harness v0.2.

## v0.3 — сменные клиенты, Zap и внешние agents

**Результат:** structured extension уже работающего local IPC, Zap integration spike, ACP Gateway для внешних coding-agents, manual executable profiles, Auth Center contract и клиентские Blocks. Harness остаётся источником policy/session state; клиент не принимает решения за него.

Вертикальные gates:

1. IPC extension → approval request/response, diffs, attachment refs и backend/auth states;
2. Zap baseline smoke → v0.2 CLI работает в обычной Zap tab без патча Zap;
3. Zap structured path → отдельный adapter или личный fork отображает typed Blocks, diff и approval; OSC/notification hooks не считаются полным протоколом;
4. mock ACP agent → initialize/session/stream/cancel/permission/exit;
5. manual local executable profile → capability negotiation и lifecycle;
6. Auth profiles → agent-owned CLI, system-browser OAuth и расширенные backend states поверх v0.2 Keychain/local profiles;
7. Safe Auto mock reviewer и attachment negotiation без host effects.

**Готово, когда:** CLI и Zap используют одну durable session model; disconnect клиента не убивает harness run; protocol mismatch даёт явное `Incompatible`; mock ACP проходит полный smoke; structured permission всегда превращается в typed action; OAuth URL не открывается без человека; unsupported attachment получает явный отказ; клиент и harness могут обновляться независимо в пределах negotiated version.

## v0.4 — long-session context, compaction и fork

**Результат:** token-budget context builder, versioned compaction с source range, resume с compacted context, immutable parent prefix/fork point, local file checkpoints и bounded attachment lifecycle.

**Готово, когда:** restart даёт ту же projection; summary показывает source range и версию; child не меняет parent history; long-session harness benchmark держит заданный RSS и queue ceiling независимо от клиента.

## v0.5 — локальные эффекты и capability SDK

**Результат:** typed manifest/permissions/version, exact approval diff/command/target, workspace sandbox, sample external capability, enforced Safe Auto reviewer/input probe и outbound attachment policy.

**Готово, когда:** edit не применим до approval/reviewer allow; command не выходит за scope/time/resource limit; hard-deny и human-only не auto-approve; classifier outage/invalid verdict не достигает execution; malformed или over-permissioned plugin безопасно отклонён; policy replay детерминирован.

## v0.6 — remote capabilities

**Результат:** SSH profiles, Keychain refs, known-host, controlled remote process/PTY, tmux и SFTP transfer events. Представление может жить в Zap fork, CLI или отдельном клиенте.

**Готово, когда:** host-key change блокирует connection; model не выбирает произвольный hostname; tmux требует profile + выбранную session; transfer требует file-level approval и восстанавливается после restart.

## v0.7 — рабочий MVP

**Результат:** multi-session, search, notifications, export/delete, crash recovery, migrations, performance suite и выбранный поддерживаемый клиентский путь.

**Главный gate:** пользователь запускает harness из Zap или другого клиента, формулирует задачу, видит plan/tool evidence, одобряет одну точную правку, resume/fork-ит сессию и выполняет один remote workflow. У каждого эффекта есть audit record; kill/restart не выдаёт ложное «готово»; harness держит memory ceiling без привязки к RSS конкретного терминала.

## Опционально — собственный terminal client

Собственный GPUI PTY/ANSI renderer не входит в критический путь. Существующий `agentic-terminal-app`, темы и CI pane сохраняются как reference client и экспериментальная площадка.

Go/no-go принимается после Zap integration spike. Продолжать полноценный terminal emulator стоит только если подтверждён конкретный разрыв: невозможен typed approval/Blocks, нарушается граница `origin`, нет стабильного integration seam, Zap fork непригоден по UX/производительности или нужен отдельный распространяемый продукт. До такого факта `portable-pty`, `alacritty_terminal`, tabs, selection/copy и scrollback остаются optional backlog.

## Намеренно вне MVP

Cloud sync, collaboration, marketplace с неограниченными plugins, обязательный собственный terminal emulator, Windows/Linux parity, IDE/editor и multi-user auth меняют trust/memory model и требуют отдельного решения.
