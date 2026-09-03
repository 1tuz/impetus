# Rules for Coding Agents

Style (caveman), YAGNI/ponytail, **RTK**, and token reduction — see Codewhale constitution / `~/.codewhale/RTK.md`:

- Global: `~/.codewhale/constitution.json` + `append_system_prompt` (RTK on every shell)
- Repo: `.codewhale/constitution.json`
- **Every `bash`:** only through `rtk …` (`rtk cargo`, `rtk git`, `rtk rg`, …)

**Subagents (CodeWhale):** only on explicit request or clear benefit; cap ≤5, **no more than 2 builders simultaneously**. On spawn immediately: `worktree: true`, full `write_roots` (if touching `Cargo.toml`/tests — include crate root, not just `src/…`), one narrow slice per child. **`task verify` — once by parent**, not in every child. On `wall_time_budget` / API error — checkpoint + re-dispatch one worker, not a batch of 5.

This file covers product boundaries and verification for this repo only.

## Immovable Boundaries

- Harness-first: current stage — standalone Rust runtime and CLI. Standalone TUI — first-class planned client; do not start a custom PTY/ANSI terminal emulator without a documented unmet requirement.
- Zap uses its own UI and connects Impetus as agent backend; separate adapter or personal fork are acceptable. Do not copy Zap/Warp client internals into harness core.
- `impetus-core` and headless runtime do not depend on terminal renderer, native GUI, or specific client.
- Client does not own SQLite connection, secrets, SSH transport, or policy. It sends typed requests and displays durable events/approvals from harness.
- Every typed action has `origin=user|agent` and goes through `Policy → Deny | Allow | NeedsApproval`; only `Allow` or user-accepted approval continues through `Sandbox → Capability → Execution`. Model cannot grant itself `origin=user` or approval.
- Secrets stored only in macOS Keychain. In SQLite, JSONL, tracing, typed payloads, and tests — only reference labels, never token/private key/passphrase.
- Do not use `latest` and unpinned git dependencies.

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

After Rust changes, must execute:

```zsh
cargo fmt --all -- --check
cargo test --workspace
cargo check --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

For harness/provider/ACP/auth changes, add test without secrets: stream/cancel/restart, profile validation, policy decision, and redaction/export.

`task verify` is short equivalent of four required Rust commands. `task setup` checks environment and installs repository-owned hooks.

## CI and Verification

- Before handoff of Rust changes, execute `task verify` (locally).
- **PR Rust CI:** one macOS job runs `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, and `cargo test --workspace --lib --bins`; Clippy replaces a separate CI `cargo check`. Rust CI runs only for Rust/workflow changes and does not rerun on `main` after a merged PR.
- **Integration CI:** `cargo test --workspace` runs only from the manual/nightly macOS workflow. Integration tests from `crates/*/tests/` stay outside the PR path because they require macOS Seatbelt and compile slowly in Docker. Locally run full `task verify` with integration tests.
- **Dependency security CI:** `cargo audit` and `cargo deny` run only when `Cargo.toml`, `Cargo.lock`, `deny.toml`, or their workflow changes. Pages runs only for `site/**` changes.
- On `Cargo.toml` or `Cargo.lock` changes, execute `task security`; do not ignore RustSec/CVE, license/source/bans findings without versioned entry in `deny.toml` with specific reason.

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
3. **Before push:** mandatory `task verify` (fmt, test, check, clippy)
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
7. Required PR CI passes → **GitHub auto-merges to main**
8. After merge: `git checkout main && git pull` for next task

#### Auto-merge Setup (once per project)

In GitHub Repository Settings → General → Pull Requests:
- ✓ "Allow auto-merge"
- ✓ "Automatically delete head branches"

In Branch protection rules for `main`:
- ✓ "Require status checks to pass before merging"
- Can enable auto-merge for each PR via `gh pr merge --auto --squash`

### Commit Rules

- Divide work into atomic commits by single reason for change; do not mix tooling, product code, and independent documentation without necessity.
- Before commit, execute `task verify`. For docs-only changes, additionally check links/diagrams with applicable local validator.
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
- **Archived/obsolete docs:** `docs/archived/`, `docs/superpowers/`, historical audits/spikes/roadmaps (current: `ARCHITECTURE.md`, `ROADMAP.md`)
- **Generated HTML/diagrams:** `*.html` in root or `docs/` (except explicitly versioned reference docs)
- **IDE/tool artifacts:** `opencode.json`, `.DS_Store`, `__pycache__/`, `*.pyc`
- **Runtime state:** `*.db`, `*.db-shm`, `*.db-wal`, session logs, trace dumps

Before commit, check `git status` and `git diff --cached`. If accidentally staged forbidden file — `git reset HEAD <file>` and add to `.gitignore`.
