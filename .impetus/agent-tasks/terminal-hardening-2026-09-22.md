# Impetus terminal hardening handoff

Target: current `1tuz/impetus` `main` after IPC v14 / ExtensionHost landing.

Run in GOAL + AUTO mode. Use the maximum practical number of independent subagents, then a second review wave. Do not use real provider credentials, interactive login, Keychain prompts, browser auth, or live external agents during verification.

## Ground rules

- Rebase/update to current `main` first. Do not assume old PR branches are current.
- Treat code as source of truth. Keep `TODO.md` and `ARCHITECTURE.md` synchronized with reality.
- Do not mark an area Implemented if the production daemon/client path is stubbed, declare-only, or only mock-tested.
- Preserve the small trusted kernel: Policy -> Approval -> Sandbox -> Capability -> Execution.
- Preserve no-root/no-sudo/no-password normal operation.
- Prefer extension-first implementations for optional capabilities; do not grow `impetusd` with duplicated Browser/LSP/Memory implementations when the public extension boundary can host them.

## P0: ACP permission API consistency — DONE

Shipped: gateway transport-only (`Select|Deny`); durable NeedsApproval only in
`AcpAdapter` → ApprovalRequested → ResolveApproval. `respond_permission` no-op
and NeedsApproval→Cancelled-without-broker removed (#339 / #360). Tests:
`acp_adapter` unit + `daemon_unix_acp_permission` E2E. Do not reintroduce a
second permission API.

## P0: provider/model options end-to-end — DONE

Shipped: `StreamOptions` carries service_tier + generic provider_options;
catalog discover → SetSessionModel validate → persist → stream body
(`openai_provider` mock HTTP + harness
`set_session_model_service_tier_and_options_reach_stream` + daemon_unix_e2e
persist/restart). No silent fallback on unsupported tiers/options.

## P0: extension host usable by real first-party extensions

The package host is present but richer `host_process` operation is still open. Finish the public contract needed by the separate `impetus-extensions` repo.

Implement a versioned, bounded host-process RPC surface for extension capabilities. At minimum support the operations required by the planned first-party Browser/LSP/reference extensions without private-core imports.

Requirements:
- request id + typed operation + typed result/error;
- cancellation/timeouts;
- process crash isolation and cleanup;
- manifest capability/permission gate before dispatch;
- payload size limits;
- no implicit shell execution;
- no raw secrets in extension protocol;
- extension API compatibility negotiation;
- real daemon E2E using a deterministic local fixture extension.

If Browser/LSP need a core API not available yet, add the smallest generic capability contract; do not copy their implementation into core.

## P1: unify extension / legacy Skill + MCP source of truth

Eliminate split-brain between legacy CLI install inventory and daemon ExtensionHost/package state.

Acceptance:
- one canonical daemon-owned effective inventory;
- CLI is presentation/control over the same state;
- deterministic migration of existing legacy installs;
- no duplicate activation of the same MCP/Skill capability;
- restart persistence tests.

## P1: IPC and schema hardening

- Extend validate-on-wire coverage to all security-relevant/mutating requests and responses.
- Keep IPC compatibility range behavior; client must retain negotiated version + capabilities and gate optional calls.
- Add adjacent-version deterministic tests and unsupported-capability tests.
- Do not bump IPC version unless wire shape/capability contract actually changes.

## P1: AttachmentStore session binding

Bind `GetAttachment` to session ownership where the attachment is approval/diff scoped.

Acceptance:
- cross-session read denied;
- missing/expired attachment deterministic error;
- TUI/client fetch uses session id;
- tests cover owner vs foreign session.

## P1: child/subagent progress

Add bounded mid-run child action/progress events to the parent durable stream without leaking hidden reasoning.

Include only structured status/tool/action summaries. Never expose hidden chain-of-thought.

Acceptance:
- Started -> bounded Progress/Action -> Finished/Failed/Cancelled ordering;
- parent/session isolation;
- reconnect cursor works;
- event flood bounded/coalesced.

## P1: same-uid local isolation

Harden the Unix socket beyond filesystem mode where practical:
- set restrictive umask before bind;
- inspect peer credentials on supported Unix platforms;
- prevent ACP child processes marked `IMPETUS_ACP_CHILD=1` from opening the control-plane socket unless explicitly authorized;
- preserve normal CLI/TUI/Desktop clients;
- fail closed on ambiguous privileged/control-plane paths.

Do not introduce root helpers or password prompts.

## P1: ACP remaining production gaps

Finish the honest remaining ACP checklist with deterministic mocks where possible:
- richer tool/status stream mapping into durable harness events / ToolOrchestrator boundary;
- disconnect/restart semantics: durable session survives and unknown work is never reported Completed;
- agent registry/discovery/version probing based on installed capabilities, not hard-coded vendor flags;
- meaningful ACP health/status surface;
- ACP-specific redaction/export audit;
- quarantine/remove legacy custom JSON-RPC gateway from production selection while preserving migration/tests if needed.

Live Codex/Claude/etc. smoke stays ignored/manual; deterministic SDK mock must cover CI behavior.

## P1: LSP/Browser modularity

Do not implement a giant built-in browser stack merely to clear TODO.

- Core/daemon owns generic capability contracts, policy, lifecycle, IPC and extension dispatch.
- Concrete CDP/WebDriver Browser implementation belongs in `impetus-extensions` unless there is a proven kernel requirement.
- Concrete language-server integrations should use the generic LSP/extension contract; keep process isolation/restart/cancel in generic infrastructure.
- Wire request cancellation and the already-declared diagnostics/symbol surfaces if they are part of the existing public contract.
- No duplicate core + extension sources of truth.

## P1: TUI model picker

Finish terminal TUI Provider -> Model -> Reasoning -> provider-options selection using daemon catalog only.

Acceptance:
- dynamic catalog;
- unavailable values disabled/rejected;
- active session selection visible;
- state restored after reconnect/restart;
- no vendor-specific hard-coded model lists.

## Tests: offline/non-auth only

Run `scripts/verify-offline-hardening.sh` after implementation.

Also add/extend deterministic tests for:
- ACP exact permission option selection and denial;
- ACP config model/reasoning application;
- provider option/service tier outgoing request JSON;
- invalid/stale provider option rejection;
- extension host-process fixture RPC, timeout, crash, permission deny;
- extension/legacy inventory migration;
- IPC adjacent-version capability gating;
- attachment session isolation;
- child progress reconnect/order/bounds;
- Unix peer/isolation behavior where platform APIs permit;
- daemon restart/session recovery;
- no-sudo/no-password userspace path.

Tests requiring installed agents, API keys, browser OAuth, external network, or interactive Keychain access must remain ignored/manual and must not be required for CI completion.

## Parallel subagents

Use maximum practical parallelism. Suggested independent owners:
1. ACP permission + transport API
2. ACP lifecycle/health/stream mapping
3. provider options / CloseRouter-compatible metadata
4. extension host-process RPC
5. extension/legacy SoT migration
6. IPC/schema validation
7. attachment isolation
8. child progress events
9. Unix peer/security/no-sudo
10. LSP/browser extension boundary
11. TUI model/options picker
12. deterministic E2E/CI
13. docs consistency

Then start a fresh independent review wave: architecture, security, ACP, providers, extensions, IPC, E2E, docs. Fix findings and rerun verification.

## Definition of done

- `scripts/verify-offline-hardening.sh` passes on supported local platform(s).
- CI-equivalent fmt/check/clippy/tests pass without credentials.
- No non-ignored test asks for auth, network credentials, sudo, Touch ID, or Keychain UI.
- No known public no-op/stub on the production path.
- Optional concrete capabilities stay modular/extension-first.
- `TODO.md` contains only genuinely deferred work; `ARCHITECTURE.md` status matches code.
- Produce a final short report listing changed files, test commands/results, remaining external-only manual smokes, and any core API blockers for `impetus-extensions`.

After completing the work, remove this handoff file from the final product commit unless the maintainer explicitly wants to keep it.
