# Development

## Workspace

Impetus is a Rust 2024 workspace. `Cargo.toml` pins Rust `1.98`.

### Local gate (cheap — default)

```zsh
task verify
```

```zsh
cargo fmt --all -- --check
git diff --check
```

Do **not** routinely run `cargo test/check/clippy --workspace` on a laptop.
Heavy quality is on GitHub Actions. Optional full local suite: `task verify:full`.

For dependency changes, CI Security job runs `audit`/`deny`; local
`task security` is optional.

## Pull request CI

Workflow: `.github/workflows/ci.yml` (single PR pipeline).

1. **Detect** — `scripts/ci-affected.sh` vs PR base (`main`). Emits `rust`,
   `macos`, `docs_only`, `packages`, `check_packages`, …
2. **Linux** (if Rust) — primary gate: `fmt`, Clippy on affected + dependants
   (`--lib --bins --tests -D warnings`), unit + integration/`crates/*/tests`
   + daemon E2E for affected packages (offline mocks; no provider keys).
3. **macOS** (if Rust **and** platform-sensitive paths) — Seatbelt / sandbox /
   no-sudo / PTY filters only. Skipped for ordinary crate edits (does not block
   Gate).
4. **Security** — when `Cargo.toml` / `Cargo.lock` / `deny.toml` change.
5. **Site** — only when `site/**` changes (`npm run check`).
6. **Docs** — docs-only PRs get a cheap whitespace check (no Rust).
7. **Gate** — always-on aggregator for branch protection so skipped scoped jobs
   do not fail required checks.

Concurrency: `cancel-in-progress: true` — newer push cancels older CI on the
same PR ref.

Path scope notes: docs/markdown/assets skip Rust; `Taskfile.yml`, `.githooks/**`,
and non-CI `scripts/*` also skip Rust (`scripts/ci-affected.sh` and
`.github/workflows/ci.yml` still force workspace self-test). Dependants expand
to a fixed point (e.g. `impetus-acp-gateway` pulls `impetus-core` and then
core's consumers including `impetus-tui`). Platform paths (sandbox/Seatbelt,
PTY, Keychain/auth, ACP child profile, no-sudo daemon tests) set `macos=true`.

Selector self-check: `bash scripts/tests/ci-affected.sh`.

Ubuntu 24.04 PR CI is the **full affected Rust quality gate** (not merely a
compile guard). What release smoke must still prove on a clean machine
(doctor, daemon, no secrets in logs, Linux data-dir override):
[ubuntu-smoke.md](ubuntu-smoke.md).

## Docs capability claims check

Selected capability claims (sandbox level, durable artifacts, tool schema gate,
Seatbelt wrap, extension runtime) live in
`crates/impetus-core/tests/fixtures/docs_capability_claims.json` and are checked
against `CapabilityTruthReport::gather` (same source as `impetus doctor --json`
capability probes). No secrets in the fixture.

```zsh
cargo test -p impetus-core --test docs_capability_claims
```

When capability truth changes intentionally: update the fixture (and keep
ARCHITECTURE / ROADMAP wording aligned). Do not invent capabilities that
`CapabilityTruthReport` does not report.

Preview what CI would select:

```zsh
task ci:affected
```

### Required GitHub branch-protection check

Mark **only** `Gate` (job name under workflow `CI`) as required for auto-merge.
Do not require the internal macOS/Linux/security/site jobs individually — they
may be skipped when out of scope.

## Full request-flow coverage (#15)

Issue #15 asked for end-to-end Memory harness flows with a deterministic mock
provider, success + error paths, and CI execution. That acceptance is already
met by existing `--lib` tests — **no duplicate** under
`crates/impetus-core/tests/`.

| Criterion | Evidence (PR CI: `--lib --bins`) |
| --- | --- |
| Harness setup / teardown | `Harness::with_test_provider` + `tempfile` workspace in `harness_api` tests |
| Prompt → policy → NeedsApproval → Resolve → resume → observation | `harness_api::approval_resume_returns_durable_tool_observations_to_the_model` (`MemoryEventStore` + `MockProvider::scripted`) |
| Error path (reject) | `harness_api::rejected_approval_records_denial_and_resumes_without_execution` |
| Error path (cancel) | `harness_api::cancellation_stops_an_active_agent_run_without_a_final_answer` |
| Complementary EffectSeam slice | `security_runtime_pr` (#174): approve/reject, sandbox deny, redaction, artifacts |
| Named capability sentinels (#315) | `sentinel_protocol` / `sentinel_git` / `sentinel_files` / `sentinel_events` / `sentinel_artifacts` |

Run the vertical slice locally:

```zsh
cargo test -p impetus-core --lib approval_resume_returns_durable_tool_observations_to_the_model
cargo test -p impetus-core --lib rejected_approval_records_denial_and_resumes_without_execution
cargo test -p impetus-core --lib security_runtime_pr
cargo test -p impetus-protocol --lib -- sentinel
cargo test -p impetus-core --lib -- sentinel
```

Honest boundary: these are colocated lib tests (same binaries PR CI already
runs). Heavy `crates/*/tests/` suites (Seatbelt production E2E, audit-log IPC
fixtures) stay outside the PR gate by design — see below.

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
| `task bench` | Local Criterion event-log baselines (not a PR CI / `verify` gate). |
| `task ci:affected` | Print CI scope for current branch vs `origin/main`. |
| `task security` | `cargo audit` + `cargo deny`. |

## Change boundaries

- Keep `impetus-core` independent of native GUI, a terminal renderer, and a
  particular client.
- Do not store raw secrets in SQLite, JSONL, logs, test fixtures, or config.
- Treat process, PTY, network, and filesystem effects as harness capabilities;
  clients do not own policy or the SQLite connection.

See [CONTRIBUTING.md](../../CONTRIBUTING.md).
