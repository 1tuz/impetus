# Getting started

This guide runs the daemon and reference CLI from a source checkout. Impetus
does not currently publish an installer or a packaged application.

## Prerequisites

- macOS
- Rust `1.98.0`, selected by `rust-toolchain.toml`
- [Task](https://taskfile.dev/) for the repository shortcuts

From the repository root, install the pinned Rust toolchain and validate the
workspace:

```zsh
task setup
task verify
```

`task setup` checks the local macOS/Rust prerequisites, installs Rust `1.98.0`
with `clippy` and `rustfmt`, and configures the repository-owned Git hooks.

## Start a mock session

In the first terminal, run the daemon:

```zsh
cargo run -p impetus
```

The daemon creates its data directory at
`~/Library/Application Support/Impetus` unless you override it. With no
arguments, it serves the built-in mock streaming provider.

In a second terminal, create a session:

```zsh
cargo run -p impetus-cli -- create
```

The command prints a UUID. Use that UUID in later commands:

```zsh
cargo run -p impetus-cli -- prompt <session-id> "Summarize this repository"
cargo run -p impetus-cli -- stream <session-id>
```

`stream` prints the events currently stored after sequence zero; it is not an
interactive terminal. Use `list`, `cancel`, or `context` as needed:

```zsh
cargo run -p impetus-cli -- --help
```

## Use a local provider

The example profile targets a loopback OpenAI-compatible endpoint without a
credential. Copy it outside the repository if you need to edit it:

```zsh
cp config/provider-profile.example.json /tmp/impetus-provider.json
cargo run -p impetus -- --provider-profile /tmp/impetus-provider.json
```

The profile must contain only supported non-secret fields. See
[configuration](configuration.md) before configuring a Keychain-backed or OAuth
provider.
