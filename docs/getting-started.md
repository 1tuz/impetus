# Getting started

Developer checkout: daemon `impetusd` + CLI client `impetus`. Нет packaged
installer в этом guide — см. [README](../README.md) для curl install.

## Prerequisites

- macOS
- Rust `1.98.0`, selected by `rust-toolchain.toml`
- [Task](https://taskfile.dev/) for repository shortcuts

```zsh
task setup
task verify
```

## Start daemon and client

Terminal 1 — daemon:

```zsh
cargo run -p impetusd
```

Создаёт data dir `~/Library/Application Support/Impetus` (или `IMPETUS_DATA_DIR`).
Без аргументов — mock streaming provider.

Terminal 2 — CLI client:

```zsh
cargo run -p impetus -- create
```

UUID из вывода — для последующих команд:

```zsh
cargo run -p impetus -- prompt <session-id> "Summarize this repository"
cargo run -p impetus -- stream <session-id>
cargo run -p impetus -- --help
```

`stream` печатает stored events; это не interactive TUI.

Legacy: `cargo run -p impetus-cli` — deprecated, используй `impetus`.

## Provider profile

Пример — loopback OpenAI-compatible endpoint без credential:

```zsh
cp config/provider-profile.example.json /tmp/my-provider.json
cargo run -p impetusd -- /tmp/my-provider.json
```

См. [configuration](configuration.md).

## Roles

| Binary | Role |
| --- | --- |
| `impetusd` | Authoritative daemon (socket, SQLite, policy, execution) |
| `impetus` | User CLI client (`HarnessClient` → socket) |

Target diagnostics: `impetus doctor` — [TODO](../TODO.md) Phase 1.
