# Configuration

Daemon `impetusd` accepts one optional command-line argument:

```text
impetusd [--provider-profile PATH]
```

Without that argument, Impetus uses its mock streaming provider. The daemon
rejects unknown arguments and profiles with unknown fields.

## Data and socket paths

| Setting | Default | Purpose |
| --- | --- | --- |
| `IMPETUS_DATA_DIR` | `~/Library/Application Support/Impetus` | SQLite event store directory. |
| `IMPETUS_SOCKET` | `<data-dir>/harness.sock` | Unix-socket path used by daemon, `impetus` CLI, and Zap adapter. |

The daemon creates the Unix socket with mode `0600`. It refuses to replace an
existing socket path, so stop the old daemon before starting another one at the
same path.

## Direct-provider profile

`config/provider-profile.example.json` shows the schema:

```json
{
  "id": "local.mock",
  "endpoint": "http://127.0.0.1:11434",
  "model": "mock-model",
  "credential_strategy": { "kind": "none" }
}
```

Supported fields are `id`, `endpoint`, `model`, and `credential_strategy`.
The endpoint must be an absolute URL without a query string or fragment. The
provider requests `<endpoint>/v1/chat/completions` with streaming enabled.

### Credential strategies

| `kind` | Accepted endpoint | Required fields | Notes |
| --- | --- | --- | --- |
| `none` | Loopback `http` or `https` | none | For a local provider only. |
| `keychain_reference` | Non-empty HTTPS URL | `service`, `account` | The daemon reads the credential from the macOS Keychain only when making a request. |
| `system_browser_o_auth` | HTTPS URL | `authorization_url`, `token_url`, `client_id`, `keychain_service`, `keychain_account` | Profile validation exists; complete user-facing OAuth flow is not a documented setup path yet. |

Never add a token, private key, or passphrase to a profile, event, log, or test
fixture. The profile is an opaque locator, not a secret store.

## ACP profiles

`config/acp-profile.example.json` describes an external ACP agent executable.
Its `command` must be an absolute path. With `agent_owned` credentials, the
agent CLI owns its login and `credential_ref` must be absent. The ACP gateway is
a library and test surface; this repository does not provide a daemon command
that loads this example automatically.

`config/agent-backends.example.json` is a planning catalog, not a runtime
configuration file consumed by the daemon.
