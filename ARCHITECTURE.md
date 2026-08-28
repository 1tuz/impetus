# Архитектура проекта

Это единственный canonical архитектурный документ: hierarchy, зависимости,
runtime boundaries и ownership. Исполнимый порядок работ — в
[docs/ROADMAP.md](docs/ROADMAP.md).

## Иерархия

```text
impetus/
├── crates/
│   ├── impetus-core/                durable domain/runtime foundation
│   ├── impetus/                     headless daemon + Unix socket server
│   ├── impetus-client/              transport-neutral client contract
│   ├── impetus-cli/                 reference client for Zap/Terminal.app
│   └── impetus-app/                 optional GPUI reference client + CI pane
├── config/
│   └── capabilities.json            declarative capability catalog
├── docs/                            product contracts, roadmap, UX and audits
├── scripts/                         macOS bootstrap and local checks
├── .gitlab-ci.yml                   versioned verification/security contract
├── Taskfile.yml                     local task entry points
├── Cargo.toml                       workspace membership and shared pins
├── README.md                        entry point for users and contributors
└── ARCHITECTURE.md                  this project map
```

`target/`, runtime SQLite files, artifacts, socket files and local CI output
are generated state; they are not source architecture.

## Dependency direction

```text
Zap / Terminal.app
        │ ordinary shell tab
        ▼
impetus-cli ────────────────► impetus-client
                                      │
                                      │ typed local IPC
                                      ▼
                           impetus
                                      │
                                      ▼
                            impetus-core
                     events · policy · SQLite · tools · artifacts

impetus-app ────────────────────────► impetus-core
  optional GPUI reference client; outside headless runtime path
```

Confirmed by workspace manifests and imports:

| Crate | Owns | Depends on |
| --- | --- | --- |
| `impetus-core` | events, projections, policy, approvals, SQLite, tools, artifacts and IPC DTOs | Rust libraries only; no GPUI/PTY/ANSI |
| `impetus` | daemon lifecycle, socket permissions and subscription delivery | `core` |
| `impetus-client` | `HarnessClient`, in-memory and Unix transports | `core` |
| `impetus-cli` | command parsing and JSON output | `client`, `core` DTOs |
| `impetus-app` | optional GPUI diagnostics, themes and GitLab CI preview | `core` |

## Product boundary and ownership

Harness — long-lived local-first process, independent from terminal emulator
and GUI. It owns durable session state, SQLite, policy decisions, approvals,
capability scope and audit events. A client sends typed intent/decision and
renders durable events; it never owns SQLite, Keychain secret, SSH transport or
policy state machine.

```text
typed request → explicit origin → policy
→ Deny | Allow | NeedsApproval → sandbox → capability → execution → durable event
```

Model/backend can create only `origin=agent`; it cannot claim `origin=user` or
approve its own action.

| Concern | Owner | Critical path |
| --- | --- | --- |
| sessions, providers, tools, policy, audit | harness | v0.2 |
| create/attach/list/prompt/stream/cancel; status in session responses | typed local IPC | v0.2 |
| approvals, diffs, attachments, backend states | IPC extension | v0.3 |
| terminal rendering, tabs, selection, scrollback | Zap or another client | outside harness |
| controlled process/PTY | capability host | only when a capability needs it |

## Clients and recovery

**Zap baseline.** User runs headless CLI in a normal Zap or Terminal.app tab.
Zap owns terminal rendering and shell history; harness owns task/session
history. No Zap fork is required for this path.

**Reference CLI.** CLI is a recovery and contract client. It uses the same
`HarnessClient` contract as future TUI/IDE adapters. It receives no SQLite
connection or secret.

**Optional GPUI app.** `impetus-app` is a diagnostics/theme/CI-preview
client. It is not headless runtime source of truth and must not pull GPUI,
Metal, terminal renderer, PTY or ANSI parsing into headless crates.
It owns no SQLite connection or policy engine. Its existing CI buttons are
explicit direct user actions outside a harness task and therefore outside
harness audit; future task-bound CI execution must use client IPC.

On prompt, harness records durable intent and supervisor emits typed events.
Client reconnects from its last rendered sequence and receives only missing
events. Client disconnect never means completion, cancellation or approval.

## Protocol, data and safety

Local IPC wire schema is `IPC_VERSION=2`. It requires hello first, intersects requested and supported
capabilities, rejects unnegotiated requests, and transports sessions,
prompt, events, cancellation and explicit `Unavailable`, `Incompatible` and
`Interrupted — outcome unknown` states. It never transports Keychain bytes,
raw credential or unrestricted host handle.

| Data | Owner/storage | Rule |
| --- | --- | --- |
| sessions, events, tool/approval records | SQLite WAL | durable and replayable |
| large bounded tool output | artifact store | opaque hash/reference; bounded redacted window only in RAM |
| API/SSH keys | macOS Keychain | SQLite keeps opaque reference only |
| ordinary terminal scrollback | Zap/client | never copied into harness automatically |

`list`, `read` and `search` are workspace-scoped read-only tools with
provenance, pre-read/capture limits and baseline secret redaction. Client DTOs
receive an opaque artifact ID, never its filesystem path. Tool, file, web,
ACP/MCP and attachment data remain untrusted before model context.

## External agents and effects

ACP is an adapter to an external coding-agent, not harness client IPC and not a
universal provider-login mechanism. Direct provider adapters own only
streaming/cancellation plus opaque credential reference; they receive no
filesystem/process permission.

SSH/SFTP/tmux are future harness capabilities, not model-generated command
strings. User selects a profile; host/file/target approval and host-key checks
remain harness policy. Custom terminal/TUI, ACP gateway, provider login,
Keychain adapter, mutable local effects, SSH/tmux/SFTP, checkpoint/DAG are not
claimed implemented unless their canonical roadmap gate is closed.

## CI preview

GPUI CI pane is a separate client experiment. It projects
`Pipeline → Stage → Job` from `gitlab-ci-local` or `glab`; it is not a harness
execution gate and remote retry/cancel remain unavailable until exact approval.

Editable visual architecture lives in [docs/architecture.html](docs/architecture.html).
