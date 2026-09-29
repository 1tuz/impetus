# Extension package IPC

Capability: `extension_manage` (IPC version ≥ 14; install/remove ≥ **15**).

Clients never load packages themselves. Wire types live in `impetus-protocol`
(`ExtensionPackageInfo`, `IpcRequest` / `IpcResponse` variants below).

| Request | Response | Notes |
| --- | --- | --- |
| `ReloadExtensionPackages` | `ExtensionPackagesReloaded { loaded, failed }` | Discover under data dir + session workspace |
| `ListExtensionPackages` | `ExtensionPackages { packages }` | Inventory + phase |
| `GetExtensionPackage { id }` | `ExtensionPackage { package }` | One pack |
| `EnableExtensionPackage { id }` | `ExtensionPackage { package }` | Activate (instruction_pack / mcp_bridge / host_process) |
| `DisableExtensionPackage { id }` | `ExtensionPackage { package }` | Durable disable |
| `InstallExtensionPackage { source_path, replace }` | `ExtensionPackage { package }` | Copy validated dir → `$IMPETUS_DATA_DIR/extensions/packages/<id>/` + reload |
| `RemoveExtensionPackage { id }` | `ExtensionPackagesReloaded { … }` | Delete **global** pack only (workspace/dev refused) |
| `OperateExtensionPackage { … }` | `ExtensionOperate { … }` | Active `host_process` only |

`impetus extension …` CLI is **legacy** Skill/MCP install — not this surface
(package Install/Remove via CLI → Next). Catalog/marketplace = Won't.

Default `$IMPETUS_DATA_DIR` on macOS: `~/Library/Application Support/Impetus`
(see getting-started guide).
