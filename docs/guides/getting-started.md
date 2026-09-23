# Getting started

Developer checkout of Impetus. For the product one-liner and ordinary Quick Start,
see [README.md](../../README.md).

## Prerequisites

- macOS or Linux x86_64
- Rust from `rust-toolchain.toml`
- [Task](https://taskfile.dev/) for repository shortcuts

```zsh
task setup
task verify
cargo build -p impetus -p impetusd
```

Ensure `impetusd` is on `PATH` next to `impetus` (release/debug `target/…` or
`~/.local/bin`).

## Ordinary client UX (lazy daemon)

You do **not** need a second terminal for normal use. `impetus` starts
`impetusd` when the socket is down (stale socket removed; matching
`IMPETUS_SOCKET` / `IMPETUS_DATA_DIR`).

```zsh
cargo run -p impetus -- create
cargo run -p impetus -- prompt <session-id> "Summarize this repository"
cargo run -p impetus -- stream <session-id>
cargo run -p impetus -- ui
```

Default data dir:

- macOS: `~/Library/Application Support/Impetus`
- Linux: `$XDG_DATA_HOME/impetus` or `~/.local/share/impetus`

Override with `IMPETUS_DATA_DIR` / `IMPETUS_SOCKET`.

## Manual daemon (dev / debug / admin)

Use an explicit `impetusd` when you need custom profiles or logs:

```zsh
task daemon
# or:
cargo run -p impetusd -- --provider-profile /tmp/my-provider.json
```

## Desktop client (macOS)

Sibling app **Impetus Desktop** (Tauri) — same Unix socket as CLI/TUI. Repo:
`../impetus-desktop`. Adapter crate: `impetus-desktop-adapter` (#310).

```zsh
open -a "Impetus Desktop"
```

Desktop expects a reachable harness socket (CLI lazy-start or a running
`impetusd`). No Accessibility / Full Disk Access for normal IPC.

## Provider profile

```zsh
cp config/provider-profile.example.json /tmp/my-provider.json
cargo run -p impetusd -- --provider-profile /tmp/my-provider.json
```

See [configuration](configuration.md).

## Roles

| Binary | Role |
| --- | --- |
| `impetus` | User CLI / TUI — lazy-starts daemon |
| `impetusd` | Authoritative process (socket, SQLite, policy, execution) |

Diagnostics: `impetus doctor`.

## Uninstall

### Installed binaries

```zsh
rm -f ~/.local/bin/impetus ~/.local/bin/impetusd
```

### Data

```zsh
rm -rf ~/Library/Application\ Support/Impetus   # macOS
# Linux: rm -rf ~/.local/share/impetus
```

Keychain: Keychain Access or `security delete-generic-password`.

## Stop a stuck daemon

```zsh
pkill impetusd
# then remove only a confirmed-stale socket (see troubleshooting)
```

Architecture: [ARCHITECTURE.md](../../ARCHITECTURE.md).
