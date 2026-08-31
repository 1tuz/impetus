# macOS process sandbox

Agent-origin `bash`, `shell`, and `exec` calls pass through policy and exact
human approval before `ProcessExecutionRequest` invokes the production
`SandboxProvider`. On macOS the provider uses `/usr/bin/sandbox-exec`; on any
platform without that backend, process execution fails closed before spawn.

The generated Seatbelt profile starts with `deny default`. It permits process
execution, reads from the canonical workspace and required read-only system
paths, and writes only to the canonical workspace plus a private `0700`
per-execution temporary directory. Network access is absent unless the sandbox
request carries an explicit network allowance. Child processes remain in the
same Seatbelt boundary.

The provider canonicalizes the workspace and working directory before building
the profile. A working directory outside the admitted workspace is rejected.
Sensitive home roots such as `.ssh`, credential configuration, and the macOS
`Library` tree receive explicit read/write denies even when a workspace scope
would otherwise contain them. Non-UTF-8 or control-character profile paths
also fail closed.

The child environment is cleared. Only a fixed system `PATH`, an isolated
`HOME`/`TMPDIR`, locale/terminal defaults, and a small explicit non-secret
allowlist can be set. Standard output and error are drained with a two MiB cap.
Each child starts a new process group; timeout and cancellation terminate that
group so descendants cannot survive the command.

Before spawn, the harness persists a secret-free sandbox decision containing
only the backend, prepared/denied state, network flag, writable-root count, and
a stable failure code. Profiles, paths, commands, environment values, and
secrets are not included in that event.

Platform integration coverage is in
`crates/impetus-core/tests/macos_sandbox_production.rs`. It exercises allowed
workspace writes, outside and symlink escape denial, sensitive-home reads,
network denial, child inheritance, unavailable-backend fail-closed behavior,
and process-tree cleanup on timeout and cancellation.

This boundary covers agent-controlled local shell/process execution. Daemon
startup, user-selected ACP clients, external modules, and daemon-native HTTP
services have separate lifecycle and permission contracts; they are not routed
through the shell tool.
