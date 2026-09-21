# TUI Reference Audit

> **Status: audited.** Decisions below are locked against pinned JCode commit.
> Re-audit only when intentionally bumping the pin.

Impetus is not a fork of JCode and does not import its application/runtime layer.
JCode is a **UX / presentation reference** only.

**Principle:**

```text
JCode   → UX patterns (this audit)
Impetus → own thin TUI client (Ratatui + Crossterm)
          talks to harness only via HarnessClient
```

**Codex** — secondary reference for composer, large paste, doctor/diagnostics,
approval UX, errors/remediation.

**Baseline stack:** Ratatui + Crossterm — **GO** (evaluation: [RATATUI_SPIKE.md](RATATUI_SPIKE.md), #137).

---

## Impetus boundary (hard)

| Layer | Owns | TUI may |
| --- | --- | --- |
| `impetusd` / `impetus-core` | SQLite, secrets, policy, sandbox, tools, providers | never import |
| `HarnessClient` + typed IPC | sessions, stream, approvals, artifacts, status | only API surface |
| Standalone `impetus` TUI | Ratatui widgets, local scrollback buffer, key map | display durable events / request typed actions |

- Every typed action keeps `origin=user|agent` and goes through harness policy.
- TUI never grants itself `origin=user` approval or bypasses policy.
- Large paste: chunked upload → `ArtifactStore` → `ArtifactRef` on the wire;
  raw body never enters durable events or logs.
- Do not vendor `jcode-tui` / `jcode-app-core` crates or copy their modules.
- **Regression guard (#142):** `crates/impetus-tui` must list `impetus-client`
  only (no direct `impetus-core` in `Cargo.toml`). Tests in
  `impetus_tui::boundary` fail if a direct core dep or `use impetus_core`
  appears. Transitive core via `impetus-client` wire types is allowed until
  further client/core decomposition.

---

## Source audit checklist

- [x] Inspected current `https://github.com/1jehuang/jcode` tree via API/raw
- [x] Locked audited **commit SHA** in this file
- [x] Listed presentation-layer **crates/paths**
- [x] Per-component `ADAPT | REIMPLEMENT | SKIP` with code-based rationale
- [x] Marked gaps taken from Codex / terminal spec / Impetus architecture

**Upstream:** [1jehuang/jcode](https://github.com/1jehuang/jcode)

**Audited commit:** `2a4edaa02057ac994a601311c4f03ed450e1b3c9`
(date: 2026-09-21; message: `docs: update weekly stars chart`)

**Pin note:** Prefer this exact SHA for UX comparisons. Bump only with a new
audit pass and an intentional docs change.

---

## Decision legend

| Decision | Meaning |
| --- | --- |
| `ADAPT` | Transfer presentation idea/pattern; own Ratatui implementation |
| `REIMPLEMENT` | Same UX contract; pure Impetus code; no JCode module copy |
| `SKIP` | Out of standalone TUI scope, wrong trust boundary, or Zap/other client |

---

## Presentation inventory (audited paths)

Primary presentation crates under the pinned commit:

| Crate / path | Role |
| --- | --- |
| `crates/jcode-tui/` | Main TUI app; **depends on `jcode-app-core`** (not a thin client) |
| `crates/jcode-tui/src/tui/ui_input.rs` | Composer render (`ComposerMode`: Chat / Slash / Shell) |
| `crates/jcode-tui/src/tui/app/input.rs` | Input loop, clipboard paste, submit path |
| `crates/jcode-tui/src/tui/app/input/newline.rs` | Multiline: Shift/Alt+Enter, trailing `\` continuation |
| `crates/jcode-tui/src/tui/app/input/paste_guard.rs` | Bracketed-paste trailing-Enter suppress (#544) |
| `crates/jcode-tui-core/src/stream_buffer.rs` | Paced stream reveal (arrival ≠ paint) |
| `crates/jcode-tui/src/tui/stream_buffer.rs` | Re-export of core stream buffer |
| `crates/jcode-tui-markdown/` | Full markdown (syntect, optional mermaid/LaTeX) |
| `crates/jcode-render-core/` | Shared markdown/math preprocess kernels |
| `crates/jcode-tui/src/tui/ui_diff.rs` | Unified-diff line parse + add/del colors |
| `crates/jcode-tui/src/tui/ui_file_diff.rs` | File-oriented diff presentation |
| `crates/jcode-tui-permissions/` | Standalone permissions Ratatui app |
| `crates/jcode-tui-session-picker/` + `…/session_picker.rs` | Session list + preview |
| `crates/jcode-tui/src/tui/fuzzy.rs` + `crates/jcode-fuzzy/` | Slash-command fuzzy match |
| `crates/jcode-tui/src/tui/app/commands*.rs` | Slash-command dispatch (not a separate palette crate) |
| `crates/jcode-tui/src/tui/ui_overlays.rs` | Modal overlays (changelog, etc.) |
| `crates/jcode-tui/src/tui/ui_status.rs` | Status / model / chrome helpers |
| `crates/jcode-tui-usage-overlay/` | Usage health overlay widgets |
| `crates/jcode-tui/src/tui/ui_viewport.rs` | Viewport chrome / badges |
| `crates/jcode-tui/src/tui/app/handterm_native_scroll.rs` | Handterm-native scroll socket (host-specific) |
| `crates/jcode-tui-messages/` | Prepared transcript / wrap cache |
| `crates/jcode-tui-style/src/palette.rs` | Color palette literals (not command palette) |
| `crates/jcode-tui-mermaid/`, `jcode-tui-anim/`, `jcode-tui-account-picker/`, … | Extra presentation (mostly SKIP for Impetus MVP) |

Stack confirmed in `crates/jcode-tui/Cargo.toml`: **Ratatui 0.30**, **Crossterm 0.29**
(`event-stream`), plus tight coupling to app-core — **do not mirror that coupling**.

---

## Component matrix (locked)

| Component | Reference | Decision | Reason |
| --- | --- | --- | --- |
| Composer (single-line) | JCode `ui_input` / `ComposerMode` | ADAPT | Mode chrome (chat vs `/` slash) is good UX; rebuild on Ratatui without app-core |
| Composer (multiline) | JCode `input/newline.rs` | ADAPT | Shift/Alt+Enter + trailing `\` fallback; Impetus owns keymap |
| Bracketed paste | JCode `paste_guard.rs` + terminal spec | ADAPT | Trailing-Enter suppress after paste; Impetus already ships bracketed paste (Phase 6) |
| Large paste detection | Codex + Impetus | REIMPLEMENT | Compact `[Pasted text · N KB · M lines]` label — Impetus path already in harness, not JCode |
| Large paste upload | Impetus arch | REIMPLEMENT | Chunked → `ArtifactStore` → `ArtifactRef` via `HarnessClient`; JCode clipboard bus ≠ durable artifact model |
| Keyboard handling | JCode `keybind` + Crossterm | ADAPT | Patterns only; Impetus key map stays local to TUI |
| Streaming output | JCode `jcode-tui-core` `StreamBuffer` | ADAPT | Paced reveal (arrival ≠ paint) is the keep idea; thin Impetus buffer over harness stream events |
| Markdown render | JCode `jcode-tui-markdown` | REIMPLEMENT | Keep **bounded** markdown; SKIP mermaid/LaTeX/Handterm APC and heavy syntect stack for MVP |
| Diff view | JCode `ui_diff` / `ui_file_diff` | ADAPT | Add/del coloring + unified-diff parse UX; data from typed harness diffs |
| Approval UI | JCode `jcode-tui-permissions` | REIMPLEMENT | JCode records via `jcode_base::safety` files — wrong authority; Impetus uses typed harness approvals only. Layout (list / approve / deny) may visually echo JCode |
| Session picker | JCode `session_picker` | ADAPT | List + preview UX; load/filter via `HarnessClient` sessions API — no JCode session store |
| Fuzzy search | JCode `fuzzy` / `jcode-fuzzy` | ADAPT | Typo-tolerant slash/session filter idea; own small matcher or std crate — no vendor |
| Command palette | JCode slash composer + overlays | ADAPT | JCode has no separate palette crate — slash mode + overlays; Impetus may add thin palette over same idea |
| Scrolling / scrollback | JCode viewport + messages cache | REIMPLEMENT | Client scrollback over durable events; do not treat TUI buffer as source of truth |
| Handterm native scroll | `handterm_native_scroll.rs` | SKIP | Host-specific Unix socket / APC — not Impetus product |
| Terminal resize | Crossterm / Ratatui | ADAPT | Standard layout on resize |
| Status / usage UI | JCode `ui_status` + `usage-overlay` | ADAPT | Badge/overlay presentation; values from typed harness status/budget APIs |
| Redraw / event coalescing | JCode stream pacing + frame metrics | ADAPT | Coalesce paints; keep harness events durable and complete |
| Color style palette | `jcode-tui-style` | SKIP / later ADAPT | Optional theming later; not Phase 7 MVP blocker |
| Mermaid / LaTeX / anim | `jcode-tui-mermaid`, anim, Handterm | SKIP | Out of harness TUI MVP; Zap or later optional |
| Account / OAuth pickers | `jcode-tui-account-picker` | SKIP | Auth via Keychain + harness profiles, not JCode login UI |
| Error + remediation | Codex + `impetus doctor` | REIMPLEMENT | Align with doctor/remediation copy; JCode not primary |
| Agent Runtime | — | SKIP | `impetusd` only |
| Provider / session / tool authority | — | SKIP | Harness owns authority |
| PTY / terminal emulator | Zap | SKIP | Not a terminal emulator |

---

## Gaps (absent or weak in JCode for Impetus)

Taken from Codex / terminal spec / Impetus architecture, not JCode:

1. **Durable `ArtifactRef` large-paste upload** — Impetus harness contract (already landed); JCode uses clipboard/bus paste into app state.
2. **Typed approval IPC** (`NeedsApproval` → user accept → continue) — harness policy path; JCode permissions TUI writes local safety files.
3. **`HarnessClient`-only client** — JCode TUI re-exports `jcode-app-core`; Impetus forbids that shape.
4. **Doctor / remediation UX** — Codex-style error + fix hints tied to `impetus doctor`.
5. **Bracketed-paste matrix** (iTerm, Terminal.app, SSH) — terminal-spec testing, not JCode-owned.

---

## Explicit non-goals (TUI)

- Fork or vendor JCode application / TUI crate code.
- Direct `impetus-core` / SQLite / Keychain imports from the TUI process.
- Custom ANSI terminal emulator, tabs, or scrollback-as-product-core.
- Loading external agent CLIs from TUI bypassing harness policy.
- Mandatory mermaid, LaTeX, Handterm, swarm gallery, or JCode account OAuth UI.

---

## After this audit (implementation follow-ups)

1. Ratatui spike: composer + paced stream + one approval mock over `HarnessClient`.
2. Bracketed paste test matrix (iTerm, Terminal.app, SSH) — keep paste_guard idea.
3. Wire large paste UX to existing chunked `ArtifactStore` upload (already in harness).
4. Open follow-up issues per unchecked Phase 7 rows in `TODO.md` (no feature dump in this PR).
