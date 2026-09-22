# Extension package IPC

Capability: `extension_manage` (IPC version ≥ 14).

Clients never load packages themselves. Wire types live in `impetus-protocol`
(`ExtensionPackageInfo`, `IpcRequest` / `IpcResponse` variants below).

| Request | Response | Notes |
| --- | --- | --- |
| `ReloadExtensionPackages` | `ExtensionPackagesReloaded { loaded, failed }` | Discover under data dir + session workspace |
| `ListExtensionPackages` | `ExtensionPackages { packages }` | Inventory + phase |
| `GetExtensionPackage { id }` | `ExtensionPackage { package }` | One pack |
| `EnableExtensionPackage { id }` | `ExtensionPackage { package }` | Activate (instruction_pack / mcp_bridge) |
| `DisableExtensionPackage { id }` | `ExtensionPackage { package }` | Durable disable |

`impetus extension …` CLI is **legacy** Skill/MCP install — not this surface.

Default `$IMPETUS_DATA_DIR` on macOS: `~/Library/Application Support/Impetus`
(see getting-started guide).
