# Component Management

## Overview

Components in Impetus include:
- **Built-in tools** (bash, read, write, edit, search) — always available, bundled with `impetus-core`
- **External modules** (Phase 2+) — optional extensions loaded via Module Runtime
- **Compatibility adapters** (Phase 3+) — bridges to external formats (MCP, Agent Plugins, etc.)

## Component Versioning and Reproducibility

### Version/Digest Lock Concept

**Goal:** Ensure reproducible builds and deterministic agent behavior across environments.

**Mechanism:**
- Each component has a stable `id`, semantic `version`, and content `digest` (SHA-256)
- Lock file (`impetus-lock.json`) captures exact component versions and digests used
- On load, harness verifies digest match and warns on version drift

**Lock file format (draft):**
```json
{
  "schema_version": 1,
  "harness_version": "0.1.0",
  "components": {
    "bash": {
      "type": "builtin",
      "version": "0.1.0",
      "digest": "sha256:...",
      "source": "impetus-core"
    },
    "custom-module": {
      "type": "external",
      "version": "1.2.3",
      "digest": "sha256:...",
      "source": "file:///path/to/module.wasm",
      "compatibility": "module_runtime_v1"
    }
  }
}
```

**Lifecycle:**
1. `impetus components list --lock` generates/updates lock file
2. Lock file committed to repo alongside `.codewhale/` or project config
3. On harness start, verify loaded components match lock (warn/error on mismatch)
4. Lock file respected by CI, deployment pipelines, team environments

**Trade-offs:**
- ✓ Reproducible agent behavior
- ✓ Audit trail for component changes
- ✓ Safe rollback on component upgrade issues
- ✗ Requires digest calculation and storage
- ✗ Manual lock update on intentional component upgrade

**Phase 1 status:** Concept defined, implementation deferred to Module Runtime (Phase 2+).

---

## Update/Disable Flows

### Design: Component Lifecycle Without Marketplace

**Constraint:** No centralized marketplace, no auto-update daemon, no phone-home telemetry.

**Principles:**
- User-initiated updates only
- Explicit enable/disable per component
- Clear provenance and trust model
- Local-first: file paths, git URLs, static binaries

### Update Flow

**Built-in tools:**
- Updated via `impetus` / `impetusd` upgrade (Cargo, Homebrew, release binary)
- No independent versioning (tied to harness version)

**External modules (Phase 2+):**
1. User discovers module (docs, community, local development)
2. User registers module descriptor (file path or URL) with harness
3. Harness probes compatibility, permissions, health
4. User approves load (explicit consent gate)
5. Module loaded into isolated process/sandbox
6. Module appears in `impetus components list`

**Update check (optional, manual):**
- `impetus components check-updates` queries registered module sources
- Displays available updates with changelog/compatibility notes
- User chooses: `impetus components update <id>` or `--all`
- New version probed and loaded after approval

**Disable flow:**
- `impetus components disable <id>` marks module as disabled
- Disabled modules not loaded on harness start
- `impetus components enable <id>` reverses
- Built-in tools cannot be disabled (always available)

**Removal:**
- `impetus components remove <id>` unregisters external module
- Cached binaries/artifacts removed from local storage
- Built-in tools cannot be removed

### Trust and Provenance

**Source types:**
- `builtin` — bundled with harness, implicitly trusted
- `file://` — local path, user-owned
- `https://` — remote URL, checksum required (SHA-256 or GPG signature)
- `git+https://` — git repo + commit SHA, reproducible build

**Verification:**
- External modules: checksum or signature verified before load
- Compatibility: harness checks module protocol version and capabilities
- Permissions: user approves filesystem, network, secrets access

**No auto-trust:** User must explicitly approve each module and its permissions.

### Phase 1 Status

**Implemented:**
- `impetus components list` — shows built-in tools and registry state
- `impetus components status [id]` — health and metadata

**Deferred to Phase 2+ (Module Runtime):**
- External module registration, loading, isolation
- Update check/apply flows
- Enable/disable/remove operations
- Lock file generation and verification

**Phase 1 completion criterion:** Concept documented, built-in introspection working.

---

## References

- [ARCHITECTURE.md](../ARCHITECTURE.md) § Module Runtime
- [docs/ROADMAP.md](ROADMAP.md) § Phase 2
- `impetus components --help` for CLI usage
