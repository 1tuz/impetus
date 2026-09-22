# Host process protocol

Entrypoint `kind = "host_process"` runs an out-of-process child. The host speaks
newline-delimited JSON-RPC 2.0 on the child's stdin/stdout
(`impetus_extension_sdk::host_protocol`).

## Security

- No shell: `Command::new` + args only
- Command is a PATH binary name **or** package-relative path (no absolute, no `..`)
- Basename deny-list: `sudo` / `doas` / `su` / `pkexec` / …
- Manifest must declare `process_spawn`
- Activate still runs `extension_policy` against SandboxScope
- Child crash does not take down the daemon; reload/deactivate kills children

## Handshake

1. Host spawns `command` + `args` with `cwd` = package directory
2. Host → `extension/initialize` (`protocol_version`, `extension_id`,
   `extension_api_version`)
3. Child → result `{ "protocol_version": 1, "name"?: "…" }`
4. On disable/reload: host → `extension/shutdown`, then kill

`HOST_PROTOCOL_VERSION` is currently `1`. Mismatch → Failed phase.

## Author fixture

See SDK `host_protocol` module + core unit test `host_process_activates_with_initialize_handshake`.
