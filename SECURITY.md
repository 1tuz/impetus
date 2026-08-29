# Security policy

Impetus handles local execution authority, provider credentials, and durable
agent-session data. Please report suspected vulnerabilities privately.

## Reporting a vulnerability

Use the repository's private vulnerability-reporting channel on GitHub when it
is available. If it is not enabled, contact the repository maintainer through
their GitHub profile and request a private channel before sharing technical
details.

Do not include credentials, private keys, full exploit payloads, or local file
contents in a public issue, discussion, log, or screenshot.

Include the affected revision, a minimal reproduction, the expected and actual
behavior, impact, and any mitigation you have tested. The maintainer will
coordinate a fix and disclosure path privately.

## Scope

Reports are especially useful for flaws that could bypass policy or approvals,
weaken sandbox/capability enforcement, expose Keychain-backed secrets, reveal
durable session data, or permit unauthorized Unix-socket access.

This is early-stage software. No compatibility or response-time guarantee is
made here; the policy explains how to report issues safely.
