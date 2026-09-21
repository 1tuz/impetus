# TODO — Impetus

Executable roadmap. **Code is source of truth.** Status labels:

- **Implemented** — production path + tests
- **Partial** — library/import exists; gaps remain
- **Planned** — not started or deferred

Detail and evidence: [ARCHITECTURE.md](ARCHITECTURE.md).
Narrative: [docs/ROADMAP.md](docs/ROADMAP.md).

Rule: mark Implemented only when the vertical slice works end-to-end. Types-only
or import-only adapters are Partial.

---

## P0 — Runtime correctness (now)

Gate before P1 product features.

### 1. Provider-native protocol adapters

- [x] Wire `OpenAiProvider` (Chat Completions SSE + tool-call assembly) into
      `impetusd` / `Harness` via `OpenAiNativeAdapter`
- [x] Fix stream end handling so accumulated tool calls emit before
      `[DONE]` / `message_stop` early-return
- [x] Export `AnthropicProvider` (still optional / not default daemon path)
- [x] Keep legacy OpenAI-compatible text adapter in tree for compatibility
- [x] Explicit `ProviderProtocolAdapter` trait boundary (shared assembler)
      (`provider_protocol_adapter.rs`: trait + `ToolCallAssembler`; used by
      `openai_provider.rs` + `anthropic_provider.rs`)
- [x] OpenAI Responses API (`/v1/responses`) — Partial (opt-in via profile
      `openai_http_api=responses`; Chat Completions remains default; shared
      `ToolCallAssembler`; fixture SSE unit tests)

Evidence: `openai_provider.rs`, `openai_native_adapter.rs`, `openai_responses.rs`,
`anthropic_provider.rs`, `provider_protocol_adapter.rs`, `impetusd` `--provider-profile`.

### 2. Mandatory tool argument validation

- [x] Validate model tool args against tool JSON Schema **before** policy/execution
      (`tool_schema::validate_tool_arguments` in `ToolOrchestrator::normalize_tool_call`)
- [x] Reject malformed args without reaching executor (typed `ToolArgError` /
      `OrchestratorError::InvalidArguments`; no silent coercion at the schema gate)
- [x] Provider HTTP `tools` schemas — OpenAI Chat Completions + Anthropic
      Messages `tools` from `builtin_tool_schemas()` (`openai_tools_payload` /
      `anthropic_tools_payload`; `provider_http_tools: true` in capability truth)

### 3. Single durable ArtifactStore semantics

- [x] Doctor text: distinguish path-scope sandbox vs Seatbelt process wrap
- [x] Wire measured provider usage into `BudgetChecker::record_usage` via
      `record_turn_with_usage`
- [x] Process/shell stdout/stderr → durable artifact when large
      (`ProcessExecutionRequest::execute` + preview/`ArtifactRef` on bash path)
- [x] Ephemeral `AttachmentStore` for approval previews (keep; document as non-durable)
- [x] `DurableArtifactStore` for truncated tool/web/paste bodies

### 4. Context engine + durable compaction

- [x] HOT/WARM/COLD + lazy descriptions + token-budgeted assemble
- [x] ContextBuilder chunked artifact summarize
- [x] Shared-prefix fork + named checkpoints
- [x] Wire measured provider usage into `BudgetChecker::record_usage`
- [x] Compaction as durable events (`CompactionStarted` / range / summary refs /
      `CompactionCompleted`) executed from agent loop — not silent history rewrite
- [x] Structural state (permissions, cwd, budgets, parent, worktree) never only in
      text summary (`CompactionStructuralState` on `CompactionCompleted`)

### 5. Security / runtime E2E in PR CI

Keep suite small. PR CI today: macOS `fmt` + `clippy -D warnings` +
`cargo test --workspace --lib --bins`.

- [x] Add focused lib/bin tests (or tiny PR-safe suite) covering:
  - [x] approve → execute; reject — `security_runtime_pr` + harness
        `approval_resume_*` / `rejected_approval_*` (#174; full-flow #15
        accepted via same Memory+MockProvider harness lib tests — no
        duplicate under `crates/impetus-core/tests/`; see
        `docs/development.md` § Full request-flow coverage)
  - [x] cancel — harness `cancellation_stops_an_active_agent_run_*` (lib)
  - [x] reconnect / attach after daemon restart (where feasible without Seatbelt)
        — runtime `attach_recovers_pending_approval_*` /
        `reattach_recovers_the_exact_deferred_tool_arguments` (lib; no daemon
        process restart / Seatbelt)
  - [x] sandbox deny → no execution — `security_runtime_pr` + `effects` path-scope
        fail-closed (lib). Seatbelt process wrap still out of PR path.
  - [x] `UnknownOutcome` / retry blocked for mutating — `security_runtime_pr` +
        `module_fallback` (lib)
  - [x] secret redaction — `redact_tool_outcome` + tools
        `client_visible_tool_output_redacts_*` (lib). Deeper audit-log IPC
        fixtures stay in `tests/audit_log_redaction.rs` (not `--lib`).
  - [x] durable artifact restore — `security_runtime_pr` reopen +
        `tool_orchestrator` large_*_survives_store_reopen (lib)
- [x] Do **not** move full Seatbelt integration into every PR; keep nightly/manual
      (`tests/macos_sandbox_spike.rs` / Seatbelt remain non-PR)

### 6. Capability truth generation

- [x] `impetus doctor --json` (and human doctor) reflects real capability matrix
      (providers wired, seatbelt vs path-scope, artifact stores, extensions runtime,
      tool_schema gate) via `CapabilityTruthReport`
- [x] Prefer generating/checking docs claims from doctor JSON where practical
      (`crates/impetus-core/tests/docs_capability_claims.rs` +
      `tests/fixtures/docs_capability_claims.json` vs `CapabilityTruthReport::gather`)

---

## P1 — Operator / extension / orchestration layer

Start after P0 foundations are solid. Keep the **built-in agent set small**; prefer
composable primitives over a catalog of overlapping skills/commands/hooks.

Orchestration stack (names may vary if cleaner boundaries exist):

```text
AgentScheduler + WorkflowEngine + WorktreeManager
```

Agents remain **execution roles**. Workflows own step ordering. Worktrees are
**managed resources** with durable ownership — not “spawn git worktree and hope”.

### 1. Versioned canonical schemas

- [x] Shared schema registry (`schema` module): ids + `schema_version` convention
- [x] Stable schemas with deterministic validation (partial — registry slice):
  - [ ] `impetus.session.v1`
  - [ ] `impetus.extension.v1`
  - [ ] `impetus.mcp.v1`
  - [x] `impetus.capabilities.v1`
  - [x] `impetus.approval_detail.v1` wired into shared registry (#189/#191 pattern)
- [ ] Provider/harness-specific details nested; do not leak into common fields
- [x] Compatibility evolution (version field + reject unknown critical fields)
  (registry helpers; full payload coverage still growing)

### 2. Extension lifecycle (not file copy)

Lifecycle:

```text
Manifest → ResolutionPlan → InstallPlan → Apply → ExtensionState
```

- [x] Dry-run plan before filesystem mutations where practical
      (`extension_lifecycle::plan_install`: ResolutionPlan + InstallPlan for
      Skill / MCP config intents; no write; create vs modify classification)
- [x] CLI: `extension plan | install` (wraps `plan_install` / `apply_install`;
      project DBs under `{root}/.impetus/`)
- [ ] CLI/IPC: `extension doctor | repair | remove`
- [x] Persist install state: created paths, modified paths, source, version/digest,
      ownership, installation ID
      (`apply_install` + `ExtensionStateStore`; lookup by `installation_id`)
- [x] Live MCP tools in ToolOrchestrator / agent loop (beyond import-only adapter)
- [ ] Small extension contract: `SKILL.md`, MCP config, manifest, capabilities, digest
- [x] Skills import + filesystem instruction path (`InstructionResolver`, CLI)
- [x] MCP **import** adapter (JSON-RPC client library)

### 3. Ownership safety (first-class invariant)

Default:

`destination exists + no matching Impetus ownership record = do not overwrite`

- [x] Ownership records: path, owner, source, digest, version, installation ID
- [x] Uninstall removes **only** resources Impetus can prove it owns
- [x] Repair never overwrites unrelated user changes without explicit policy/approval
      (`OwnershipStore::repair`; digest mismatch refuses unless `force`)
- [x] Pre-existing user files never silently become Impetus-owned

### 4. Memory trust model

Separate explicitly:

```text
Runtime State ≠ Memory ≠ Policy
```

| Store | Role |
| --- | --- |
| `EventStore` | Authoritative runtime/session state |
| `MemoryStore` | Contextual knowledge (untrusted by default) |
| `PolicyStore` | Governed instructions and permissions |

- [x] Memory never auto-promotes to policy or tool/sandbox capability
- [x] Scopes: project / team / user; provenance; secret filtering
      (`MemoryScope` / `MemoryProvenance` on `MemoryEntry`; store path
      reuses `tools::redact_text`; fake-token unit tests)
- [x] Create-only or append-safe semantics where appropriate
      (`remember` create-only / `append` append-safe; `MemoryStoreError::AlreadyExists`;
      no silent overwrite; unit tests)
- [ ] Derived indexes disposable/rebuildable; no unsafe symlink traversal
- [ ] Human-readable source format where useful

### 5. WorktreeManager

Managed resource lifecycle (persist + recover after daemon restart):

create → resume → pause → stop → diff → review → merge-ready → conflict →
stale → close → salvage

- [x] Create/resume/stop/close with durable session ↔ worktree binding
      (`WorktreeManager` + SQLite bindings; git CLI; temp-dir tests)
- [x] Diff / merge-ready / conflict checks before merge attempts
      (`diff_summary` / `check_merge_ready` / `attempt_merge`; `merge-tree`
      conflict refuse; temp-repo tests)
- [x] Safe cleanup + abandoned/stale detection
      (`detect_stale` / `mark_stale` / `cleanup_stale`; path-missing +
      not-registered; temp-repo tests)
- [x] Salvage path for recoverable abandoned worktrees
      (`salvage` re-registers + preserves on-disk files; same `worktree_id`)
- [x] Worktree identity survives compaction/resume
      (`CompactionStructuralState.worktree_id` + `resolve_after_compaction`;
      attach restores id from CompactionCompleted)
- [x] Build-role agents prefer isolated worktrees with attached permissions
      (`create_for_role(Build)` + durable `WorktreeAttachedPermissions`;
      `enforce_write` / `to_sandbox_scope` hook; temp-dir tests)

### 6. WorkflowEngine + small recipes

Do **not** invent a new hard-coded agent type per workflow.

- [ ] Declarative recipes (examples):
  - Feature: Research → Plan → Tests → Implement → Review → Approval
  - Bug: Reproduce → Failing regression → Fix → Review
  - Refactor: Baseline tests → Characterization if needed → Refactor → Validation → Review
- [ ] Engine owns: step order, dependencies, budgets, concurrency, retry,
      checkpoints, cancellation, result propagation
- [ ] AgentScheduler schedules roles; WorkflowEngine sequences steps

### 7. Subagents (explicit roles, not a swarm)

- [ ] Roles: Explore (read-only), Research (read + approved web), Build (worktree),
      Review (read-only diff/tests)
- [ ] Structured child metadata: `parent_id`, `cwd`, `worktree`, `allowed_tools`,
      `write_roots`, `max_tokens`, `max_time`, `max_depth` — **not** prompt-only
- [ ] Persist child results before parent resume
- [ ] Concurrency caps enforced in harness

### 8. Steer vs follow-up

- [ ] Steer running task vs enqueue follow-up — distinct from a normal user message

### 9. Hooks (only if needed; performance-first)

- [ ] Cheap match/filter **before** spawning expensive processes
- [ ] Security-critical hooks prefer in-daemon / trusted runtime, not arbitrary
      external processes by default
- [ ] Measure per-tool-call overhead; add perf tests if hooks land
- [ ] Avoid large overlapping hook catalogs
- [x] Event log query baselines (append_next / list / cursor backfill) — Criterion
      benches + `docs/benchmarks/v0.2.md`; local `task bench` only, not PR CI gate
      (#16)

### 10. Anti-sprawl

- [ ] Keep built-in agent/skill/command set small; detect unused/duplicates
- [ ] No features solely for vendor parity or feature-count optics

### 11. LSP

- [ ] First-class coding tools: definition, references, diagnostics, symbols, hover
- [ ] No hard couple of runtime to one LSP binary

### 12. Web / research

- [x] Search + fetch + SSRF + citations/provenance (core path)
- [x] Session outbound / private-network grants
- [ ] Optional API search backends (Tavily/Exa) as replaceable modules only
- [ ] Real browser providers (Firefox/Chrome/…) — optional, not core deps

---

## P2 — Advanced orchestration (explicitly deferred)

- [ ] Large multi-team / swarm orchestration beyond small WorkflowEngine recipes
- [ ] Plugin marketplace / large plugin ABI
- [ ] Portable sessions between harnesses
- [ ] Deep Claude/Codex/Cursor compatibility **runtime** (imports already Partial)
- [ ] Autonomous long-running planner/tester loops
- [ ] Ubuntu 24.04 release tier + clean-machine smoke
- [ ] Full Zap discovery/authorize production protocol

---

## Frontends (ongoing; not blocking P0)

TUI uses `HarnessClient` only (`impetus-tui` boundary tests).

- [x] Ratatui/Crossterm adopted; composer single/multi; large paste upload; streaming
- [x] Bounded markdown (#146)
- [x] Diff view (#148)
- [x] Approval UI — `Overlay::Approval` / `ApprovalDetail` in `impetus-tui` (`render_approval*`, ingest/resolve in `app.rs`; tests `approval_requested_then_approve_clears_queue_and_overlay`, `approval_deny_and_detail_paths`; #165 via #169)
- [x] Session picker — `render_session_picker` + `SessionSummary` mapping (`model.rs` / `render.rs`; tests `session_picker_filters_and_activates_selected`, `session_summary_maps_fork_meta_and_optional_overrides`, `filtered_sessions_matches_label_id_and_workspace`; #166 via #169)
- [x] Command palette — `render_command_picker` + `Overlay::Commands` (`command::suggestions`, Ctrl+P; tests `command_palette_opens_filters_and_runs_selected`, `command_palette_down_selects_and_runs_command`; #175)
- [x] Scrollback/status polish — footer status strip (`format_status_strip`: connection +
      run + budget from `UiEvent`/`BudgetState`); PageUp/Dn + resize clamp
      (`clamp_timeline_scroll` / `note_timeline_metrics`); tests
      `status_strip_includes_connection_run_and_budget`,
      `page_keys_scroll_timeline_and_clamp_on_resize`, `scroll_clamp_caps_offset_after_shrink`
      (#178)
- [x] Redraw coalescing; error + remediation UX — tick-gated paint via
      `should_coalesce_redraw` (stream text still
      `chunks_coalesce_into_one_assistant_item`); timeline noise coalesce
      (`push_or_coalesce_noise`); error notices show explicit or static remediation hint
      + toast; tests `consecutive_notice_noise_coalesces_into_one_item`,
      `error_notice_shows_explicit_or_static_remediation`, `redraw_coalesce_waits_for_tick`
      (#179)

---

## Historical checklist (superseded)

Older Phase 0–10 checkboxes lived here and over-marked “done”. Prefer P0/P1/P2
above. For archaeology see git history and `docs/TODO_AUDIT_2026-08-30.md`
(dated; do not treat as current).
