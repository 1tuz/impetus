# Extension Repository Contract

**Audience:** maintainers of the separate `impetus-extensions` repository
(and any third-party extension author).

**Status:** **Usable via git pin** (issue #324). SDK `publish = false` — not a
crates.io ABI yet. Pin an Impetus git `rev` (see
[docs/extensions/depending-on-sdk.md](docs/extensions/depending-on-sdk.md)).
Docs mark the subsystem **Partial** until crates.io/tag story lands.

You should **not** need to read Impetus daemon private modules to author an
`instruction_pack`. Depend on:

- crate `impetus-extension-sdk` (git/path pin)
- this document + `docs/extensions/`
- wire types in `impetus-protocol` for IPC (`extension_manage`, IPC ≥ 14)

## What works today

### `instruction_pack`

1. Discover packages under user-writable roots (see local-development.md).
2. Validate `extension.toml` **before** load (`extension_api_version` range).
3. Reject incompatible API versions without crashing the daemon.
4. Active packs feed AgentLoop / Context via `ExtensionCapabilityRegistry`
   skill roots (not install_state path scans).
5. Daemon IPC: `ReloadExtensionPackages`, `ListExtensionPackages`,
   `GetExtensionPackage`, `EnableExtensionPackage`, `DisableExtensionPackage`,
   `OperateExtensionPackage` (cap `extension_manage`). Clients do not load
   packages themselves.
6. Per-package load failures are isolated (other packs continue).
7. Durable disable across daemon restart
   (`$IMPETUS_DATA_DIR/extensions/disabled_packages.json`).
8. Activate-time permission → Policy/SandboxScope gate (`extension_policy`):
   hard-Deny against scope blocks Active (e.g. `network` when
   `allow_network=false`). Tokens still do not bypass RiskGate / `origin=user`.

### `mcp_bridge`

Activate enables existing `$IMPETUS_DATA_DIR/mcp/{module_id}.json` (upsert via
`mcp_manage` first); disable sidelining that module.

### `host_process`

Spawn + `extension/initialize` / `extension/shutdown` JSON-RPC handshake
(see `docs/extensions/host-protocol.md`); deactivate kills the child. No shell;
no absolute command; `process_spawn` required.

Daemon IPC (cap `extension_manage`): `OperateExtensionPackage` →
`ExtensionOperate` (Active pack only; permission gate + secret-key reject +
timeout). Client helper: `HarnessClient::operate_extension_package`.
Well-known ops include `echo`, `tool/call`, `command/invoke`, coding/*,
browser/*, `memory/recall|store`, `context/contribute` (see SDK `ops`).

## Remaining (do not assume shipped)

| Item | Status |
| --- | --- |
| crates.io publish of SDK | No — git `rev` pin only |
| Tool/Command → AgentLoop tool catalog | Ops declared; AgentLoop still uses built-in + MCP tools (not extension Tool/Command catalog) |
| `LspIntegration` / `BrowserIntegration` host operate | **Public** (#362): coding/* + browser/* ops routed when Active host_process present; core `ProcessLspBackend` / Absent remain fallback |
| `MemoryProvider` / `ContextProvider` operate | **Public ops** `memory/recall|store`, `context/contribute` (#362); core MemoryStore IPC stays session SoT (#363) — not a second store |
| `impetus extension …` CLI for packages | No — that CLI is legacy Skill/MCP install |
| Concrete CDP/WebDriver / language installers | Stay in `impetus-extensions` — not core |

## Package layout

```text
my-extension/
  extension.toml
  skills/SKILL.md     # instruction_pack; frontmatter id/scope — see creating-an-extension.md
```

`instruction_pack.root` must be a **relative** path without `..` (contained
under the package directory).

## Author workflow (today)

1. Pin `impetus-extension-sdk` (git `rev` or path) — [depending-on-sdk.md](docs/extensions/depending-on-sdk.md).
2. Write `extension.toml` (`instruction_pack` is the default production path).
3. Drop under `$IMPETUS_DATA_DIR/extensions/packages/<dir>/`.
4. IPC `ReloadExtensionPackages` (auto-activates valid instruction packs).
5. Confirm via `ListExtensionPackages` + session `Context` (skill id present).
6. For `host_process`: `OperateExtensionPackage` (e.g. `op=echo`) →
   `ExtensionOperate`.
7. `DisableExtensionPackage` → skill disappears from Context (durable across
   daemon restart until `EnableExtensionPackage`).

## Compatibility

Declare `extension_api_version` (integer major). Core advertises
`CURRENT_SUPPORTED_RANGE` in the SDK. Out-of-range packs are rejected.

## Testing

- Unit: `ExtensionPackageManifest::from_toml_str` in SDK.
- Reference fixture: `crates/impetus-extension-sdk/fixtures/demo-pack/`
  (mirrored under `crates/impetus-core/test-fixtures/extensions/demo-pack/`).
- Smoke: `cargo run -p impetus-extension-sdk --example validate_demo_pack`.
- Daemon E2E owned by core CI (`daemon_unix_extensions`).

## Migration

Legacy CLI Skill/MCP installs under `.impetus/` remain adapters. New work
authors packages with `extension.toml` for `impetus-extensions`.
