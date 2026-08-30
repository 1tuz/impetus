# TODO.md Phase Status Audit — 2026-08-30

## Summary

Audit of TODO.md claims vs actual implementation. Phase completion marks `[x]` verified against codebase.

---

## Phase 0 — Foundation ✅

**Status**: Fully complete as claimed.

All core infrastructure verified:
- SQLite WAL Event Log, durable sessions, client reconnect, cursor backfill
- Policy/Approval/Sandbox/Capability gates functional
- Effect seam with deferred approval execution
- Versioned local IPC (Unix Domain Socket)

---

## Phase 1 — Binary topology & diagnostics ✅

**Status**: Fully complete as claimed.

Verified:
- Binary split: `crates/impetus` (client), `crates/impetusd` (daemon)
- `impetus doctor` with human + JSON output (`crates/impetus/src/doctor.rs`)
- Probe coverage: versions, socket, IPC handshake, Event Store, Artifact Store, sandbox

---

## Phase 2 — Module Runtime ✅

**Status**: Fully complete as claimed.

Verified:
- `ServiceRegistry`, `ModuleRegistry` in `crates/impetus-core/src/module_runtime.rs`
- `ModuleDescriptor` with capability negotiation
- Execution semantics (read_only, idempotent, mutating, non_replayable)
- `UnknownOutcome` enforcement
- Tests: `crates/impetus-core/tests/module_runtime.rs` (compatibility, fallback, lifecycle)

---

## Phase 3 — Extension compatibility ⚠️ **OVERSTATED**

**Claimed**: 3 items marked `[x]`
- Extension Compatibility Adapter layer
- Canonical types (CanonicalModuleSpec, CanonicalSkill, etc.)
- Import capability matrix (SUPPORTED | PARTIAL | UNSUPPORTED | INCOMPATIBLE)

**Reality**: Types exist (`crates/impetus-core/src/extension_compat.rs`), but:
- All `ImportCapability` entries are `Unsupported` (no real adapter logic)
- No integration tests for adapters
- Agent Skills, MCP, Plugins, Claude/Codex/Cursor adapters: **NOT IMPLEMENTED**

**Recommendation**: Mark as `[ ]` until at least one adapter has working import + tests.

---

## Phase 4 — Output optimization ✅

**Status**: Fully complete as claimed.

Verified:
- Structured observations: `TestObservation`, `DiffObservation`, `SearchObservation`, `PipelineObservation` in `crates/impetus-core/src/observations.rs`
- `DurableArtifactStore` with SHA-256 addressing (`crates/impetus-core/src/durable_artifacts.rs`)
- Integrated into `tools.rs` with `ArtifactRef` for bounded outputs
- RTK adapter with capability probing (`crates/impetus-core/src/rtk_adapter.rs`)

---

## Phase 5 — Agent runtime (continued) ✅

**Status**: 6/6 complete.

Verified:
- Durable budget tracking (`crates/impetus-core/src/budget.rs`)
- Model Router with selection rules (`crates/impetus-core/src/model_router.rs`)
- Multi-turn conversation state with tool result accumulation
- Streaming response chunking
- Error recovery and retry logic (respects `UnknownOutcome`)
- Parallel tool execution (read_only/idempotent only)
- **Cross-session isolation**: `delete_session()` in EventStore (`crates/impetus-core/src/storage.rs`) + tests (`crates/impetus-core/tests/cross_session_isolation.rs`)
- **Audit log**: `AuditLog` query interface with redaction (`crates/impetus-core/src/audit_log.rs`) + tests (`crates/impetus-core/tests/audit_log_redaction.rs`)

---

## Phase 6 — Context & sessions ⚠️ **PARTIALLY OVERSTATED**

**Claimed**: 1 item marked `[x]`
- Durable `ArtifactStore`

**Verified**: ✅ `DurableArtifactStore` implemented and integrated.

**Unclaimed but SHOULD be marked `[x]`**:
- Session fork/checkpoint: `fork_session()` exists in `EventStore` trait and implementations

**NOT implemented** (correctly marked `[ ]`):
- Lazy module/tool/MCP description loading
- HOT/WARM/COLD context tiers
- Token-budgeted module/tool selection
- Session fork **without full event duplication** (current impl copies all events)
- Session DAG (parent/fork, restore/revert)
- Large paste handling (bracketed paste, chunked upload, ArtifactRef)

**Recommendation**: Mark `fork_session()` as `[x]`, add note that it duplicates events (not shared prefix yet).

---

## Phase 7 — TUI (standalone `impetus`) ❌

**Status**: All `[ ]` as claimed. No TUI implementation started.

Basic client exists (`crates/impetus/src/main.rs`), but no Ratatui/Crossterm, no composer, no streaming renderer.

---

## Phase 10 — Security & verification ❌

**Status**: Mostly `[ ]` as claimed.

Only "Client examples: minimal read-only observer" marked `[x]` — verified in `crates/impetus-client/examples/`.

Security review, E2E verification NOT done.

---

## WEB / INTERNET RESEARCH ❌

**Status**: All `[ ]` as claimed. No WebSearchService, no DuckDuckGo/Bing backends, no BrowserService.

---

## Corrections needed in TODO.md

### Phase 3 — Extension compatibility

Mark the 3 completed items as `[ ]` until real adapter logic + tests exist:

```diff
- [x] Extension Compatibility Adapter layer (design + minimal slice)
- [x] Canonical types: `CanonicalModuleSpec`, `CanonicalSkill`, `Instruction`, `AgentProfile`, `Command`, `McpModule`, `ToolProvider`
- [x] Import capability matrix: `SUPPORTED | PARTIAL | UNSUPPORTED | INCOMPATIBLE`
+ [ ] Extension Compatibility Adapter layer (types exist; no working adapters yet)
+ [ ] Canonical types (defined but not used in real imports)
+ [ ] Import capability matrix (all entries Unsupported; no real capability detection)
```

### Phase 6 — Context & sessions

Add `fork_session()` to completed items:

```diff
+ [x] Session fork/checkpoint (with full event duplication; shared prefix not yet implemented)
  [x] Durable `ArtifactStore` (metadata + content survives restart; SHA-256 refs)
```

---

## Actual completion status by phase

- **Phase 0**: ✅ 100%
- **Phase 1**: ✅ 100%
- **Phase 2**: ✅ 100%
- **Phase 3**: ⚠️ ~10% (only type definitions, no working adapters)
- **Phase 4**: ✅ 100%
- **Phase 5**: ✅ 100%
- **Phase 6**: ⚠️ 20% (ArtifactStore + basic fork; no context optimization)
- **Phase 7**: ❌ 0%
- **Phase 10**: ❌ ~10% (only client examples)
- **WEB / INTERNET RESEARCH**: ❌ 0%

---

## Next priority work

Based on open issues and TODO.md:

1. **Issue #23**: Policy-gated coding agent vertical slice (integration of existing pieces)
2. **Phase 6**: Context optimization (lazy loading, tiered context, token budgeting)
3. **Phase 3**: Real extension adapters (start with Agent Skills or MCP)
4. **Issue #6**: Keychain credential management
5. **Phase 7**: Standalone TUI (Ratatui + streaming renderer)

---

**Audit date**: 2026-08-30  
**Auditor**: Codewhale (automated code verification)
