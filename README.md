# Impetus

> **An ultra-lightweight, Rust-built, terminal-first, local-first, all-in-one agent harness for engineering.**

[![License](https://img.shields.io/badge/license-Apache--2.0-4B8BBE.svg)](LICENSE)
[![Architecture](https://img.shields.io/badge/architecture-local--first-000000.svg)](#why-it-exists)

<p align="center">
  <img src="./assets/readme/hero.svg" width="100%"
       alt="Impetus: a local runtime for durable engineering-agent sessions and explicit control">
</p>

Impetus is an ultra-lightweight, Rust-built all-in-one local agent harness: durable sessions, model/tool
orchestration, safety decisions, credentials, and execution authority stay
together behind replaceable terminal and remote clients. A client restart
cannot silently discard an in-flight session or expand its access.

**No root / sudo / password in normal mode.** `impetus` and `impetusd` run
entirely in userspace under `$HOME` (or `IMPETUS_DATA_DIR`). Seatbelt is
userspace `sandbox-exec`. Keychain reads are silent
(`kSecUseAuthenticationUISkip`) — missing credentials fail closed; the daemon
never shows an unlock / password dialog. Privilege escalation (`sudo` / `su` /
login-shell `-l`) is refused by policy. Optional admin-only features stay
opt-in and never block core flows.

## Why it exists

Engineering agents need long-lived state and controlled tools without making a
terminal UI, provider, or client application the source of truth. The harness
is the sole authoritative owner of durable runtime/state; clients never own
SQLite, policy, model/tool runtime, credentials, or session authority.

## Current and target

**Product model.**

```text
impetus   → user-facing CLI / TUI (`impetus ui`)
impetusd  → local-first harness daemon (authoritative runtime)
```

`impetusd` owns durable sessions, Event Log, SQLite, policy, execution, and
credential references. Clients send typed requests and render events; they never
own authoritative state.

**Current.** The workspace ships `impetusd` and an `impetus` CLI client over
versioned Unix-socket IPC and `HarnessClient`, plus provider registry foundations
and an experimental Zap adapter. Also available: `impetus doctor` (diagnostics),
`impetus ui` (Ratatui TUI), and Module Runtime foundations. Primary CLI is
`impetus`; `impetus-cli` is the legacy/secondary surface (kept for existing
workflows — migration note, not deletion).

**Target.** Modular, extensible harness: `impetus` becomes first-class CLI/TUI;
Zap keeps its own UI as another `HarnessClient` consumer. Honest adapter
checklist (today vs Planned discovery/authorize): [Architecture — Zap path
(#5)](ARCHITECTURE.md#zap-path-vs-standalone-clitui-5). See
[Architecture](ARCHITECTURE.md) for kernel invariants vs replaceable modules.

## What works now

Honest status (detail: [ARCHITECTURE.md](ARCHITECTURE.md)):

- Durable sessions and ordered audit events in SQLite WAL.
- Versioned local Unix-socket negotiation before a client can act.
- Typed actions through policy, approval, **path-scope** sandbox, capability, and
  execution (fail-closed). On macOS, process spawn also wraps with Seatbelt
  (`sandbox-exec`); non-macOS stays path-scope only.
- Keychain references or a local no-secret provider endpoint; profiles never
  store raw tokens.
- Typed Rust client transport, CLI, TUI (`impetus ui`), ACP gateway library, and
  an experimental Zap adapter.
- Agent-loop vertical for filesystem reads plus approval-gated writes and shell;
  large tool/web/paste bodies use durable content-addressed artifacts; approval
  diffs use ephemeral in-memory attachments.
- Context HOT/WARM/COLD, lazy tool/instruction descriptions, session
  shared-prefix fork and checkpoints.
- Extension **import** adapters (Skills, MCP, Claude/Codex/Cursor layouts).
  Lifecycle CLI keep (`impetus extension plan|install|…`); no marketplace.
  Production MCP SoT: `impetusd` autoloads **only** `$IMPETUS_DATA_DIR/mcp/*.json`
  + live `ReloadMcpServers`; `ListMcpServers` / `ListModels` IPC
  (`connected=false` until first tool use). Explore child + Workflow Explore
  share one AgentLoop bridge. MemoryStore control-plane IPC **Implemented**
  (session JSONL under data dir) + AgentLoop project-scope context inject
  on Prompt/FollowUp/ResolveApproval resume (approval-resume). Browser daemon
  health/negotiate **Partial** (honest Absent; CDP Parked).
- Daemon-owned PTY (`portable-pty`, IPC v12): owner-session binding, cwd
  containment; Agent origin Seatbelt on macOS; optional Sqlite metadata store;
  live PTY not restart-durable; TUI passthrough (`Ctrl+\` / `/pty`).
- Session model IPC (`ListProviders` / Get/SetSessionModel) + OpenAI Chat
  Completions SSE default (`--provider-profile`); Anthropic library not default
  daemon path. JSON Schema tool-arg validation before policy on builtins.

## Request control flow

<p align="center">
  <a href="./docs/architecture-map.html">
    <img src="./assets/readme/request-control-flow-v2.svg" width="100%"
         alt="Request control flow: a client sends a request to Impetus, which controls approval, execution, and durable local history">
  </a>
</p>

This is the request-and-safety flow, not a complete system map. The canonical
architecture explains current components, ownership, and the planned client
paths: [Architecture](ARCHITECTURE.md).

## Installation

The product landing page is published at [1tuz.github.io/impetus](https://1tuz.github.io/impetus/).

### Quick install

Supported platforms: macOS Apple Silicon, Linux x86_64

```zsh
curl -fsSL https://raw.githubusercontent.com/1tuz/impetus/main/scripts/install.sh | zsh
```

Binaries will be installed to `~/.local/bin`. Add it to your PATH:

```zsh
export PATH="$HOME/.local/bin:$PATH"
```

### From source

```zsh
git clone https://github.com/1tuz/impetus.git
cd impetus
task setup
task verify
cargo build --release -p impetus -p impetusd
```

## Usage

Start the daemon:

```zsh
impetusd
```

In another terminal, create a session and interact:

```zsh
impetus create
impetus prompt <session-id> "Summarize this repository"
impetus stream <session-id>
# When the stream shows a pending approval:
impetus approve <session-id> <approval-id>
# Or reject it and let the model continue with the denial observation:
impetus approve <session-id> <approval-id> --reject
```

For provider configuration, see [configuration docs](docs/guides/configuration.md).

## Uninstall

Remove binaries:

```zsh
rm -f ~/.local/bin/impetus ~/.local/bin/impetusd
```

Remove data and sessions:

```zsh
rm -rf ~/Library/Application\ Support/Impetus  # macOS
```

Remove credentials from macOS Keychain via **Keychain Access.app** or `security delete-generic-password`.

For detailed cleanup steps, see [getting started](docs/guides/getting-started.md#uninstall).

## Design stance

Impetus is not a port or fork of another coding agent. It keeps a small trusted
kernel (events, artifacts, policy, approval, sandbox, executor) and replaceable
layers above it. Engineering principles (durable events, fail-closed admission,
explicit approvals, opaque secret references) matter more than feature parity
lists. Optional protocol/UX notes: [Design references](docs/reference/design-references.md).

## Project layout

| Path | Role |
| --- | --- |
| `crates/impetus-core` | Durable events, runtime, policy, effects, providers, tools, and IPC types. |
| `crates/impetusd` | Headless Unix-socket daemon and macOS Keychain resolver. |
| `crates/impetus` | User-facing CLI / TUI client (`doctor`, `ui`, …). |
| `crates/impetus-cli` | Legacy/secondary CLI; migrate callers to `impetus` over time (do not delete). |
| `crates/impetus-tui` | Ratatui TUI library used by `impetus ui`. |
| `crates/impetus-client` | `HarnessClient` contract and local transports. |
| `crates/impetus-zap-adapter` | Historical/experimental Zap integration baseline. |
| `crates/impetus-acp-gateway` | ACP profile and gateway library. |

## Documentation

- [Docs map](docs/README.md) — guides, architecture, reference, archive.
- [Architecture](ARCHITECTURE.md) — kernel + capability matrix (code-backed).
- [TODO](TODO.md) — Now / Next / Later backlog.
- [Roadmap](docs/architecture/roadmap.md) — short priority narrative.
- [TUI notes](docs/reference/tui-ux-audit.md) — client UX constraints and audit.
- [References](docs/reference/design-references.md) — protocols and libraries.
- [Getting started](docs/guides/getting-started.md) — source-checkout setup.
- [Development](docs/guides/development.md) — workspace checks and CI.

## Development

```zsh
task verify
```

When `Cargo.toml` or `Cargo.lock` changes, also run `task security`.

## Contributing

Read [CONTRIBUTING.md](CONTRIBUTING.md). For vulnerabilities, follow
[SECURITY.md](SECURITY.md).

## License

Licensed under [Apache-2.0](LICENSE).
