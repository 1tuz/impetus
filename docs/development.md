# Development

## Workspace

Impetus is a Rust 2024 workspace. `Cargo.toml` pins Rust `1.98`.

Local gate from the repository root (full workspace — use before handoff):

```zsh
task verify
```

```zsh
cargo fmt --all -- --check
cargo test --workspace
cargo check --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

For dependency changes, also:

```zsh
task security
```

## Pull request CI

Workflow: `.github/workflows/ci.yml` (single PR pipeline).

1. **Detect** — `scripts/ci-affected.sh` vs PR base (`main`).
2. **macOS** (if Rust changed) — `fmt`, Clippy + `cargo test --lib --bins` on
   affected packages; `cargo check` on dependants when a shared crate changed.
3. **Linux** (if Rust changed) — `cargo check` on affected + dependants only
   (compile guard, not a second full test suite).
4. **Security** — only when `Cargo.toml` / `Cargo.lock` / `deny.toml` change.
5. **Site** — only when `site/**` changes (`npm run check`).
6. **Gate** — always-green aggregator so docs-only PRs still pass required checks.

Docs/markdown/assets-only changes skip Rust jobs.

Preview what CI would select:

```zsh
task ci:affected
```

### Required GitHub branch-protection check

Mark **only** `Gate` (job name under workflow `CI`) as required for auto-merge.
Do not require the internal macOS/Linux/security/site jobs individually — they
may be skipped when out of scope.

## Manual / heavy tests

Keep running locally when you touch Seatbelt, full IPC integration, or want the
whole suite:

```zsh
cargo test --workspace
```

Integration tests under `crates/*/tests/` are intentionally outside the PR gate.

## Useful commands

| Command | Purpose |
| --- | --- |
| `task daemon` | Run `impetusd`. |
| `task client -- <args>` | Run `impetus` CLI. |
| `task ci:affected` | Print CI scope for current branch vs `origin/main`. |
| `task security` | `cargo audit` + `cargo deny`. |

## Change boundaries

- Keep `impetus-core` independent of native GUI, a terminal renderer, and a
  particular client.
- Do not store raw secrets in SQLite, JSONL, logs, test fixtures, or config.
- Treat process, PTY, network, and filesystem effects as harness capabilities;
  clients do not own policy or the SQLite connection.

See [CONTRIBUTING.md](../CONTRIBUTING.md).
