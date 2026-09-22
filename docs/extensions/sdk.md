# Extension SDK reference

Crate: `impetus-extension-sdk`.

**Dependency:** git `rev` or path pin — not crates.io yet. Full recipe:
[depending-on-sdk.md](./depending-on-sdk.md).

Depends on: `serde`, `serde_json`, `semver`, `thiserror`, `toml`.
Does **not** depend on UI, rusqlite, harness, or daemon.

## Primitives

- `ExtensionId`
- `ExtensionApiVersion` / `SupportedApiRange` / `EXTENSION_API_VERSION`
- `check_compatibility`
- `ExtensionPermission` + `validate_permissions`
- `ExtensionCapabilityKind` (closed set)
- `ExtensionEntrypoint`
- `ExtensionConfigSchema` / `ConfigScope`
- `ExtensionPackageManifest` (`from_toml_str` / `from_json_str` / `validate`)
- Trait `Extension` (fixtures only): initialize / activate / deactivate /
  shutdown / health / reload_config

## Host responsibility

The SDK validates documents. The host (`impetus-core`) discovers packages,
runs activate-time permission → SandboxScope gate (`extension_policy`), owns
lifecycle state, exposes `ExtensionCapabilityRegistry` for AgentLoop / Context,
and spawns `host_process` children after `extension/initialize` handshake.
Mutating actions still go through Policy at action time.
