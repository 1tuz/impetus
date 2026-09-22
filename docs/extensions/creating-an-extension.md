# Creating an extension

1. Depend on `impetus-extension-sdk` via git `rev` or path
   ([depending-on-sdk.md](./depending-on-sdk.md); not crates.io yet).
2. Create a package directory with `extension.toml` (see [manifest.md](./manifest.md)).
3. For `instruction_pack`: add `skills/SKILL.md` with YAML frontmatter:
   `id`, optional `scope: global|workspace`, optional `path:` / `ecosystem:`.
4. Declare `capabilities` / `permissions` (activate-time Policy/SandboxScope
   gate via `extension_policy`).
5. Place under a discovery path ([local-development.md](./local-development.md)).
6. Reload via daemon IPC `ReloadExtensionPackages` (see [ipc.md](./ipc.md) —
   not `impetus extension` CLI; that path is legacy Skill/MCP install).
7. Confirm `ListExtensionPackages` + session `Context` shows the skill id.
8. `DisableExtensionPackage` removes the skill from Context (durable across
   daemon restart).

Do not import `impetus-core` private modules from an extension package.
