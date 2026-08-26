# Roadmap v0.1 → рабочий MVP

Каждый этап имеет собственную демонстрацию и завершается только по указанным критериям — не по факту появления файлов. Этапы выполняются последовательно: интерфейс будущего этапа можно обозначить заранее, но нельзя выдавать его за работающую capability.

## v0.1 — нативный безопасный фундамент

**Результат:** GPUI-CE окно, append-only события, SQLite WAL, policy, approval и manifests.

**Готово, когда:** форматирование, core tests, `cargo check --workspace` и clippy проходят; agent-write внутри workspace создаёт `ApprovalRequested`; выход file target за workspace и SSH в local-only scope получают `Deny`; пользовательское создание local PTY не требует второй approval-card; записанные SQLite события переживают reopen; зафиксированы idle RSS baseline и сценарий измерения; на чистом Mac открывается native Metal window без WebView.

## v0.2 — полезный локальный терминал без агента

**Результат:** `portable-pty`, проверенный ANSI engine (сначала исследовать `alacritty_terminal`), tab lifecycle, scrollback chunks, resize/Ctrl-C/copy.

**Готово, когда:** zsh, Unicode, 24-bit color, resize и Ctrl-C работают в PTY; закрытие tab reap-ит child process; direct command работает без model provider; hot scrollback не превышает 8 MiB на tab; в 30-минутном soak total RSS не выше idle baseline + 128 MiB, а рост между 5-й и 30-й минутами не больше 32 MiB; очередь terminal events ограничена и её capacity указана в benchmark.

## v0.3 — natural-language loop и Blocks

**Результат:** один OpenAI-compatible streaming adapter, ACP Gateway для external coding-agents, Auth Center (Keychain reference / system-browser OAuth / agent-owned login), cancellation, composer, plan/tool/agent Blocks, read-only workspace tools, typed contract `Manual | Safe Auto` и attachment refs без host effects.

Вертикальные gates внутри этапа выполняются по порядку:

1. typed event → projection → Intent/Plan/Tool Blocks и read-only workspace action;
2. mock ACP agent → initialize/session/stream/cancel/permission/exit;
3. manual local executable profile → capability negotiation и lifecycle;
4. Keychain-backed direct provider → streaming/cancel без credential в events;
5. Auth Center → agent-owned, API key reference, browser OAuth и local endpoint states.
6. Safe Auto mock reviewer → fail-closed verdict, mode invalidation и audit projection без execution.
7. attachment vertical slice → native selection, image/resource capability negotiation, preview и unsupported state.

**Готово, когда:** «объясни репозиторий» показывает evidence Blocks без mutation; cancel не зависает; provider restart не дублирует event log; ACP mock-agent проходит initialize/session/stream/cancel; loopback endpoint не расширяет общий network scope; mock reviewer не разрешает hard-deny/human-only и при timeout возвращает block; image attachment уходит только negotiated backend-у, unsupported model получает явный отказ; typed payload/redaction tests подтверждают, что secret и attachment bytes отсутствуют в SQLite/exports/tracing.

## v0.4 — resume, compaction и fork

**Результат:** token-budget context builder, versioned compaction с source range, resume after restart, immutable parent prefix и fork point, bounded attachment blob lifecycle/delete.

**Готово, когда:** сессия после restart даёт ту же projection; summary показывает source range; child не меняет parent history; long-session benchmark держит RAM ceiling.

## v0.5 — локальные эффекты и capability SDK

**Результат:** typed manifest/permissions/version, approval card с exact diff/command/target, workspace sandbox, sample external capability, enforced Safe Auto reviewer/input probe и outbound attachment policy.

**Готово, когда:** edit не применим до approval/reviewer allow; command не выходит за scope/time/resource limit; hard-deny и human-only не auto-approve; classifier outage/invalid verdict не достигает execution; 3 последовательных или 20 суммарных blocks ставят Safe Auto на паузу; attachment проходит type/size/secret/provenance checks и не отправляется неявно; malformed/over-permissioned plugin отказан безопасно; policy replay детерминирован.

## v0.6 — SSH Manager, tmux, SFTP

**Результат:** profiles, Keychain refs, known-host, remote PTY, controlled tmux, SFTP browser с transfer events.

**Готово, когда:** host-key change блокирует connection; model не подключается к произвольному hostname; tmux требует profile + выбранную session; transfer требует file-level approval и восстанавливается после restart.

## v0.7 — рабочий MVP

**Результат:** multi-session, search Blocks, notification centre, export/delete, crash recovery, migration, performance suite, packaging/notarization plan.

**Главный gate:** пользователь создаёт local tab, работает shell-командами, формулирует задачу словами, видит plan/tool Blocks, одобряет одну правку, resume/fork-ит сессию и ведёт SSH/tmux/SFTP workflow. У каждого эффекта есть audit record; в 60-минутном four-tab demo total RSS не выше v0.1 idle baseline + 384 MiB, а рост между 10-й и 60-й минутами не больше 64 MiB; kill/restart не выдаёт ложное «готово».

## Намеренно вне MVP

Cloud sync, collaboration, marketplace с неограниченными plugins, remote agent daemon, Windows/Linux parity, IDE/editor и multi-user auth меняют trust/memory model и требуют отдельного RFC.
