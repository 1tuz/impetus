# Roadmap — фактический активный путь

Этот документ сохраняет старые этапы как историю, но их старые статусы не
являются доказательством готовности. Актуальная карта фактического кода:
[current architecture audit](current-architecture-audit.md).

## Active sequence after audit

1. **A1: safe local execution authority.** Запретить любой process spawn без
   harness-issued admission, provisioned per-session workspace и fail-closed
   Seatbelt; origin=user не обходит OS sandbox.
2. **A2: trusted origin and approval continuation.** Origin определяет
   trusted harness boundary; accepted approval запускает только matching
   durable deferred effect. ACP не принимает raw credential strings.
3. **A3: per-session coordination.** Удалить global request serialization
   только после concurrency regressions.
4. **B1: typed client plus event-driven subscription.** Typed SDK и cursor
   backfill/push без polling в daemon, in-memory transport и Zap.
5. **B2/C: complete current DTOs, then provider registry/router/budgets.**
   Не добавлять provider/model feature в central dispatcher.

Remote execution, checkpoints/DAG, TUI, swarm, profiles and learning остаются
после этих gate. Planned DTO, simulated PTY/tmux, test-only seam и Seatbelt
spike не считаются готовой product feature.

Ниже — историческая запись предыдущих заявлений и идей. Для нового статуса
использовать audit выше, а не статусные галочки ниже.

## Статус версий

| Версия | Состояние | Смысл |
| --- | --- | --- |
| v0.1 | Фундамент реализован | core, durable events/SQLite, policy/approvals, mock runtime и GPUI reference preview |
| v0.2 | Готов | standalone headless harness с real provider и безопасным execution path |
| v0.3 | Готов | structured clients и external agents |
| v0.4 | Готов | long-session context, compaction, immutable fork/checkpoint |
| v0.5 | Готов | local effects и capability SDK |
| v0.6–v0.7 | Запланировано | remote profiles, MVP UI |

Native-window smoke для optional GPUI client остаётся незакрытым хвостом v0.1.
Это не причина приостанавливать дальнейшую работу.

## Завершённые релизы

### v0.2 — standalone harness ✓

**Цель:** headless local-first harness с durable sessions, read-only repo
inspection и одним explicit provider profile.

**Готово:**
- Real provider integration (OpenAI-compatible streaming)
- Execution seam: Policy → Sandbox → Capability → Execution
- Measured limits + resource baselines
- Session survives restart без дубликатов
- Secret redaction (не попадают в SQLite/logs)

### v0.3 — structured clients и external agents ✓

**Готово:**
- IPC extension: typed approvals, diffs, attachments, backend states
- Zap integration baseline (CLI в обычной Zap tab)
- ACP gateway: manual executable profile, mock agent
- Auth Center: Keychain reference, system-browser OAuth, local no-secret

### v0.4 — long-session context ✓

**Цель:** long-session context, compaction, immutable fork/checkpoint.

**Gate:** restart/fork даёт deterministic projection и bounded memory. ✓

**Готово:**
- CompactionPolicy + auto-compaction на token threshold
- Budget integration (SessionSupervisor + IPC events)
- Immutable fork/checkpoint механизм
- Deterministic projection после restart
- Bounded memory tests

### v0.5 — local effects и capability SDK ✓

**Цель:** безопасные local effects с exact approval, fail-closed sandbox и policy replay.

**Gate:** exact approval, sandbox/reviewer fail closed, policy replay. ✓

**Готово:**
- Capability SDK с типизированными capabilities (WorkspaceRead, WorkspaceWrite, ProcessSpawn, NetworkConnect)
- CapabilityVersion для exact approval matching
- Sandbox fail-closed enforcement (проверка до approval/execution)
- PolicySnapshot и replay для аудита
- ActionFingerprint включает capability version
- Integration tests для всех gate критериев

## Текущий релиз: v0.6 — remote profiles

**Цель:** SSH profiles, controlled process/PTY execution, tmux, SFTP.

**Gate:** host-key/target/file approval переживают restart.

### Готово

- [x] SSH profiles с host-key verification
  - SSHProfile struct с host, user, port, host_key_fingerprint
  - Host-key verification перед connection (fail если mismatch)
  - Keychain integration для SSH private keys (SSHKeyReference, не raw key)
  - PolicyCheck для SSH connection (origin, target host, user)
  - Durable SSH approval в SQLite (SqliteSSHApprovalStore)
  - Host-key approval переживает restart (gate выполнен)
  - NormalizedEffect::ssh_connect() + NetworkConnect capability расширена на SshConnect/SftpTransfer
  - async-trait, base64 dependencies добавлены
- [x] Controlled process/PTY execution
  - ProcessExecutionRequest с policy check и sandbox admission
  - ProcessOutput capture с timeout и bounded output (2MB limit)
  - PtySession lifecycle: spawn, attach, detach, terminate
  - PtySessionManager координирует policy, spawn, durable storage
  - SqlitePtySessionStore для durable session state
  - Integration tests для process execution и PTY sessions
  - Fail-closed: execution только после policy Allow или exact approval
- [x] tmux integration для persistent remote sessions
  - TmuxSession lifecycle: create, attach, detach, list, kill
  - TmuxSessionManager координирует SSH, policy, durable storage
  - SqliteTmuxSessionStore для durable tmux session state
  - Remote command execution через SSH + tmux
  - Policy check для tmux session creation (origin, target host)
  - Integration tests для tmux sessions (9 тестов)
  - Sessions survive harness restart

### Оставшиеся шаги

- [ ] SFTP для remote file access
- [ ] Durable approval для remote targets (частично: SSH approval готов)

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
