# Development

## Workspace

Impetus is a Rust 2024 workspace. `Cargo.toml` pins Rust `1.98` and the
workspace contains the core, daemon, CLI, client, Zap adapter, and ACP gateway
crates.

Run the standard local gate from the repository root:

```zsh
task verify
```

It runs, in order:

```zsh
cargo fmt --all -- --check
cargo test --workspace
cargo check --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

For dependency changes, add:

```zsh
task security
```

This invokes `cargo audit` and `cargo deny check advisories bans sources
licenses`.

## Useful commands

| Command | Purpose |
| --- | --- |
| `task harness` | Run `impetusd` (daemon). Taskfile still uses legacy `-p impetus` — see [TODO.md](../TODO.md). |
| `task cli -- <args>` | Legacy `impetus-cli`; prefer `cargo run -p impetus -- …`. |
| `task ci:list` | List jobs declared in `.gitlab-ci.yml`. |
| `task ci:local` | Run GitLab CI locally with the trusted shell executor. |

`task ci:local` requires `gitlab-ci-local`. The GitLab pipeline has `verify`
and `security` stages. Its test job intentionally uses library and binary tests
instead of the full macOS integration-test set; use `task verify` locally for
the full workspace suite.

## GitHub automation

`.github/workflows/check.yml` runs the macOS Rust checks for pull requests and
pushes to `main`. `.github/workflows/star-chart.yml` runs weekly or on manual
dispatch. It uses the pinned ShieldCN action with `contents: write` so it can
commit the generated star-chart SVG. The chart is not embedded in the README
until the repository has meaningful star history.

## Change boundaries

- Keep `impetus-core` independent of native GUI, a terminal renderer, and a
  particular client.
- Do not store raw secrets in SQLite, JSONL, logs, test fixtures, or config.
- Treat process, PTY, network, and filesystem effects as harness capabilities;
  clients do not own policy or the SQLite connection.
- Update `.gitlab-ci.yml` with any Rust, toolchain, dependency-policy, or
  verification-contract change.

See [CONTRIBUTING.md](../CONTRIBUTING.md) for the contribution workflow.
