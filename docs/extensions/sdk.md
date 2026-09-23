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
- `host_protocol`: newline-delimited JSON-RPC (`initialize` / `shutdown` /
  `ping` / `operate` / `cancel`), payload limit, secret-key reject,
  permission gate helpers

## Host process operate RPC

Stdio JSON-RPC (one object per line). Host owns request id correlation,
timeouts, cancel, crash cleanup. Child advertises `supported_ops` +
`extension_api_version` on initialize (compat negotiate).

| Method | Role |
| --- | --- |
| `extension/initialize` | Protocol + API negotiate; optional `supported_ops` |
| `extension/operate` | Typed op + `request_id` + bounded `params` + optional permission |
| `extension/cancel` | Cancel in-flight operate by `request_id` |
| `extension/shutdown` / `ping` | Lifecycle / liveness |

Rules: no shell spawn; no raw secrets in params (Keychain labels only);
manifest permission gate before dispatch (`echo` exempt); line size ≤
`MAX_HOST_RPC_LINE_BYTES`. CDP/LSP stay in extensions, not core.

Fixture: `crates/impetus-extension-sdk/fixtures/host-process-echo/`.
Daemon IPC: `OperateExtensionPackage` / `ExtensionOperate` (cap
`extension_manage`); client helper `HarnessClient::operate_extension_package`.

## Host responsibility

The SDK validates documents. The host (`impetus-core`) discovers packages,
runs activate-time permission → SandboxScope gate (`extension_policy`), owns
lifecycle state, exposes `ExtensionCapabilityRegistry` for AgentLoop / Context,
spawns `host_process` children after `extension/initialize`, and dispatches
`ExtensionHost::operate` / `cancel_operate` with crash cleanup (also via IPC
`OperateExtensionPackage`). Mutating actions still go through Policy at action
time.
