# Architecture reader guide

[ARCHITECTURE.md](../ARCHITECTURE.md) — canonical architecture. This page —
compact guide to CURRENT/TARGET.

## Binary topology (target)

```text
impetus       → user-facing CLI / TUI (`impetus ui`)
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
| CLI client | `crates/impetus` | User-facing commands via `HarnessClient`, including `doctor` and `ui`. |
| TUI | `crates/impetus-tui` | Ratatui client library used by `impetus ui`. |
| Client contract | `crates/impetus-client` | `HarnessClient`, in-memory and Unix transports. |
| Second CLI | `crates/impetus-cli` | Legacy/secondary; migrate toward `impetus` (do not delete). |
| Zap adapter | `crates/impetus-zap-adapter` | Historical/experimental baseline. |
| ACP gateway | `crates/impetus-acp-gateway` | Library for external ACP agents. Honesty checklist: [ACP production hardening (#66)](../ARCHITECTURE.md#acp-production-hardening-checklist-66). |

Harness (`impetusd`) owns SQLite, policy, Keychain lookup, execution authority,
authoritative session state. Client disconnect preserves durable history; unknown
work is not reported as completed.

`ModelProvider` / `ProviderRegistry` — implemented foundations. Shared-prefix
session fork + named checkpoints form the Session DAG product surface (IPC fork /
restore). Module Runtime foundations, TUI (`impetus ui`), and `impetus doctor`
are present; extension adapters and a live components/registry browser remain thin
or stubbed in places.

**Migration note:** some older docs and `task harness` still reflect the era when
daemon was named `impetus`. Target and crates — see [TODO.md](../TODO.md) Phase 1.

## TARGET clients

Standalone first-class client: `impetus` CLI/TUI via `HarnessClient` → `impetusd`.
TUI reference audit: [TUI_REFERENCE.md](TUI_REFERENCE.md).

Zap: own UI, Impetus as agent backend after Connect/Authorize. No duplicated
sessions, approvals, or renderer in adapter target. Honest Implemented vs
Planned checklist: **Zap path vs standalone CLI/TUI (#5)** in
[ARCHITECTURE.md](../ARCHITECTURE.md).

All clients (including future remote): `HarnessClient` only — no core bypass.

## Trust boundary

```text
origin=user|agent → Policy → Sandbox → Capability → Execution → Durable Event
```

Credentials transient; profiles hold opaque platform-store references only (Keychain on macOS).

PolicyConfig load/reload + Approval UI payloads: see **Policy customization
and approval UI contracts (#9)** in [ARCHITECTURE.md](../ARCHITECTURE.md)
(`PolicyConfig` JSON + `impetus.approval_detail.v1`).

## Related docs

| Topic | Document |
| --- | --- |
| Module Runtime, invariants | [ARCHITECTURE.md](../ARCHITECTURE.md) |
| ACP Implemented / Partial / Planned | [ARCHITECTURE.md § ACP checklist (#66)](../ARCHITECTURE.md#acp-production-hardening-checklist-66) |
| Zap path vs CLI/TUI | [ARCHITECTURE.md § Zap path (#5)](../ARCHITECTURE.md#zap-path-vs-standalone-clitui-5) |
| PolicyConfig + ApprovalDetail | [ARCHITECTURE.md § Policy customization (#9)](../ARCHITECTURE.md#policy-customization-and-approval-ui-contracts-9) |
| Phases and gates | [ROADMAP.md](ROADMAP.md) |
| Executable tasks | [TODO.md](../TODO.md) |
| Ubuntu 24.04 smoke vs PR CI | [ubuntu-smoke-checklist.md](ubuntu-smoke-checklist.md) |
