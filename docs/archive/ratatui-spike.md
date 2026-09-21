# Ratatui + Crossterm spike / evaluation

> **Decision: GO.** Keep Ratatui + Crossterm as the standalone `impetus` TUI
> stack. This is an evaluation note, not a product-shell rewrite.
>
> Date: 2026-09-21 · Issue: #137 · Crate: `crates/impetus-tui`

JCode UX audit (#135, merged) is separate. This spike does **not** restate
ADAPT | REIMPLEMENT | SKIP rows; see [tui-ux-audit.md](../reference/tui-ux-audit.md).

---

## Verdict

| Question | Answer |
| --- | --- |
| Keep Ratatui + Crossterm? | **GO** — already shipping in `impetus-tui` |
| Switch to alternate TUI kit? | **NO-GO** — no unmet requirement |
| Custom PTY / ANSI emulator in harness? | **NO-GO** — client concern; Zap owns terminal host UX |
| Direct `impetus-core` from TUI? | **NO-GO** — `HarnessClient` / `UiBackend` only |

---

## Dependency pins (no `latest`)

Pinned in `crates/impetus-tui/Cargo.toml` and resolved in `Cargo.lock`:

| Crate | Pin | Lock checksum (abbrev) |
| --- | --- | --- |
| `ratatui` | `0.30.2` | `3274ba0a…` |
| `crossterm` | `0.29.0` (`event-stream`) | `d8b9f2e4…` |

Semver caret on exact patch versions already used in-tree; do not float to
`latest` or unpinned git deps. Bump only with intentional Cargo.toml + lock
update and `task verify`.

---

## Architecture boundary

```text
impetus-tui ── UiBackend ── ImpetusBackend ── HarnessClient ── IPC ── impetusd
                │
                └── MockBackend (demo / tests)
```

Hard rules restated:

- TUI is presentation-only: widgets, local scrollback buffer, keymap, paste UX.
- Durable state, policy, secrets, sandbox, tools, providers stay in `impetusd`.
- Production path uses `impetus_client::HarnessClient` via `backend::impetus`.
- `Cargo.toml` for `impetus-tui` depends on `impetus-client`, **not**
  `impetus-core`.
- Enforced by `impetus_tui::boundary` unit tests (#142) — direct core dep or
  `impetus_core` source import fails CI/`task verify`.
- Typed actions keep `origin=user|agent`; TUI cannot self-approve or bypass
  policy.

---

## Capability check (spike evidence)

| Need | Feasible? | Evidence in tree |
| --- | --- | --- |
| Minimal screen + event loop | Yes | `app::run` + `TerminalSession` + Ratatui `Frame` |
| Composer stub | Yes | `composer::Composer` + render path |
| Bracketed paste | Yes | `EnableBracketedPaste` / `DisableBracketedPaste` in `terminal.rs`; `TerminalEvent::Paste` in `app.rs` |
| Resize | Yes | `TerminalEvent::Resize` handled (triggers redraw) |
| Async event stream | Yes | Crossterm `EventStream` + Tokio select loop |

Headless smoke (no real TTY): `spike` module tests render a composer stub with
`ratatui::backend::TestBackend` and assert Paste / Resize event discriminants
exist on the Crossterm API surface.

---

## Alternatives considered

| Alternative | Why not now |
| --- | --- |
| `cursive` / `tui-rs` (legacy) | Superseded; Ratatui is the maintained fork line |
| Pure Crossterm draw (no Ratatui) | Reimplements layout/widgets we already use |
| iced / egui / native GUI | Out of Phase 7 scope; harness stays headless |
| Embed Zap renderer | Zap is separate client; do not copy into harness core |

---

## Follow-ups (out of this spike)

Product Phase 7 items remain open in `TODO.md` (shell polish, markdown/diff,
approvals UI, session picker, etc.). Browser providers and Zap protocol are
out of scope here.
