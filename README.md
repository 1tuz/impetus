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

## Why it exists

Engineering agents need long-lived state and controlled tools without making a
terminal UI, provider, or client application the source of truth. The harness
is the sole authoritative owner of durable runtime/state; clients never own
SQLite, policy, model/tool runtime, credentials, or session authority.

## Current and target

**Product model.**

```text
impetus   → user-facing CLI (future TUI)
impetusd  → local-first harness daemon (authoritative runtime)
```

`impetusd` owns durable sessions, Event Log, SQLite, policy, execution, and
credential references. Clients send typed requests and render events; they never
own authoritative state.

**Current.** The workspace ships `impetusd` and an `impetus` CLI client over
versioned Unix-socket IPC and `HarnessClient`, plus provider registry foundations
and an experimental Zap adapter. Command/JSON oriented; no TUI, `doctor`, or
Module Runtime yet.

**Target.** Modular, extensible harness: `impetus` becomes first-class CLI/TUI;
Zap keeps its own UI as another `HarnessClient` consumer. See
[Architecture](ARCHITECTURE.md) for kernel invariants vs replaceable modules.

## What works now

- Durable sessions and ordered audit events in SQLite WAL.
- Versioned local Unix-socket negotiation before a client can act.
- Typed actions through policy, approval, sandbox, capability, and execution
  checks.
- Keychain references or a local no-secret provider endpoint; profiles never
  store raw tokens.
- Typed Rust client transport, a reference CLI, ACP gateway library, and an
  experimental Zap integration baseline.
- Agent-loop vertical for filesystem reads plus approval-gated writes and shell
  commands; each result is persisted before it is returned to the model.
- Large read results use content-addressed durable artifacts; events retain a
  bounded, redacted preview and an artifact reference.

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

For provider configuration, see [configuration docs](docs/configuration.md).

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

For detailed cleanup steps, see [getting started](docs/getting-started.md#uninstall).

## Design lineage

Impetus is not a port or fork of one coding agent. It combines proven ideas
from Codex, Claude Code, OpenClaude, jcode, DeepSeek Harness, Qwen Code, Pi,
OpenCode, Aider, Kimi Code, and RTK in its own local-first Rust architecture.
See [Design references](docs/REFERENCES.md).

## Project layout

| Path | Role |
| --- | --- |
| `crates/impetus-core` | Durable events, runtime, policy, effects, providers, tools, and IPC types. |
| `crates/impetusd` | Headless Unix-socket daemon and macOS Keychain resolver. |
| `crates/impetus` | User-facing command-line client. |
| `crates/impetus-cli` | Legacy reference client (deprecated, use `impetus`). |
| `crates/impetus-client` | `HarnessClient` contract and local transports. |
| `crates/impetus-zap-adapter` | Historical/experimental Zap integration baseline. |
| `crates/impetus-acp-gateway` | ACP profile and gateway library. |

## Documentation

- [Architecture](ARCHITECTURE.md) — kernel invariants, module model, client/daemon split.
- [Roadmap](docs/ROADMAP.md) — phases and gates.
- [TODO](TODO.md) — executable task list.
- [TUI reference audit](docs/TUI_REFERENCE.md) — JCode/Codex UX decisions (planned).
- [References](docs/REFERENCES.md) — design lineage, protocols, and libraries.
- [Getting started](docs/getting-started.md) — source-checkout setup.
- [Development](docs/development.md) — workspace checks and CI.

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
