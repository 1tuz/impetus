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

`impetus extension install package <dir> [--replace]` and
`impetus extension remove <id> --package` use this surface when the daemon
sock is live and negotiates `extension_manage` (sock down → offline
`ExtensionHost` under `$IMPETUS_DATA_DIR`). Legacy Skill/MCP
`extension plan|install|remove|…` remains offline FS until migrated; after
those mutates CLI best-effort calls `ReloadExtensionPackages` when sock live.
Catalog/marketplace = Won't.

Default `$IMPETUS_DATA_DIR` on macOS: `~/Library/Application Support/Impetus`
(see getting-started guide).
