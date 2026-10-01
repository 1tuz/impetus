# Extension architecture

Impetus core owns the **extension infrastructure**. Concrete first-party
extensions live in the separate
[`impetus-extensions`](https://github.com/1tuz/impetus-extensions) repository
(catalog demos for `instruction_pack` / `mcp_bridge` / `host_process`;
`compatibility.json` pins `impetus-extension-sdk`).

See also:

- [EXTENSION_REPOSITORY_CONTRACT.md](../../EXTENSION_REPOSITORY_CONTRACT.md) —
  public contract for the external repo
- [ARCHITECTURE_DRAFT.md](./ARCHITECTURE_DRAFT.md) — design notes for #324
- [manifest.md](./manifest.md)
- [sdk.md](./sdk.md)
- [lifecycle.md](./lifecycle.md)
- [permissions.md](./permissions.md)
- [compatibility.md](./compatibility.md)
- [creating-an-extension.md](./creating-an-extension.md)
- [testing.md](./testing.md)
- [local-development.md](./local-development.md)
- [ipc.md](./ipc.md)
- [host-protocol.md](./host-protocol.md)
- [depending-on-sdk.md](./depending-on-sdk.md)

## Status honesty

Split status (see `ARCHITECTURE.md` matrix):

| Surface | Status | Gate |
| --- | --- | --- |
| Package lifecycle (discover/list/enable/disable/install/remove/operate) | **Implemented** | IPC `extension_manage` ≥ v14; install/remove ≥ v15 |
| Runtime MCP/skills in AgentLoop | **Partial** | Live path works; remaining = live crates.io upload (dry-run ready) |
| Marketplace / catalog browse | **Won't** | Manual/source install only |

Do **not** claim the full Extension subsystem Implemented until crates.io
crate page resolves / tagged pin for external `impetus-extensions` lands.

Sole public extension substrate = **Extension Host**
(`instruction_pack` / `mcp_bridge` / `host_process`). Legacy Module Runtime
library stack was **Removed** (`#443`).

## Layers

```text
impetus-extension-sdk     # public types (manifest, permissions, API version)
impetus-core host         # discovery, validate, registry, AgentLoop inject
impetusd IPC              # list/get/enable/disable/install/remove/reload/operate
```

Legacy CLI Skill/MCP install (`impetus extension plan|install|…`) remains a
compatible adapter; new work authors **packages** with `extension.toml` and
uses daemon Install/Remove IPC.
