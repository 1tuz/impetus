# Архитектура Impetus

**Канон:** [ARCHITECTURE.md](ARCHITECTURE.md) (EN). Этот файл — краткая
RU-навигация по терминам и ссылкам. При любом расхождении побеждает EN.
Не дублируем capability matrix и PROTO здесь — они быстро устаревают.

| Документ | Роль |
| --- | --- |
| [ARCHITECTURE.md](ARCHITECTURE.md) | Инварианты, слои, matrix, IPC, honesty |
| [docs/architecture/roadmap.md](docs/architecture/roadmap.md) | Now / Next / Later |
| [docs/architecture/binary-topology.md](docs/architecture/binary-topology.md) | `impetus` / `impetusd` / lazy-start |
| [docs/architecture/kernel-invariants.md](docs/architecture/kernel-invariants.md) | Trusted Kernel rules |
| [TODO.md](TODO.md) | Открытые задачи |
| [EXTENSION_REPOSITORY_CONTRACT.md](EXTENSION_REPOSITORY_CONTRACT.md) | Extension packs |

## Product invariant

Impetus = local AI-agent runtime/harness: маленький **Trusted Kernel** +
заменяемые Runtime / Extensions / Clients.

- **`impetus-core`** — Rust crate (kernel + runtime libs), **не** синоним
  Trusted Kernel.
- **`impetusd`** — authoritative process boundary; обычный `impetus`
  **lazy-starts** его (manual `impetusd` — debug/admin).
- Клиенты шлют typed IPC и показывают durable events/approvals. Они **не**
  владеют SQLite, secrets, SSH transport или policy.

**Без root / sudo / password в нормальном режиме:** userspace data dirs,
Seatbelt через `sandbox-exec`, silent Keychain, RiskGate Deny на privilege
escalation, PTY без login-shell `-l`.

## Слои (canonical)

```text
Clients (CLI/TUI · Desktop · ACP · Zap)
        ↓  HarnessClient / versioned Unix IPC
     impetusd
        ├─ Runtime services (AgentLoop, Context, Tools, Providers, Workflows, …)
        ├─ Trusted Kernel (EventStore, Policy, Approval, Sandbox, Capability, Executor, Keychain refs)
        └─ Replaceable → Extension Host / SDK → packs (MCP, skills, LSP/browser, host_process, …)
```

Диаграммы (канон EN):

- [system-architecture.svg](assets/readme/system-architecture.svg)
- [execution-flow.svg](assets/readme/execution-flow.svg)

## Binary topology

```text
impetus       = user-facing CLI / TUI (lazy-starts daemon)
impetusd      = authoritative daemon
impetus-core  = domain / runtime libraries (no standalone binary)
impetus-cli   = legacy connect-only CLI (no lazy-start)
```

Lazy-start через shared crate `impetus-daemon-control` (CLI — тонкий адаптер):
stale socket unlink только если никто не слушает; concurrent spawn
сериализуется через `daemon.spawn.lock` (`flock` — crash holder не блокирует
autostart навсегда); live socket + IPC `Incompatible` → hard stop без
unlink/respawn. Детали: [binary-topology.md](docs/architecture/binary-topology.md).

## IPC

Версия протокола, capability list и Hello negotiation — **только** в EN
`ARCHITECTURE.md` и `crates/impetus-protocol` (`IPC_VERSION` /
`IPC_MIN_SUPPORTED`). Не копируем число сюда.

## Extensions

Package SDK + host (`instruction_pack` / `mcp_bridge` / `host_process`),
lifecycle CLI Implemented, marketplace Won't. Контракт sibling-репо:
[EXTENSION_REPOSITORY_CONTRACT.md](EXTENSION_REPOSITORY_CONTRACT.md).
Статус matrix — EN.

## Clients / TUI / Desktop

- TUI (`impetus ui`) — first-class presentation over `HarnessClient`.
- Desktop — отдельный клиент (sibling repo); SoT только daemon.
- Открытый polish — [TODO.md](TODO.md) § Clients.

Полный honesty status (Partial / Remaining) — только
[ARCHITECTURE.md](ARCHITECTURE.md).
