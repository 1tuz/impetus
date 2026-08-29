# Architecture reader guide

[ARCHITECTURE.md](../ARCHITECTURE.md) is the canonical architecture. This page
is a compact guide to its CURRENT/TARGET distinction.

## CURRENT

| Component | Path | Responsibility |
| --- | --- | --- |
| Core | `crates/impetus-core` | Events/projections, session runtime, policy, approvals, effects, providers, read-only tools, IPC messages. |
| Daemon | `crates/impetus` | Unix-socket server, provider profile, macOS Keychain resolver. |
| Client contract | `crates/impetus-client` | `HarnessClient`, in-memory and Unix transports. |
| Reference CLI | `crates/impetus-cli` | Command/JSON `create`, `list`, `prompt`, `stream`, `cancel`, `context`. |
| Zap adapter | `crates/impetus-zap-adapter` | Historical/experimental structured adapter baseline. |
| ACP gateway | `crates/impetus-acp-gateway` | Library types and gateway logic for external ACP agents. |

The all-in-one harness owns SQLite, policy decisions, Keychain lookup,
execution authority, and authoritative session state. Client disconnects
preserve durable history; unknown work is not reported as completed.

`ModelProvider` and `ProviderRegistry` are implemented foundations. The
current direct provider path remains profile-driven. Copied event history on a
fork is not a Session DAG.

## TARGET clients

The standalone first-class client is `impetus` CLI/TUI, connecting through
`HarnessClient` to `impetusd`. It is planned for normal terminals, SSH, and
environments without Zap; its implementation framework is not decided.

Zap retains its own UI and selects Impetus as an agent backend after explicit
Connect/Authorize. The target is discovery, connection status, backend
selection, and request forwarding—not duplicated sessions, approvals, model
state, renderer, or custom Blocks protocol.

## Trust boundary

A typed effect follows Policy (`allow`, `deny`, or `needs approval`),
sandbox availability, capability scope, and execution. Credential data is
transient; profiles contain opaque Keychain references, never raw tokens.

## Historical material

[IMPLEMENTATION_HISTORY.md](IMPLEMENTATION_HISTORY.md) and files under
`docs/archived/` are historical snapshots, not current architecture.
