# Local development

## Discovery roots (user-writable)

| Scope | Path |
| --- | --- |
| Global | `$IMPETUS_DATA_DIR/extensions/packages/<id>/` |
| Workspace | `{workspace}/.impetus/extensions/<id>/` |
| Dev | `$IMPETUS_DATA_DIR/extensions/dev/<id>/` |

No root-owned directories. Workspace packages get **no** automatic privilege boost.

## Loop

1. Edit package under a discovery root.
2. Reload via IPC `ReloadExtensionPackages` (see [ipc.md](./ipc.md)).
3. Exercise session `Context` / AgentLoop; `ListExtensionPackages` for status.
4. `DisableExtensionPackage` to confirm skills disappear (persists across
   daemon restart via `extensions/disabled_packages.json`).

