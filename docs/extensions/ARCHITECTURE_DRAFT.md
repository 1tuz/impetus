# Extension subsystem architecture (draft → #324)

Status: **design in progress** — not Implemented until GOAL criteria met.

## Split

| Repo | Owns |
| --- | --- |
| `impetus` (this) | Extension API/SDK, manifest schema, discovery, load/validate/compat, permissions bridge, lifecycle, capability registry, AgentLoop wiring, daemon IPC, isolation, infra tests |
| `impetus-extensions` (external) | First-party packs, examples, extension-author tests/docs |

Core must not depend on concrete first-party extension crates.

## Layers

```text
impetus-extension-sdk   # stable types + traits (no UI / rusqlite / harness)
        ↑
impetus-core            # host: discovery, policy, registry, AgentLoop inject
        ↑
impetusd / IPC          # list/get/enable/disable/reload/status/caps/compat
```

Existing Skill/MCP **install lifecycle** (`extension plan|install|migrate|…`) is
the control plane for daemon-owned inventory under `$IMPETUS_DATA_DIR`. Workspace
`--root` layout remains for project-local installs; `extension migrate` copies
Enabled rows into the daemon SoT without duplicate MCP/Skill activation.

## Precursors in core (do not reinvent blindly)

| Existing | Role today | Relation to SDK host |
| --- | --- | --- |
| `extension_*` + `ExtensionRuntime` | CLI install + Enabled inventory IPC | Control plane over daemon SoT (`$IMPETUS_DATA_DIR`); feeds effective inventory + legacy skill roots |
| `module.rs` / `ModuleLifecycle` | Library OOP module states + unix IPC stubs | Reuse lifecycle state vocabulary; do not use `temp_dir` sockets as prod SoT |
| `plugins::CapabilityRegistry` | Known permission string allowlist | Expand allowlist to SDK permission enum; wire into Policy |

## Manifest

Package root: `extension.toml` (preferred) or `extension.json`.

Required concepts:

- `id`, `name`, `version`, `description`, `author`
- `extension_api_version` (semver major; **not** Impetus app version)
- `entrypoint` (typed; see below)
- `capabilities` (typed enum tokens)
- `permissions` (explicit; default deny)
- `configuration` (JSON Schema + defaults + scope)
- optional `dependencies` on other extension ids

Legacy `impetus.extension.v1` (Skill/MCP digest install) remains valid for
CLI install plans; SDK packages use `impetus.extension_package.v1` envelope.

## Entrypoint kinds (closed set)

| Kind | Execution | Notes |
| --- | --- | --- |
| `instruction_pack` | Declarative files | Skills/instructions under pack; AgentLoop via registry |
| `mcp_bridge` | Existing MCP SoT | Declares MCP module id; spawn still via ToolProviderRuntime |
| `host_process` | Out-of-process | Crash-isolated; JSON-RPC over stdio (`initialize` / `operate` / `cancel`) |

No `dlopen` / arbitrary in-process native ABI in v1. In-process Rust
`Extension` trait is for **fixtures/tests** only (`catch_unwind` boundary).

## Compatibility

- Core advertises `EXTENSION_API_VERSION` + supported range.
- Manifest `extension_api_version` must fall in range or load is rejected
  (`CompatError`) without affecting other extensions.
- Impetus crate version bumps must not force ecosystem breakage unless API
  major changes.

## Discovery (user-writable only)

1. Global: `$IMPETUS_DATA_DIR/extensions/packages/<id>/`
2. Workspace: `{workspace}/.impetus/extensions/<id>/` (no extra trust)
3. Dev: `$IMPETUS_DATA_DIR/extensions/dev/<id>/` (symlink/path allowlist)

No root-owned paths. Workspace location ≠ elevated permissions.

## Permissions

Manifest permissions map into existing Policy → Approval → Sandbox. Extensions
cannot grant themselves `origin=user` or bypass RiskGate. Categories (v1):

`filesystem_read`, `filesystem_write`, `network`, `process_spawn`, `pty`,
`git`, `mcp`, `browser`, `lsp`, `memory`, `secrets_provider`.

## Lifecycle

`discover → validate → compat → permission_eval → load → initialize →
activate → operate → config_reload → deactivate → unload`

Failures are per-extension; daemon continues. Removed-on-disk → mark Failed /
drop from registry on reload.

## AgentLoop

Enabled extensions expose capabilities through `ExtensionCapabilityRegistry`.
AgentLoop / InstructionResolver consume **registry views**, not ad-hoc path
scans of install_state. No special-case module ids.

## IPC (daemon SoT)

Beyond inventory list/get: enable, disable, reload, status, errors,
capabilities, compatibility state. Clients never own extension loading.
