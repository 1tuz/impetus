# Troubleshooting

## `connect harness socket` / `Failed to connect to impetusd` fails

Ordinary `impetus` commands **lazy-start** `impetusd`. If connect still fails:

1. Confirm `impetusd` is on `PATH` (same install dir as `impetus`).
2. Check `IMPETUS_SOCKET` / `IMPETUS_DATA_DIR` match between client and daemon.
3. For debugging, start the daemon in the foreground:

```zsh
task daemon
# or after installation:
impetusd
```

Defaults:

- macOS socket: `~/Library/Application Support/Impetus/harness.sock`
- Linux: `$XDG_DATA_HOME/impetus/harness.sock` or `~/.local/share/impetus/harness.sock`

## The daemon refuses to replace a socket

Another `impetusd` may still be running, or a previous run left a socket at the
configured path. Stop the owning process before starting a new daemon. The CLI
removes a **stale** socket (path exists, nothing accepts) before spawn — do not
delete a live socket while a daemon is healthy.

## A provider profile is rejected

Check these constraints:

- `id` and `model` are non-empty.
- `endpoint` is an absolute URL without a query string or fragment.
- A `none` credential strategy points only to `localhost`, `127.0.0.1`, or
  `::1`.
- `keychain_reference` profiles use HTTPS and non-empty `service` and
  `account` fields.
- The file contains no extra fields and no raw credential.

Compare with `config/provider-profile.example.json` and [configuration](configuration.md).

## A Keychain-backed provider cannot authenticate

`impetusd` resolves `service` and `account` only when sending a provider
request. Confirm the generic-password entry exists and the process may read it.
Errors are redacted; do not paste credentials into issues or logs.

### macOS Keychain prompts after rebuild

`impetusd --provider-profile` with `keychain_reference` may trigger a macOS
authorization dialog on the **first provider request** (lazy lookup — not at
daemon start). Service/account come from the profile JSON (labels only).

**Root cause (not Full Disk Access):** Keychain ACL / partition list binds to
the calling binary’s code-signing identity (`cdhash` / designated requirement).
Unsigned or ad-hoc `cargo build` binaries get a **new identity every rebuild**,
so “Always Allow” does not stick across rebuilds. Same path after `cargo install`
or replacing the binary also re-prompts.

| Choice | Effect |
| --- | --- |
| **Allow** / **Allow Once** | Works until next rebuild or path change. |
| **Always Allow** | Persists only while binary signing identity stays the same. |

**Safe operator path**

1. Confirm item: Keychain Access → search service label from profile (often
   `impetus`), or `security find-generic-password -s '<service>' -a '<account>'`
   (never print `-w` into logs/issues).
2. Inspect binary identity: `codesign -dv --verbose=4 "$(command -v impetusd)"`
   (or `target/debug/impetusd`). Unsigned / changing cdhash → expect re-prompt.
3. Prefer a **stable signing identity** for daily local daemons (Apple Development
   or a dedicated codesign identity you reuse). Sign after build:
   `codesign -f -s '<identity>' target/debug/impetusd`, then click Always Allow
   once. Do **not** use permissive ACL (`security … -A` / allow-all-apps).
4. Live Keychain tests: interactive only. CI / scripts:
   `CI=true` or `IMPETUS_NONINTERACTIVE=1` → fail closed, no GUI.
   `IMPETUS_CREDENTIAL_BACKEND=mock` → never touches Keychain.

Do **not** grant Full Disk Access to work around Keychain — it is not required
and widens trust incorrectly. Impetus does not ship `-A` ACL scripts; credentials
stay reference-only via documented `service` / `account` fields.

### CI and non-interactive runs

When `CI=true` or `IMPETUS_NONINTERACTIVE=1` (truthy: `1`, `true`, `yes`,
`on`), `impetusd` does **not** call Keychain. Provider requests fail closed
with `MissingCredential` instead of opening a GUI or hanging.

Default daemon (no `--provider-profile`) and tests use `NoCredentialResolver`.
For local live Keychain tests, run interactively without those env vars. Opt-in
backends: `IMPETUS_CREDENTIAL_BACKEND=mock` (never reads Keychain) or
`keychain` (reads only when interactive). See [configuration](configuration.md).

## A planned interface returns `Unavailable`

The IPC protocol advertises attachment and approval-detail requests, but backing
work is still on the roadmap. Do not treat these as a complete public API yet.

## Diagnostics (planned)

`impetus doctor` and `impetus doctor --json` will report versions, socket, IPC
compatibility, store health, providers, modules, and remediation hints. Not
implemented yet — see [TODO.md](../../TODO.md).

## CI behaves differently from `task verify`

GitLab CI runs a narrower unit-test scope for Linux Docker execution. Local
`task verify` runs `cargo test --workspace`, including macOS integration tests.
See [development](development.md).
