# Permissions

Extensions declare permissions in the manifest. Default: **deny**.

## Categories (v1)

| Token | Intent |
| --- | --- |
| `filesystem_read` | Read workspace / allowed roots |
| `filesystem_write` | Write under sandbox |
| `network` | Outbound network |
| `process_spawn` | Spawn processes |
| `pty` | PTY sessions |
| `git` | Git / worktree operations |
| `mcp` | MCP integration |
| `browser` | Browser capability |
| `lsp` | LSP integration |
| `memory` | Memory provider |
| `secrets_provider` | Resolve Keychain **references** only |

## Enforcement

Declared permissions are validated on manifest load. At activate /
auto-activate, `impetus-core::extension_policy` evaluates each token against
the daemon `SandboxScope`:

- hard-`Deny` (e.g. `network` when `allow_network=false`) → package stays
  `Failed`, not `Active`
- `Allow` / `NeedsApproval` → package may become `Active` (activate-time
  `NeedsApproval` does **not** block Active today)
- host_process `operate` → manifest permission token + secret-key reject only
  (not a second full EffectSeam pass)

Agent-origin harness tools still go through:

`Policy → Deny | Allow | NeedsApproval` → Sandbox → Capability → Execution.

Extensions cannot:

- set `origin=user` for themselves
- approve their own requests
- read raw secrets into logs/SQLite
- require sudo/root/password in normal mode

Workspace location does **not** grant extra permissions.
