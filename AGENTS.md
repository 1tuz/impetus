# Rules for Coding Agents

Style (caveman), YAGNI/ponytail, **RTK**, and token reduction — see Codewhale constitution / `~/.codewhale/RTK.md`:

- Global: `~/.codewhale/constitution.json` + `append_system_prompt` (RTK on every shell)
- Repo: `.codewhale/constitution.json`
- **Every `bash`:** only through `rtk …` (`rtk cargo`, `rtk git`, `rtk rg`, …)

**Subagents (CodeWhale):** only on explicit request or clear benefit; cap ≤5, **no more than 2 builders simultaneously**. On spawn immediately: `worktree: true`, full `write_roots` (if touching `Cargo.toml`/tests — include crate root, not just `src/…`), one narrow slice per child. **`task verify` (cheap fmt) — once by parent**, not full workspace compile in every child. On `wall_time_budget` / API error — checkpoint + re-dispatch one worker, not a batch of 5.

This file covers product boundaries and verification for this repo only.

## Persistent project memory

Before non-trivial work read `TODO.md` and `ARCHITECTURE.md`.
After implementation update their status in the same change.
Stale documentation is a bug. `TODO.md` = open work only (no `[x]` with
Partial tails). `ARCHITECTURE.md` = Implemented only when production
daemon/client path works end-to-end.

## Immovable Boundaries

- Harness-first: current stage — standalone Rust runtime and CLI. Standalone TUI — first-class planned client; do not start a custom PTY/ANSI terminal emulator without a documented unmet requirement.
- Zap uses its own UI and connects Impetus as agent backend; separate adapter or personal fork are acceptable. Do not copy Zap/Warp client internals into harness core.
- `impetus-core` and headless runtime do not depend on terminal renderer, native GUI, or specific client.
- Client does not own SQLite connection, secrets, SSH transport, or policy. It sends typed requests and displays durable events/approvals from harness.
- Every typed action has `origin=user|agent` and goes through `Policy → Deny | Allow | NeedsApproval`; only `Allow` or user-accepted approval continues through `Sandbox → Capability → Execution`. Model cannot grant itself `origin=user` or approval.
- Secrets stored only in macOS Keychain. In SQLite, JSONL, tracing, typed payloads, and tests — only reference labels, never token/private key/passphrase.
- Do not use `latest` and unpinned git dependencies.
- Reject features whose only justification is vendor parity or feature-count optics.

## Harness and Client Protocol

- Controlled shell/process/PTY — execution capability. ANSI parser, tabs, scrollback, and terminal renderer — client function; do not mix these concepts.
- Versioned local IPC must support capability negotiation, prompt/stream/status/cancel, typed approvals/diffs, and explicit `Incompatible` state.
- Client disconnect or crash must not destroy durable session or report unknown outcome as `Completed`.
- Basic Zap path — Zap's own UI with connected Impetus backend. Structured integration built as separate adapter/fork; OSC/notification hooks do not replace typed protocol.
- Do not add local HTTP UI, Electron/WebView, or Node runtime to harness. Composition of separate personal Zap fork does not expand harness dependency/trust boundary.

## ACP and Models

- ACP — protocol between client and external coding-agent, not universal provider API or authorization storage.
- For ACP backend, authorization belongs to selected agent CLI; application launches it only after explicit user action and displays its profile/status.
- `agent-client-protocol = 2.x` means major Rust SDK crate; do not enable draft protocol v2 features without separate RFC and compatibility tests.
- For direct provider auth use exactly one variant: Keychain API-key reference, system-browser OAuth, or local/no-secret. No raw token field in client and no secret passed to model.
- URL-mode OAuth opens only with user confirmation in system browser; URL visible in full. Do not use WebView.
- Support for specific Codex/Claude/Cursor/Gemini/Qwen backend determined by installed version and ACP registry/discovery, not assumption about CLI flag.

## Verification

Local Mac stays cheap. Default before push:

```zsh
task verify
# ≡ cargo fmt --all -- --check && git diff --check
```

Do **not** run locally by default: `cargo test --workspace`, `cargo clippy --workspace`,
`cargo check --workspace`, daemon/ACP/MCP/Workflow/PTY E2E. Quality lives on
GitHub Actions. If CI fails one test, run **only that test** for diagnosis.

Optional full local suite (rare): `task verify:full`.

For harness/provider/ACP/auth logic under test on CI: mocks only — no live keys,
Keychain UI, or browser OAuth.

`task setup` checks environment and installs repository-owned hooks.

## CI and Verification

- **Before push:** `task verify` (fmt + `git diff --check` only).
- **PR Fast** (`.github/workflows/pr-fast.yml`) — sole required check for merge:
  - One Ubuntu job named **`PR Fast`**
  - `git diff --check` + `cargo fmt --check`
  - `cargo check` on **directly affected** crates only (no dependant fan-out;
    `Cargo.toml`/`Cargo.lock` → `--workspace`)
  - Docs/tooling-only: no Rust toolchain
  - **No** clippy, tests, security, macOS, E2E
  - `cancel-in-progress` per PR number
- **Nightly Full** (`.github/workflows/nightly.yml`) — deep quality:
  - Schedule **02:00 UTC+3** (`cron: "0 23 * * *"`) + `workflow_dispatch`
  - Linux: fmt, clippy `-D warnings`, check, full `cargo test --workspace`
  - Security: `cargo audit` + `cargo deny`
  - macOS: Seatbelt / no-sudo / PTY / Unix socket platform suite
  - Site npm check; selector self-test
  - Concurrency group `nightly-full` (one at a time)
- Local `task security` / `task verify:full` optional; do not ignore RustSec
  findings without a versioned `deny.toml` entry.
- Preview affected packages: `task ci:affected`.

## Git and Commits

### Feature Branch Workflow (strictly required)

**NEVER PUSH DIRECTLY TO `main`.** Any push to main without PR is workflow violation.

**Check current branch:** before starting work, always execute `git branch --show-current` and verify not on `main`. Tasks from TODO.md taken sequentially; each task = one issue + one feature branch.

#### Before Starting Work

1. **Check current branch:** `git branch --show-current` — if `main`, stop and create feature branch
2. **Check open issues:** `gh issue list` — select next task from TODO.md
3. **Create issue** if does not exist (each task from TODO.md = issue)
4. **Create feature branch from current main:**
   ```bash
   git checkout main
   git pull origin main
   git checkout -b feature/issue-42-short-description
   ```
   Template: `feature/issue-N-description` or `fix/issue-N-bug-name`

#### Workflow

1. Work in feature branch (never in `main`)
2. Atomic commits: each with `closes #N`, `fixes #N`, or `refs #N`
3. **Before push:** mandatory `task verify` (fmt + `git diff --check`)
4. **Push to feature branch:**
   ```bash
   git push -u origin feature/issue-42-short-description
   ```
5. **Create PR via CLI:**
   ```bash
   gh pr create --fill
   ```
   Or via GitHub Web UI
6. **Enable auto-merge** in PR: `gh pr merge --auto --squash` after creation
7. Required check **`PR Fast`** passes → **GitHub auto-merges to main**
8. After merge: `git checkout main && git pull` for next task

Agent loop: `fmt` → `git diff --check` → commit → push → **PR Fast** → merge.
Full regression runs on **Nightly Full** (or manual `workflow_dispatch`).
Do not run full local/workspace CI before every push.

#### Auto-merge Setup (once per project)

In GitHub Repository Settings → General → Pull Requests:
- ✓ "Allow auto-merge"
- ✓ "Automatically delete head branches"

In Branch protection rules for `main`:
- ✓ "Require status checks to pass before merging"
- ✓ Required check: **only** `PR Fast` (job name under workflow `PR Fast`)
- Do **not** require Nightly Full, macOS, Linux Quality, Security, or old `Gate`
- Can enable auto-merge for each PR via `gh pr merge --auto --squash`

Head branches delete automatically after merge (`delete_branch_on_merge`).
Do not leave merged feature branches on the remote.

### Commit Rules

- Divide work into atomic commits by single reason for change; do not mix tooling, product code, and independent documentation without necessity.
- Before commit, execute `task verify` (fmt + `git diff --check`). Full
  `task verify:full` is optional and usually unnecessary — PR Gate covers quality.
- **Commit message in English.** Format: `type: Brief summary (closes #N)` or `type(scope): Summary (refs #N)`
- Allowed types: `feat`, `fix`, `docs`, `refactor`, `perf`, `test`, `build`, `ci`, `chore`, `revert`
- Subject <= 72 characters, starts with lowercase (after `type:`), no trailing period
- **Issue-driven workflow (strictly required):** each commit must reference issue via `closes #N`, `fixes #N`, or `refs #N`. If no issue — **stop and create issue first**. Work without issue is forbidden.
- Body (optional) describes "what" and "why", not "how". Wrap at 72 characters.
- Examples:
  - `feat: add subsystem health probes to doctor (closes #42)`
  - `fix(ipc): handle large enum variants with Box (refs #38)`
  - `docs: update implementation history for phase 2 (refs #15)`
- Do not use `--no-verify`, do not commit secrets, `.env`, local DBs, provider credentials, browser caches, `target/`, and generated runtime state.
- Do not amend/rebase/force-push or configure remote without explicit user instruction.

## Forbidden Files and Directories in Repository

Following categories of files and directories **forbidden** in commits and must be in `.gitignore`:

- **Build artifacts:** `target/`, `**/target/`, any compiled binaries and intermediate build outputs
- **Temporary configs:** `config/` with example/template configs (only versioned `.example` files in `docs/` or root allowed)
- **Archived/obsolete docs:** do not reintroduce `docs/superpowers/` or loose
  historical audits outside `docs/archive/`; current truth is root
  `ARCHITECTURE.md` + `docs/architecture/roadmap.md` + `TODO.md`
- **Generated HTML/diagrams:** `*.html` in root or `docs/` (except explicitly versioned reference docs)
- **IDE/tool artifacts:** `opencode.json`, `.DS_Store`, `__pycache__/`, `*.pyc`
- **Runtime state:** `*.db`, `*.db-shm`, `*.db-wal`, session logs, trace dumps

Before commit, check `git status` and `git diff --cached`. If accidentally staged forbidden file — `git reset HEAD <file>` and add to `.gitignore`.
