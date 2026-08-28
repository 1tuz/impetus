# Orbit

[Русская версия](README.ru.md)

Orbit is a local-first macOS harness for coding agents. It keeps sessions and audit events durable, routes every effect through explicit policy, and stays independent from any one terminal or GUI.

> Early-stage software. The core is usable for local development; public interfaces and integrations will evolve.

![Orbit architecture](docs/orbit-architecture.svg)

Architecture diagram created with [diagram-design](https://github.com/cathrynlavery/diagram-design).

## Why Orbit

An agent runtime should not gain access merely because a model asked for it. Orbit separates the durable runtime from its clients and applies a clear decision path to every typed action:

`Policy → Allow | Needs approval | Deny → Sandbox → Capability → Execution`

- **Durable by default.** SQLite WAL stores sessions, events, approvals, and projections so a client disconnect does not discard the session state.
- **Explicit control.** Actions carry `user` or `agent` origin. Agent-originated effects cannot grant themselves approval.
- **Secret-safe boundaries.** Credentials stay in macOS Keychain; events, SQLite, logs, and client IPC use references or redacted data only.
- **Replaceable clients.** The headless runtime, Unix-socket protocol, CLI, and optional native client have separate responsibilities.

## Current scope

- Headless daemon with versioned local Unix-socket IPC and capability negotiation.
- Reference CLI for creating, attaching to, streaming, prompting, and cancelling sessions.
- Durable event store, policy and approval flow, bounded workspace read-only tools, and controlled process/PTY capabilities.
- Explicit local or OpenAI-compatible provider profiles; Keychain-backed credentials are resolved only when a provider request is made.

## Quick start

Requirements: macOS, Rust `1.98.0`, and Xcode Command Line Tools. The optional native client also requires Metal support.

```zsh
task setup
task verify
```

Run the harness in one terminal:

```zsh
cargo run -p impetus
```

Then create a session from another terminal:

```zsh
cargo run -p agentic-terminal-cli -- create
```

Use `cargo run -p agentic-terminal-cli -- --help` for the available session commands.

## Zap integration

Zap is not required by Orbit and does not own its policy, state, or secrets. Today, Orbit can run from a normal Zap tab. We plan to improve the integration with a dedicated adapter that presents typed status, output, diffs, and approval requests while keeping the runtime boundary intact.

## Influences and acknowledgements

Orbit is built independently, drawing specific ideas from several projects and protocols:

- [Zap](https://github.com/zerx-lab/zap): local-first terminal UX and the direction for a future structured client adapter.
- [DeepSeek Harness](https://github.com/deepseek-ai/deepseek-harness): capability seams, manifests, and append-only traces.
- [Agent Client Protocol](https://agentclientprotocol.com/): a boundary for external coding-agent adapters, session updates, and negotiated capabilities.
- [Claude Code](https://code.claude.com/): explicit permission modes and fail-closed safety thinking.
- [GPUI-CE](https://github.com/gpui-ce/gpui-ce) and [Zed GPUI examples](https://github.com/zed-industries/zed/tree/main/crates/gpui/examples): an optional native macOS reference client.

The [reference notes](docs/REFERENCES.md) record what each source inspired and what deliberately remains outside Orbit.

## Project notes

- [Architecture diagram](docs/orbit-architecture.html)
- [Roadmap](docs/ROADMAP.md)
- [Reference notes](docs/REFERENCES.md)
- [Agent-facing repository rules](AGENTS.md)

## License

[Apache-2.0](LICENSE)
