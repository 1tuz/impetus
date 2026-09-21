# Browser Provider Protocol (reference)

> **Status:** audited reference for Impetus optional Browser track.
> Impetus is **not** a fork of JCode and does not vendor browser bridges.

**Upstream:** [1jehuang/jcode](https://github.com/1jehuang/jcode)
`docs/BROWSER_PROVIDER_PROTOCOL.md`

**Audited commit:** `2a4edaa02057ac994a601311c4f03ed450e1b3c9`
(master tip when issue #133 was implemented; re-audit before copying new shapes)

**Audited path:** `docs/BROWSER_PROVIDER_PROTOCOL.md`

**Impetus protocol id:** `0.1` (same semantic major as upstream draft; own Rust trait surface)

---

## Principle

```text
JCode Browser Provider Protocol  →  reference shapes (negotiate / health / session)
Impetus BrowserService            →  own in-process contracts + Module Runtime
Real Firefox/Chrome/WebDriver     →  optional later providers (not this slice)
```

Harness core must **not** require Chromium, Playwright, Node, or Electron.

---

## Decision legend

| Decision | Meaning |
| --- | --- |
| `ADAPT` | Keep semantic shape; implement as Impetus types/traits |
| `REIMPLEMENT` | Same intent, pure Impetus design (no wire copy) |
| `SKIP` | Out of scope for harness MVP or forbidden dependency |

---

## Protocol surface audit

| Upstream shape | Decision | Impetus mapping / reason |
| --- | --- | --- |
| Design goals (one tool, many providers, negotiation) | `ADAPT` | `BrowserService` / `BrowserProvider` + Module Runtime |
| `provider.describe` | `ADAPT` | `BrowserProviderDescriptor` |
| `provider.status` (`ready` / `degraded` / `unavailable`) | `ADAPT` | `BrowserServiceStatus` |
| Capability lists / features | `ADAPT` | `BrowserCapability` + negotiate intersection |
| `session.ensure` / `session.close` | `ADAPT` | `ensure_session` / `close_session` |
| `page.open` (navigate) | `ADAPT` | `navigate` (+ research `fetch_rendered`) |
| JSON-RPC / stdio / socket envelope | `REIMPLEMENT` | In-process async traits first; external transport later |
| `page.snapshot` / `click` / `type` / `wait` / `screenshot` | `SKIP` | Deferred to real provider slice |
| Tabs / eval / downloads / custom `*.` methods | `SKIP` | Not needed for research MVP |
| Firefox / Chrome / CDP / WebDriver bridges | `SKIP` | Separate TODO; optional modules only |
| Certification suite (full interactive) | `SKIP` | Mock negotiation/session tests only for CI |
| Chromium / Playwright / Node as required deps | `SKIP` | Forbidden in harness core |

---

## Impetus contract (this slice)

Normalized ops harness can rely on without a browser binary:

1. **Describe** — static provider metadata (`provider_id`, `protocol_version`, families, capabilities).
2. **Status / health** — `Unavailable` \| `Degraded` \| `Misconfigured` \| `Available`.
3. **Negotiate** — intersect requested capabilities with provider descriptor; reject incompatible protocol major.
4. **Session** — `ensure` / `close` opaque `session_id` (no browser process required for mock).
5. **Navigate** — session-scoped URL open result (`page_id`, url, optional title).
6. **Rendered fetch** — research path already on `BrowserService` (`fetch_rendered`).

Default production wiring: **no provider registered** → honest `Unavailable` (optional track).
CI: `MockBrowserProvider` proves negotiate → ensure → navigate → close without binaries.
Optional Firefox/Chrome slots: `RealBrowserProviderModule` + `OptionalBrowserService`
advertise family via negotiate/health; session/navigate fail-closed until a real
automation adapter lands (no compile-time binary path, no CDP crates in core).

See `crates/impetus-core/src/web_research/browser.rs` and `real_browser.rs`.

---

## Explicit non-goals

- Shipping Firefox/Chrome/Safari/WebDriver providers in this change.
- Mandating any browser runtime in `Cargo.toml` required deps.
- Copying JCode bridge CLI, extension, or native-host install flows.
- Changing SSRF / private-network grants.

---

## After this reference

1. Optional real providers as Module Runtime plugins (separate issues).
2. If upstream bumps protocol past `0.1`, re-pin SHA and refresh the table.
3. Interactive page ops only when a concrete provider lands.
