# Lifecycle

```text
discover → validate manifest → compatibility check → permission evaluation
  → load → initialize → activate → operate
  → configuration reload → deactivate → unload
```

## Failure modes

| Failure | Host behavior |
| --- | --- |
| Invalid manifest | Reject package; continue others |
| API mismatch | `CompatError`; reject package |
| Permission denied | Do not activate; record error |
| Init/activate failure | Mark Failed; do not register capabilities |
| Crash (host_process) | Child kill/isolate; daemon stays up; package → Failed/Disabled |
| Removed while running | Next reload drops from registry |
| Daemon restart | Rediscover from disk; durable disable restored from
  `$IMPETUS_DATA_DIR/extensions/disabled_packages.json` |

Declarative `instruction_pack` / `mcp_bridge` do not run author code in-process.
`host_process` runs crash-isolated children (see [host-protocol.md](./host-protocol.md)).
