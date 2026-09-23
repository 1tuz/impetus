# Impetus final hardening goal

Work on the current `main` of the terminal Impetus repository.

Use GOAL mode and AUTO subagents. Use as many parallel subagents as the environment supports for independent areas, then run a second independent review wave before declaring completion.

Do not reduce scope to MVP. Do not move existing production obligations to Later/Parked just to close the task.

## Current known gaps to close

### 1. Split PR CI from nightly CI

The current `.github/workflows/ci.yml` still runs separate detect, Linux, macOS, security, site and gate jobs on pull requests. Replace that design with two independent workflows:

- `.github/workflows/pr-fast.yml`
- `.github/workflows/nightly.yml`

PR Fast requirements:

- trigger on `pull_request` and `workflow_dispatch`
- one Ubuntu runner whenever practical
- `git diff --check`
- `cargo fmt --all -- --check`
- only a lightweight affected-crate `cargo check`
- no automatic macOS runner
- no clippy
- no cargo test
- no integration/E2E
- no cargo audit/deny
- docs-only changes skip Rust setup and Cargo entirely
- `cancel-in-progress: true` per PR
- optimize for minimum wall-clock time, not maximum PR coverage
- keep one stable required check name

Nightly Full requirements:

- trigger every day at 02:00 UTC+3, therefore cron `0 23 * * *`
- also support `workflow_dispatch`
- full Linux quality and tests
- workspace clippy with warnings denied
- workspace tests
- real daemon E2E
- ACP, approvals, providers, MCP, workflows, extensions, Git/worktrees, PTY, protocol compatibility and failure paths
- security checks such as cargo audit/deny
- separate macOS platform validation for Seatbelt, Keychain, no-sudo/no-password, PTY, socket/filesystem permissions and macOS-specific regressions
- no real external API credentials required; use mocks/local fixtures
- independent jobs should run in parallel
- only one nightly run at a time

The old PR-heavy `ci.yml` must not remain active in parallel.

Local developer/agent validation must stay ultra-light by default:

- `cargo fmt --all -- --check`
- `git diff --check`

Do not automatically run workspace check/clippy/tests/E2E locally. A targeted test/check is allowed only to diagnose a concrete failure.

### 2. Provider options and service tiers end-to-end

**Shipped (verify):** catalog discovery → SetSessionModel validation/persist →
`StreamOptions` → OpenAI-compatible request body (`service_tier` + generic
`provider_options`). No silent fallback on unsupported values. Coverage:
`openai_provider::request_body_includes_service_tier_and_provider_options`,
harness `set_session_model_service_tier_and_options_reach_stream`,
`daemon_unix_e2e` persist across restart.

Remaining honesty: CloseRouter-specific fields stay adapter metadata, not core
enums — keep that boundary when adding providers.

### 3. ACP permission/approval path cleanup

**Shipped (verify, do not reintroduce):** gateway is transport-only
(`PermissionDecision::Select|Deny` only). Durable `NeedsApproval` is brokered
solely in `AcpAdapter` → `ApprovalRequested` → `ResolveApproval` → exact ACP
option. Public `respond_permission` no-op and `NeedsApproval`→Cancelled without
a broker were removed (#339 / #360 / `daemon_unix_acp_permission`).

Required production flow (must remain the only path):

ACP permission request
-> Impetus policy
-> durable ApprovalRequested
-> ResolveApproval
-> exact ACP PermissionOption mapping
-> response to ACP
-> agent continues

Coverage: unit (`acp_adapter` needs_approval_*) + daemon E2E approve/deny.

### 4. Real daemon E2E coverage

Extend the current real Unix-socket daemon E2E so it covers the remaining production boundaries instead of only unit-level state.

At minimum cover:

- protocol handshake/negotiation
- create session
- model selection
- reasoning selection
- provider option/service tier selection
- prompt/stream
- durable approval path
- ACP permission mapping where feasible with a mock ACP agent
- MCP mutation, not only list/reload
- Git
- Files/Diff
- workflow execution
- extension lifecycle/capability execution
- PTY attach/detach
- client reconnect
- daemon restart/session restore
- persisted model/reasoning/provider options

No real cloud credentials.

### 5. Extension host public capability surface

Keep the modular harness philosophy: trusted core stays small; optional functionality is replaceable and disableable.

The public extension contract currently has richer capability kinds than the fully usable host surface. Finish enough public host API so independent extension repositories can implement real capabilities without private-core hacks.

Priority public extension capabilities:

- tool/command execution contract
- context provider
- browser integration contract
- LSP integration contract
- memory provider contract
- skill provider / AgentLoop injection

Requirements:

- explicit manifest/version compatibility
- explicit permissions
- policy/approval boundaries remain in Impetus core
- extension cannot bypass sandbox/policy/approval
- extension failure must not crash the daemon
- enable/disable/reload and restart lifecycle remain deterministic
- independent external repository can build against public SDK only

Do not copy first-party extension implementation back into core if it can live in `impetus-extensions`.

### 6. Decide Browser/LSP/Memory boundary cleanly

Avoid a half-core/half-extension duplicate architecture.

For each of Browser, LSP and Memory:

- decide what is generic trusted-core infrastructure
- decide what is replaceable extension implementation
- remove duplicate ownership and ambiguous sources of truth
- keep only the minimum stable core seam needed by external extensions

Browser must not stay permanently in a state where core is partial while extensions cannot implement it either.

LSP generic lifecycle must have a clear owner and support startup/shutdown, workspace binding, request routing, cancellation/crash/restart semantics appropriate to the chosen boundary.

Memory core session/event durability must remain distinct from optional long-term memory providers.

### 7. IPC negotiation and capability gating

Verify the client stores and uses the negotiated protocol version and capability set, not merely the server accepting a range.

Tests should cover:

- latest client/latest server
- latest client/previous supported server
- previous supported client/latest server
- no compatible version
- feature unavailable under negotiated capabilities

### 8. Documentation truth

After code changes, reconcile all of:

- README.md
- TODO.md
- ARCHITECTURE.md
- EXTENSION_REPOSITORY_CONTRACT.md
- provider/protocol/security docs

Remove stale statements that still describe already-merged feature branches or old Seatbelt/MCP state.

Rules:

- `Implemented` means production wired and tested
- `Partial` means genuinely partial
- do not say `Nothing open` while required production gaps remain
- do not describe a stale branch as the source of already-merged functionality

## Required subagent decomposition

Use independent subagents at minimum for:

1. PR/nightly CI architecture
2. Rust caching/affected-crate strategy
3. provider options/service tiers
4. ACP approvals
5. daemon E2E
6. extension public API
7. Browser/LSP/Memory boundary
8. IPC compatibility
9. documentation consistency
10. security/no-sudo regression review

After implementation, launch a fresh review wave for architecture, tests, CI, ACP, provider options, extensions and docs. Reviewers must not be the same agents that implemented the area when the environment allows it.

## Completion criteria

Do not mark the goal complete until all are true:

- PR pipeline is ultra-light and does not run macOS/tests/clippy/security by default
- nightly full pipeline runs at 02:00 UTC+3 and manually on demand
- local default checks are only fmt/diff plus optional tiny targeted diagnostics
- service tier/provider options reach real provider requests and persist correctly
- ACP has one complete durable permission path with no contradictory incomplete public fallback
- real daemon E2E covers approval/provider options/MCP mutation and the other critical boundaries
- external extension repositories can implement real capabilities through public SDK/host contracts
- Browser/LSP/Memory ownership is explicit and non-duplicated
- negotiated IPC capabilities are actually used client-side
- Linux/nightly and macOS/nightly validations pass
- docs match code

## Final report

Keep the final report concise:

- Goal: reached/not reached
- Changed
- PR CI timing before/after
- Nightly composition and schedule
- Provider options/service tiers status
- ACP approval status
- Extension API status
- E2E coverage
- Tests/checks run
- Remaining external blockers only
