# Ubuntu 24.04 release-tier smoke checklist

Docs slice for [TODO.md](../TODO.md) P2 item **Ubuntu 24.04 release tier +
clean-machine smoke** (#293). Indexes what a clean Ubuntu 24.04 machine must
prove before calling Linux a release tier, and what PR CI already covers.

Status of **this document**: Implemented (honesty index only).

Status of **automated Ubuntu clean-machine smoke / packaging matrix**:
**Planned**. Do not mark the P2 TODO item Implemented because this file exists.

Labels match [ARCHITECTURE.md](../ARCHITECTURE.md): **Implemented** /
**Partial** / **Planned**.

## What PR CI already covers

Workflow: [`.github/workflows/ci.yml`](../.github/workflows/ci.yml). Detail:
[development.md](development.md) § Pull request CI.

| Surface | Runner | What it proves today | Status |
| --- | --- | --- | --- |
| Path detect | `ubuntu-24.04` | Affected crate / docs-only / security / site scope | Implemented |
| Format + Clippy + `--lib --bins` tests | `macos-14` | Affected packages compile clean and unit/bin tests pass | Implemented (macOS only) |
| `cargo check` (affected + dependants) | `ubuntu-24.04` | Linux compile guard — not a second full test suite | Implemented (compile only) |
| `cargo audit` / `cargo deny` | `ubuntu-24.04` | Dependency advisories when lockfiles change | Implemented (path-gated) |
| Site `npm run check` | `ubuntu-24.04` | Landing site only when `site/**` changes | Implemented (path-gated) |
| Docs-only PRs | Gate aggregator | Required check stays green without Rust jobs | Implemented |
| Full `cargo test --workspace` | — | Local `task verify` / `task test` — **not** PR merge gate | Manual / local |
| Integration under `crates/*/tests/` | — | Seatbelt, heavy IPC fixtures, etc. — outside PR gate by design | Manual / local |
| Clean-machine install + daemon smoke on Ubuntu | — | No GitHub Ubuntu smoke job for release tier | Planned |
| Ubuntu GitHub runner matrix beyond compile check | — | Out of scope for this docs slice (#293) | Planned / deferred |

Honest boundary: **Ubuntu PR CI ≠ Ubuntu release smoke**. A green Linux
`cargo check` on a warmed Actions image does not prove a clean Ubuntu 24.04
install path, daemon lifecycle, or secret hygiene under operator conditions.

## What clean-machine Ubuntu 24.04 smoke must prove

Run on a **fresh** Ubuntu 24.04 x86_64 host (or VM) with no prior Impetus data
dir, no cached `target/`, and no developer Keychain/macOS assumptions. Prefer
the published Linux artifact (`scripts/install.sh` / release
`impetus-linux-x86_64`) or a from-source build that matches release toolchain
(`rust-toolchain.toml`).

Record pass/fail per row. Do **not** claim Implemented in architecture docs
until evidence exists outside this checklist.

| # | Proof | Why it matters | Status today |
| --- | --- | --- | --- |
| 1 | Install or build yields `impetus` + `impetusd` on `PATH` (or documented install dir) | Release artifact / build is usable without a macOS checkout | Planned (install script names Linux artifact; no documented clean-machine gate) |
| 2 | Set explicit `IMPETUS_DATA_DIR` (and optional `IMPETUS_SOCKET`) under a Linux-appropriate path (e.g. XDG-style `~/.local/share/impetus`) | Default data root in code still uses macOS `Library/Application Support/Impetus` unless overridden | Planned — override required for honest Linux layout |
| 3 | `impetusd` starts, creates socket + durable store under the chosen data dir, stays up | Daemon is the authority; client alone is not a smoke | Planned |
| 4 | `impetus doctor` / `impetus doctor --json` runs against the live socket and reports versions, socket/IPC, store, and capability probes without crashing | Operator diagnostics must work on the release OS | Partial — doctor Implemented on macOS-first paths; Ubuntu clean-machine evidence missing |
| 5 | CLI create → prompt (mock or local no-secret profile) → stream/status without requiring macOS Keychain | Core harness loop on Linux | Planned |
| 6 | Stop/restart daemon; client reconnect or reattach does not invent `Completed` for unknown work | Durable session honesty across process restart | Planned (lib coverage exists on macOS PR CI; not Ubuntu process smoke) |
| 7 | Logs, JSONL, SQLite, doctor JSON, and CLI output contain **reference labels only** — no raw API tokens, private keys, or passphrases | Secret boundary holds on Linux where Keychain is absent | Planned as clean-machine proof (lib redaction tests exist; not a substitute for operator log review) |
| 8 | Credential path used on smoke is `none`/local loopback **or** an explicitly documented Linux secret-store strategy — never a raw token field in config | AGENTS.md: one auth variant; Keychain is macOS Implemented only | Planned — Linux secret-store parity Planned in roadmap |

Out of scope for this checklist (separate work): full release packaging
automation, Seatbelt/Linux sandbox backends, Windows, ACP hardening (#294),
correctness mega-issues (#296), adding Ubuntu GitHub smoke runners.

## Suggested manual sequence (when executing smoke)

Not automated. Operators adapt paths; keep secrets out of pasted logs.

```zsh
# Fresh host: install toolchain or release binaries, then:
export IMPETUS_DATA_DIR="${HOME}/.local/share/impetus"
export IMPETUS_SOCKET="${IMPETUS_DATA_DIR}/harness.sock"
mkdir -p "${IMPETUS_DATA_DIR}"

impetusd &          # or: cargo run -p impetusd
impetus doctor --json
impetus create
# prompt / stream with mock or localhost none-credential profile
# stop daemon; restart; confirm durable session honesty
# grep/review logs + doctor JSON for secret material (must find none)
```

Fail the smoke if doctor cannot talk to the daemon, if default paths silently
assume macOS layout without documenting the override, or if any secret value
appears in durable or diagnostic output.

## Gaps vs CI (summary)

| Already covered by PR CI | Still requires local / release smoke |
| --- | --- |
| macOS fmt, Clippy, `--lib --bins` | Ubuntu process-level daemon + CLI |
| Ubuntu `cargo check` compile guard | Clean install / artifact boot |
| Path-gated audit/deny + site | Secret hygiene under Linux operator paths |
| Docs-only Gate | Documented Linux data-dir defaults (today: override) |
| Local `task verify` on a developer Mac | Repeatable Ubuntu 24.04 clean-machine evidence |

## Related

- [TODO.md](../TODO.md) — P2 Ubuntu release tier item (remains open)
- [docs/ROADMAP.md](ROADMAP.md) — Platform: Linux install target; sandbox/Keychain parity Planned
- [docs/development.md](development.md) — PR CI vs `task verify`
- [docs/getting-started.md](getting-started.md) — macOS-first developer path
- [ARCHITECTURE.md](../ARCHITECTURE.md) — capability matrix; Linux sandbox Planned
