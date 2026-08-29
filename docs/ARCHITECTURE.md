# Architecture reader guide

[ARCHITECTURE.md](../ARCHITECTURE.md) — canonical architecture. Эта страница —
компактный guide к CURRENT/TARGET.

## Binary topology (target)

```text
impetus       → user-facing CLI / future TUI
impetusd      → authoritative daemon
impetus-core  → libraries (no binary)
```

```text
impetus ──HarnessClient──► impetusd ──► impetus-core
```

## CURRENT

| Component | Path | Responsibility |
| --- | --- | --- |
| Core | `crates/impetus-core` | Events, session runtime, policy, approvals, effects, providers, tools, IPC types. |
| Daemon | `crates/impetusd` | Unix-socket server, provider profile, macOS Keychain resolver. |
| CLI client | `crates/impetus` | User-facing commands via `HarnessClient` (target: CLI/TUI). |
| Client contract | `crates/impetus-client` | `HarnessClient`, in-memory and Unix transports. |
| Legacy CLI | `crates/impetus-cli` | Deprecated reference client. |
| Zap adapter | `crates/impetus-zap-adapter` | Historical/experimental baseline. |
| ACP gateway | `crates/impetus-acp-gateway` | Library for external ACP agents. |

Harness (`impetusd`) owns SQLite, policy, Keychain lookup, execution authority,
authoritative session state. Client disconnect preserves durable history; unknown
work is not reported as completed.

`ModelProvider` / `ProviderRegistry` — implemented foundations. Copied-event fork
≠ Session DAG. Module Runtime, TUI, doctor, extension adapters — not implemented.

**Migration note:** часть older docs и `task harness` ещё отражают эпоху, когда
daemon назывался `impetus`. Target и crates — см. [TODO.md](../TODO.md) Phase 1.

## TARGET clients

Standalone first-class client: `impetus` CLI/TUI via `HarnessClient` → `impetusd`.
TUI reference audit: [TUI_REFERENCE.md](TUI_REFERENCE.md).

Zap: own UI, Impetus as agent backend after Connect/Authorize. No duplicated
sessions, approvals, or renderer in adapter target.

All clients (including future remote): `HarnessClient` only — no core bypass.

## Trust boundary

```text
origin=user|agent → Policy → Sandbox → Capability → Execution → Durable Event
```

Credentials transient; profiles hold opaque platform-store references only (Keychain on macOS).

## Related docs

| Topic | Document |
| --- | --- |
| Module Runtime, invariants | [ARCHITECTURE.md](../ARCHITECTURE.md) |
| Phases and gates | [ROADMAP.md](ROADMAP.md) |
| Executable tasks | [TODO.md](../TODO.md) |
