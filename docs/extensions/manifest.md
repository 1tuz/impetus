# Manifest reference

Package file: `extension.toml` (preferred) or `extension.json`.

Schema id: `impetus.extension_package.v1` (crate `impetus-extension-sdk`).

## Fields

| Field | Required | Notes |
| --- | --- | --- |
| `schema_version` | no (default 1) | Package manifest schema |
| `id` | yes | `[a-z0-9][a-z0-9_-]{0,63}` |
| `name` | yes | Human-readable |
| `version` | yes | Must parse as semver |
| `description` | yes | May be empty string |
| `author` | yes | May be empty string |
| `extension_api_version` | yes | Extension API major (not Impetus app version) |
| `entrypoint` | yes | Typed closed set |
| `capabilities` | yes | ≥1 closed-set tokens |
| `permissions` | no | Explicit; default deny |
| `configuration` | no | JSON Schema + defaults + scope |
| `dependencies` | no | Other extension ids (validated; host resolve Remaining) |

## Entrypoint

```toml
[entrypoint]
kind = "instruction_pack"  # production path today
root = "skills"            # relative, no `..` / absolute

# kind = "mcp_bridge"
# module_id = "echo"       # must already exist under $IMPETUS_DATA_DIR/mcp/

# kind = "host_process"    # see host-protocol.md
# command = "./ext.sh"
# args = []
```

## Example

```toml
schema_version = 1
id = "demo-pack"
name = "Demo Pack"
version = "0.1.0"
description = "Reference fixture"
author = "impetus"
extension_api_version = 1
capabilities = ["skill_provider"]
permissions = ["filesystem_read"]

[entrypoint]
kind = "instruction_pack"
root = "skills"
```

Legacy install manifests (`impetus.extension.v1` Skill/MCP digest) are a
separate CLI contract and are not the authoring surface for
`impetus-extensions`.
