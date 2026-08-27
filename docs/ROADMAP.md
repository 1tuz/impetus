# Roadmap — один активный путь

Это единственный исполнимый roadmap. `TODO.md` — короткая очередь из него.
`iteration-2-roadmap.md` и `iteration-3-roadmap.md` сохраняют аудит/идеи, но
не могут объявлять второй текущий этап.

## Статус версий

| Версия | Состояние | Смысл |
| --- | --- | --- |
| v0.1 | Фундамент реализован | core, durable events/SQLite, policy/approvals, mock runtime и GPUI reference preview |
| v0.2 | В работе | standalone headless harness с real provider и безопасным execution path |
| v0.3–v0.7 | Запланировано | следующие продуктовые версии; к ним не переходить раньше v0.2 gates |

Native-window smoke для optional GPUI client остаётся незакрытым хвостом v0.1.
Это не причина приостанавливать v0.2.

## Текущий релиз: v0.2 — standalone harness

**Цель:** headless local-first harness с durable sessions, read-only repo
inspection и одним explicit provider profile. Zap/Terminal.app запускают CLI в
обычной tab; GPUI остаётся optional reference client.

**Уже подтверждено:** typed event log/replay, SQLite WAL, policy/approval,
supervisor с mock restart/cancel, Unix socket, CLI, read-only tools/artifacts,
`HarnessClient` с in-memory/Unix transports и sequence-based event subscription.

### Оставшиеся шаги — строго по порядку

1. **Provider boundary.** Один OpenAI-compatible streaming adapter;
   explicit local/no-secret и Keychain-reference profiles; cancellation, retry
   budget, health state и redaction tests. Поддерживаемые endpoints выбираются
   profile, а не догадкой.
2. **Execution seam.** `NormalizedEffect → Policy → Allow | NeedsApproval |
   Deny → Sandbox → Capability → Execution`; никаких unrestricted effects без
   macOS sandbox proof. [Seatbelt spike](MACOS_SANDBOX_SPIKE.md) подтверждает
   механизм, но не включает mutating capabilities.
3. **Measured limits.** Baselines для RSS, queue, artifact/output bytes,
   restart/cancel latency и context/token accounting. Отдельно доказать, что
   headless graph не содержит GPUI, Metal, PTY и ANSI renderer.

**v0.2 готово, когда:** session переживает client/provider restart без
дубликатов; repo question имеет evidence-backed read-only answer; cancel
ограничен по времени; secret отсутствует в SQLite/export/log; headless runtime
не зависит от GPUI/terminal renderer.

## Потом — v0.3: structured clients и external agents

1. IPC extension: typed approvals, diffs, attachment refs, backend/auth states
   и negotiated `Incompatible`.
2. Zap baseline smoke в обычной tab, затем decision: adapter или private fork
   только если нужен structured Blocks/diff/approval UX.
3. ACP gateway: manual executable profile, mock agent
   initialize/session/stream/cancel/permission/exit, agent-owned login.
4. Auth Center contract: Keychain reference, system-browser OAuth and local
   no-secret profiles. OAuth URL открывается только действием пользователя.

**Не строить TUI или terminal emulator в этом этапе.** Они возможны лишь после
Zap spike и зафиксированного неудовлетворённого requirement.

## Далее — продуктовые возможности

| Версия | Результат | Главный gate |
| --- | --- | --- |
| v0.4 | long-session context, compaction, immutable fork/checkpoint | restart/fork даёт deterministic projection и bounded memory |
| v0.5 | local effects и capability SDK | exact approval, sandbox/reviewer fail closed, policy replay |
| v0.6 | remote profiles: SSH, controlled process/PTY, tmux, SFTP | host-key/target/file approval переживают restart |
| v0.7 | MVP: sessions, search, notifications, export/delete, chosen client path | task проходит intent → evidence → approval → effect → resume/fork |

## Не является roadmap stage

- **GPUI CI pane** — изолированный client experiment, не gate harness.
- **Native-window smoke старого v0.1** — технический хвост reference client,
  не блокирует v0.2.
- **Custom terminal/TUI** — optional research after Zap decision, не следующий
  «этап 0.2».
- Cloud sync, marketplace, multi-user auth, Windows/Linux parity — вне MVP.

## Правило статуса

Нельзя называть planned interface или empty DTO готовой feature. Каждый gate
закрывается только tests + applicable runtime smoke + `task verify`; для
Rust/CI/dependency changes также local GitLab job и `task security`.
