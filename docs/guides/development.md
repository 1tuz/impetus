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
Optional full local suite: `task verify:full`. Nightly Full covers deep quality.

## Pull request — PR Fast

Workflow: [`.github/workflows/pr-fast.yml`](../../.github/workflows/pr-fast.yml).

One Ubuntu job named **`PR Fast`** (sole branch-protection required check):

1. `git diff --check`
2. Scope via `scripts/ci-affected.sh` (base SHA ↔ head SHA, shallow fetch)
3. Docs/tooling-only → stop (no Rust)
4. Site-only → `npm ci` + `npm run check`
5. Rust → `cargo fmt --check` + `cargo check` on **directly affected** packages
   (`--lib --bins`). No dependant fan-out. Workspace lockfile → `--workspace`.

**Not on PR:** clippy, tests, security, macOS, E2E, integration.

Concurrency: `pr-fast-${{ pr.number }}` with `cancel-in-progress: true`.

Target wall-clock: docs ~10–20s; small warm Rust ~30s; core/workspace <60s
when runner provisioning allows.

## Nightly Full

Workflow: [`.github/workflows/nightly.yml`](../../.github/workflows/nightly.yml).

- Schedule: **02:00 UTC+3** (`cron: "0 23 * * *"`)
- Manual: Actions → Nightly Full → Run workflow
- Parallel jobs: Linux Quality (fmt/clippy/check), Linux Tests (full workspace),
  Security (audit/deny), macOS Platform, Site, selector self-test
- Offline mocks only — no live provider/GitHub credentials
- Concurrency: `nightly-full`, cancel-in-progress

## Docs capability claims check

```zsh
cargo test -p impetus-core --test docs_capability_claims
```

(runs on Nightly / optional local)

Preview what PR Fast would select:

```zsh
task ci:affected
```

### Required GitHub branch-protection check

Mark **only** `PR Fast` as required for auto-merge on `main`.
Do not require Nightly Full or any Nightly job for PR merge.

## Full request-flow coverage (#15)

Issue #15 asked for end-to-end Memory harness flows with a deterministic mock
provider, success + error paths, and CI execution. That acceptance is covered by
colocated `--lib` tests under `harness_api` / `security_runtime_pr` — exercised
on **Nightly Full** (not PR Fast).

| Criterion | Evidence |
| --- | --- |
| Harness setup / teardown | `Harness::with_test_provider` + `tempfile` workspace in `harness_api` tests |
| Prompt → policy → NeedsApproval → Resolve → resume → observation | `harness_api::approval_resume_returns_durable_tool_observations_to_the_model` |
| Error path (reject) | `harness_api::rejected_approval_records_denial_and_resumes_without_execution` |
| Error path (cancel) | `harness_api::cancellation_stops_an_active_agent_run_without_a_final_answer` |
| Complementary EffectSeam slice | `security_runtime_pr` (#174) |
| Named capability sentinels (#315) | `sentinel_*` in core/protocol |

Optional local diagnosis:

```zsh
cargo test -p impetus-core --lib approval_resume_returns_durable_tool_observations_to_the_model
```

## Manual / heavy tests

Prefer Nightly Full / `workflow_dispatch`. Optional local:

```zsh
task verify:full
# or
./scripts/verify-offline-hardening.sh
```

## Useful commands

| Command | Purpose |
| --- | --- |
| `task daemon` | Run `impetusd`. |
| `task client -- <args>` | Run `impetus` CLI. |
| `task bench` | Local Criterion event-log baselines. |
| `task ci:affected` | Print affected package scope vs `origin/main`. |
| `task security` | `cargo audit` + `cargo deny` (also Nightly). |

## Change boundaries

- Keep `impetus-core` independent of native GUI, a terminal renderer, and a
  particular client.
- Do not store raw secrets in SQLite, JSONL, logs, test fixtures, or config.
- Treat process, PTY, network, and filesystem effects as harness capabilities;
  clients do not own policy or the SQLite connection.

See [CONTRIBUTING.md](../../CONTRIBUTING.md).
