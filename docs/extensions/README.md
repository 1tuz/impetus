# Extension architecture

Impetus core owns the **extension infrastructure**. Concrete first-party
extensions live in the separate `impetus-extensions` repository.

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

Do **not** mark this subsystem `Implemented` in `ARCHITECTURE.md` until:

1. `impetus-extension-sdk` is stable enough for an external crate to depend on
2. Daemon can discover, validate, load, enable/disable packages
3. AgentLoop consumes Active packs via host registry views (skills still need
   filesystem roots for `SKILL.md` content — not install_state path hacks)
4. Lifecycle + isolation tests + real `impetusd` E2E pass
5. crates.io publish / tagged pin story for external `impetus-extensions`

Until then: **Partial** (`instruction_pack` + `mcp_bridge` + `host_process`
handshake shipped; SDK not crates.io yet).

## Layers

```text
impetus-extension-sdk     # public types (manifest, permissions, API version)
impetus-core host         # discovery, validate, registry, AgentLoop inject
impetusd IPC              # list/get/enable/disable/reload/status/caps/compat
```

Legacy CLI Skill/MCP install (`impetus extension plan|install|…`) remains a
compatible adapter; new work authors **packages** with `extension.toml`.
