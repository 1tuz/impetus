# impetus-daemon-control

Shared **local `impetusd` lifecycle** for Impetus hosts (CLI, Desktop, …).

```text
ensure_daemon_running(DaemonOptions { daemon_binary, socket_path, data_dir, … })
```

Not EventStore / Policy / AgentLoop / UI. See
[binary-topology.md](../../docs/architecture/binary-topology.md).

Desktop: pass bundled `impetusd` as `daemon_binary`; reuse this crate instead of
a private spawn copy.
