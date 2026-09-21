# Capability leases and RepoMap (extension notes)

Design-only. Not implemented. Kept short so backlog stays honest.

## Capability leases

**Intent:** temporary, scoped elevation for one action / turn / session —
e.g. write only under `path/`, network only to `host`, then revoke.

**Current building blocks (reuse later):**

- `EffectSeam` + `AdmittedOperation` (one-shot admission proof)
- Approval fingerprint + `intent_revision`
- `PolicyConfig` overrides (session-wide, not TTL)
- Explore `allowed_tools` / `write_roots` (structural subset, not lease)

**Why not now:** lease id, TTL, revoke IPC, and mid-turn scope mutation need
Harness policy to be shared mutable state plus event/audit surface. That is a
second permission channel beside Policy → Approval → Sandbox. Shipping it
before Seatbelt / Explore E2E / MCP runtime would create a parallel abstraction.

**Natural fit later:** extend `DeferredEffect` / approval resume with optional
`LeaseGrant { capability, target, expires_at, single_use }` applied inside
`EffectSeam::execute_after_approval_*` only. No new top-level permission engine.

## RepoMap (Aider-style derived context)

**Intent:** disposable compact repo map — key files, symbols, edges — injected
into child/parent context. Derived only; never source of truth.

**Existing hooks (do not invent a second index yet):**

- `coding_tools` / `DocumentSymbol` / definition / references
- IPC `GotoDefinition` / coding surfaces
- Optional `LspBackendModule`
- `context_optimizer` COLD refs + `ContextBuilder`

**Why not full map now:** needs indexer lifecycle, ignore rules, freshness,
and budgeted serialization into ContextBuilder. Large enough to distract from
live Explore / MCP wiring.

**Extension point:** add `RepoMapProvider` trait later that returns a labeled
artifact (or short markdown) from workspace root; ContextBuilder / Explore
`context_label` path consumes it. Prefer LSP/coding_tools-backed implementation
over a new parser crate.
