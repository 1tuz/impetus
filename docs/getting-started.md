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
task daemon
# или напрямую:
cargo run -p impetusd
```

Создаёт data dir `~/Library/Application Support/Impetus` (или `IMPETUS_DATA_DIR`).
Без аргументов — mock streaming provider.

Terminal 2 — CLI client:

```zsh
task client -- create
# или напрямую:
cargo run -p impetus -- create
```

UUID из вывода — для последующих команд:

```zsh
task client -- prompt <session-id> "Summarize this repository"
task client -- stream <session-id>
task client -- --help
```

`stream` печатает stored events; это не interactive TUI.

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

Target diagnostics: `impetus doctor`.

## Uninstall

### Installed binaries

If installed via `scripts/install.sh` (default location `~/.local/bin`):

```zsh
rm -f ~/.local/bin/impetus ~/.local/bin/impetusd
```

If installed to a custom `$IMPETUS_INSTALL_DIR`, remove binaries from that location.

### Data and state

Impetus stores sessions, events, and configuration in:

```zsh
~/Library/Application Support/Impetus  # macOS
~/.local/share/impetus                 # Linux (future)
```

To remove all data:

```zsh
rm -rf ~/Library/Application\ Support/Impetus
```

### Credentials

On macOS, provider credentials are stored in Keychain. To remove:

1. Open **Keychain Access.app**
2. Search for `impetus` or your provider service names
3. Delete matching keychain items

Or via command line:

```zsh
# List Impetus keychain entries
security find-generic-password -s "impetus" 2>&1 | grep "svce"

# Delete specific entry (example)
security delete-generic-password -s "<service-name>" -a "<account-name>"
```

### Cleanup summary

```zsh
# Stop daemon
pkill impetusd

# Remove binaries
rm -f ~/.local/bin/impetus ~/.local/bin/impetusd

# Remove data
rm -rf ~/Library/Application\ Support/Impetus

# Remove credentials (Keychain Access or security command)
```
