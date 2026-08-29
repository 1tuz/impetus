# Instruction Model Refactor Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a lazy, safe instruction model that separates project rules, conventions, guides, and skills.

**Architecture:** A pure resolver discovers optional workspace files and produces stable, scoped context plus estimated token usage. The harness passes that transient context to a compatible multi-message provider API; IPC/CLI expose the live projection, while proposal-only learning stays outside policy and filesystem mutation.

**Tech Stack:** Rust 2024, serde, sha2, Tokio, Clap, existing Unix and in-memory client transports.

**Spec:** `docs/superpowers/specs/2026-08-28-instruction-model-design.md`

## Global Constraints

- Keep `impetus-core` independent of native GUI, terminal rendering, and individual clients.
- Preserve `AGENTS.md` and bare legacy `SKILL.md` support where practical.
- Resolve instruction content transiently; never persist it in events, SQLite, logs, exports, or raw IPC prompt text.
- Instruction declarations cannot alter origin, policy, approvals, sandbox, capabilities, credentials, or execution.
- Use deterministic ordering and bounded, per-file cache invalidation; do not add embeddings, DSLs, package management, or a dependency solver.
- Token telemetry is estimated until backed by a provider tokenizer.
- Do not change the durable event schema in this slice.
- Follow test-first development; run targeted tests per task and `task verify` once in the parent worktree after integration.

---

### Task 1: Record the audited model and delivery boundary

**Files:**
- Create: `docs/instruction-model-audit.md`
- Create: `docs/instruction-model-roadmap.md`
- Modify: `docs/ROADMAP.md`

**Produces:** Current-state audit and a scoped design roadmap linked from the canonical roadmap without claiming it replaces the active v0.6 gate.

- [ ] **Step 1: Write the audit from verified runtime flow**

Document `IpcRequest::Prompt -> Harness::handle -> run_openai_stream -> OpenAiCompatibleProvider`, the absence of an existing instruction resolver, and why `plugins.rs` cannot be reused.

- [ ] **Step 2: Write the scoped roadmap**

Specify four slices: pure resolver, transient provider integration, negotiated context inspection, and proposal-only learning. State compatibility, token/RAM overhead, cache invalidation, and safety boundaries.

- [ ] **Step 3: Link it from the canonical roadmap**

Add one short reference under a non-active design/architecture section. Do not replace or reorder v0.6 work.

- [ ] **Step 4: Validate documentation links**

Run: `rtk rg -n 'instruction-model-(audit|roadmap)' docs README.md TODO.md`

Expected: both documents and the canonical link are present.

### Task 2: Add the pure instruction catalog and resolver

**Files:**
- Create: `crates/impetus-core/src/instructions.rs`
- Modify: `crates/impetus-core/src/lib.rs`

**Produces:** `InstructionResolver`, typed taxonomy, scope matching, explicit skill references, stable projection, per-layer estimated telemetry, and bounded per-file cache.

- [ ] **Step 1: Write failing unit tests**

Cover root `AGENTS.md`, `.impetus/SOUL.md`, bare `SKILL.md`, conventions versus guides versus skills, path/ecosystem scope exclusion, explicit references with deduplication, stable order, and a one-file cache refresh.

- [ ] **Step 2: Run the focused tests to verify failure**

Run: `rtk cargo test -p impetus-core instructions --lib`

Expected: failure because the module and resolver do not exist.

- [ ] **Step 3: Implement the smallest resolver**

Add typed instruction references and a filesystem-only resolver. Resolve `SOUL -> project rules -> conventions -> guides -> selected skills`; keep metadata optional and legacy skill discovery intact.

- [ ] **Step 4: Run focused tests to verify success**

Run: `rtk cargo test -p impetus-core instructions --lib`

Expected: all resolver tests pass.

### Task 3: Integrate transient context and expose it through negotiated IPC

**Files:**
- Modify: `crates/impetus-core/src/provider.rs`
- Modify: `crates/impetus-core/src/harness_api.rs`
- Modify: `crates/impetus-core/src/ipc.rs`
- Modify: `crates/impetus-client/src/lib.rs`
- Modify: `crates/impetus-client/src/unix.rs`
- Modify: `crates/impetus-cli/src/main.rs`

**Consumes:** `InstructionResolver` and its `ResolvedInstructions` projection.

**Produces:** Compatible multi-message provider input, a live context response, matching client transports, and a `context` CLI command.

- [ ] **Step 1: Write failing tests**

Add provider tests proving ordered system context followed by the user message; harness tests proving durable intent remains only user text; IPC and both transport tests proving context round-trip and capability negotiation; CLI parsing test for `context SESSION_ID`.

- [ ] **Step 2: Run focused tests to verify failure**

Run: `rtk cargo test --workspace context --lib --bins`

Expected: failures because message-list and context APIs are absent.

- [ ] **Step 3: Implement the compatible boundary**

Keep `stream_user_message` as a wrapper. Add message-list streaming, resolve only from the harness workspace, add a context IPC capability/request/response, and wire the existing in-memory and Unix clients plus CLI command.

- [ ] **Step 4: Prove safety and persistence boundaries**

Test that a skill declaring `requires: ssh-prod` does not change policy/capability outcomes, and that resolved text is absent from persisted intent events.

- [ ] **Step 5: Run focused tests to verify success**

Run: `rtk cargo test --workspace context --lib --bins`

Expected: all new context tests pass.

### Task 4: Add proposal-only instruction learning

**Files:**
- Create: `crates/impetus-core/src/instruction_learning.rs`
- Modify: `crates/impetus-core/src/lib.rs`

**Produces:** Candidate target classification, lifecycle progression, stricter skill threshold, and no automatic filesystem mutation.

- [ ] **Step 1: Write failing unit tests**

Cover classification to memory/convention/guide update/skill improvement, lifecycle transitions, convention promotion at its threshold, skill non-promotion below its stricter threshold, and no filesystem write.

- [ ] **Step 2: Run the focused tests to verify failure**

Run: `rtk cargo test -p impetus-core instruction_learning --lib`

Expected: failure because the learning module does not exist.

- [ ] **Step 3: Implement deterministic proposal logic**

Use in-memory data only, accept observed evidence explicitly, and return proposals rather than paths or write operations.

- [ ] **Step 4: Run focused tests to verify success**

Run: `rtk cargo test -p impetus-core instruction_learning --lib`

Expected: all learning tests pass.

### Task 5: Integrate and verify the branch

**Files:**
- Review all files changed by Tasks 1-4.

- [ ] **Step 1: Inspect combined diff and run formatting**

Run: `rtk cargo fmt --all -- --check`

Expected: exit code 0.

- [ ] **Step 2: Run mandatory repository verification once**

Run: `rtk task verify`

Expected: formatter, workspace tests, check, and clippy all pass.

- [ ] **Step 3: Run documentation and security checks if applicable**

Run: `rtk git diff --check`

Expected: no whitespace errors. If `Cargo.toml` or `Cargo.lock` changed, additionally run `rtk task security`.

- [ ] **Step 4: Perform whole-branch review**

Review the complete diff against this plan and the design, then resolve any Critical or Important findings before handoff.
