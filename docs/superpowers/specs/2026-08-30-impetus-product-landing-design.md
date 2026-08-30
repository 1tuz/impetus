# Impetus Product Landing Design

## Goal

Create an English, product-first landing page that explains Impetus in five
seconds, proves its local-first control model with real product material, and
provides a direct installation path. The page must feel like a premium
developer tool rather than a GitHub theme or generic SaaS template.

## Product truth

Impetus is a Rust-built, terminal-first agent harness. The authoritative
daemon owns durable sessions, ordered events, policy decisions, approvals,
sandboxed execution, credential references, and model/tool orchestration.
Clients use typed, versioned IPC and do not own authoritative state. The page
must not imply that Impetus is a terminal emulator or show capabilities that
are absent from the repository.

## Audience and job

The primary audience is an engineer evaluating local coding-agent
infrastructure. The page has one job: establish why a durable policy-controlled
runtime is preferable to stitching state, tools, credentials, and clients
together, then make installation obvious.

## Design direction: Control Ledger

The visual language is controlled, durable, and technical. A thin event spine
is the signature element: it carries a request through policy, approval,
sandbox, execution, and the durable event log. This system-specific device
replaces decorative gradients, glowing blobs, and generic feature iconography.

The hero is asymmetric. A five-column copy block sits beside a seven-column
terminal/event transcript. The page is left-aligned, uses large negative space,
and spends visual emphasis on the transcript and control flow rather than on a
centered marketing headline.

## Copy direction

- Headline: **Give coding agents a runtime you can trust.**
- Supporting copy: **Durable sessions, policy-gated tools, and local execution
  in one Rust harness—independent of any client or model.**
- Primary action: **Install Impetus**
- Secondary action: **View on GitHub**
- Product language uses concrete nouns: event log, policy decision, approval,
  sandbox, Keychain reference, typed IPC, and durable session.
- All page copy, labels, metadata, accessibility text, and generated artifacts
  are English.

## Visual system

### Color

One electric-cyan accent identifies interactive controls and the active request
path. It occupies less than 20 percent of the page surface.

| Token | Value | Use |
| --- | --- | --- |
| `color.bg.canvas` | `#070A0E` | Page background |
| `color.bg.surface` | `#0D131A` | Terminal and card surfaces |
| `color.bg.elevated` | `#121B24` | Elevated controls |
| `color.border.default` | `#22303D` | Dividers and inactive paths |
| `color.text.primary` | `#F2F6F8` | Headlines and primary text |
| `color.text.secondary` | `#8B9AAA` | Supporting text |
| `color.accent.primary` | `#51D6FF` | CTA, focus, active request path |

Red, amber, and green may appear only when required as semantic terminal
status colors; they are not brand accents.

### Typography

- Display and body: Geist Sans variable.
- Commands, labels, and event metadata: Geist Mono variable.
- Desktop hero: 72 px, 0.96 line height, -0.045 em tracking.
- Mobile hero: 44 px, 1.0 line height, -0.035 em tracking.
- Body: 17 px desktop and 16 px mobile, 1.6 line height.
- Utility labels: 11–13 px monospace with restrained uppercase tracking.

### Spacing and shape

- Base rhythm: 8 px, with 4 px allowed for micro-spacing.
- Desktop content width: 1200 px inside a 1440 px frame.
- Desktop section spacing: 112 px.
- Mobile content padding: 20 px inside a 390 px frame.
- Mobile section spacing: 72 px.
- Surface radii: 6 px and 10 px only. Buttons are not pills.
- Cards use a border or a tonal shift, never border plus shadow.

## Penpot deliverables

The Penpot file contains these semantically named pages:

1. `00 Design System`
2. `01 Landing Desktop 1440`
3. `02 Landing Mobile 390`

The design system contains primitive and semantic color tokens, spacing,
radius, and typography tokens; library colors and typography styles; and these
reusable components:

- `Navigation/Desktop`, `Navigation/Mobile`
- `Button/Primary`, `Button/Secondary`, `Button/Ghost`
- `Terminal/Window`
- `Terminal/Event Row`
- `Flow/Node`
- `Card/Capability`
- `Command/Copy Block`
- `Section/Header`

Every container uses Flex or Grid layout. Every layer name describes its role.
Desktop and mobile screens are assembled from component instances.

## Page structure

### Navigation

Wordmark at left; Architecture, Docs, and GitHub links; one Install action.
Mobile keeps the wordmark, GitHub shortcut, and a compact menu control.

### Hero

The copy occupies five grid columns. The terminal transcript occupies seven.
The transcript uses real command names from the current CLI: `doctor`,
`create`, `prompt`, `stream`, and `approve`. It visualizes a pending action and
its durable outcome without inventing a graphical client.

### Durable control

An asymmetric bento contains four capabilities, not a repetitive card wall:

- Durable sessions and ordered SQLite WAL events.
- Policy and typed human approval before sensitive execution.
- Keychain references instead of raw secrets in state or logs.
- Replaceable CLI, TUI, ACP, and Zap-facing client boundaries through typed IPC.

The durable-session block spans two columns and carries the event spine.

### How a request stays controlled

A compact flow shows:

`Client → Harness → Policy decision → Sandbox + capability → Execution`

Dashed support connections lead to the durable event log and Keychain
references. On mobile this becomes a vertical sequence; labels remain readable
without horizontal scrolling.

### Real CLI showcase

Use current repository command output and repository-owned architecture assets.
The section explains that clients can reconnect and replay durable events. It
does not show an imaginary polished TUI.

### Install

One copyable command block contains the documented installer command. A short
platform note names macOS Apple Silicon and Linux x86_64 as documented support.

### Final action and footer

The final action repeats Install and GitHub, followed by a minimal footer with
Architecture, Documentation, Apache-2.0, and repository links.

## Responsive behavior

At 390 px, navigation actions collapse, the hero becomes a vertical stack, and
the transcript moves directly below the CTA. Bento cards become a prioritized
single column. The control flow becomes vertical. Command blocks wrap labels
but preserve horizontally scrollable command text. Tap targets are at least
44 px and no section relies on hover.

## Motion and interaction

- One orchestrated page-load reveal for the hero transcript.
- A low-frequency cursor blink inside the terminal.
- Small border/color transitions for hover and keyboard focus.
- Copy command produces an English status announcement.
- `prefers-reduced-motion: reduce` disables reveals, cursor blinking, and smooth
  scrolling.

## Implementation

Use Astro with TypeScript and static output. Do not add React, Tailwind, a UI
framework, or a general animation library. Client JavaScript is limited to the
mobile navigation and copy-command behavior. Fonts are self-hosted. Assets use
repository-safe paths derived from Astro's base path.

The GitHub Pages workflow builds from `site/`, uploads `site/dist`, and deploys
with the official Pages actions. The Astro base path resolves to `/impetus` in
GitHub Actions and `/` for local development.

## Verification

- Penpot exports for desktop, mobile, and each major section.
- Penpot critique score of at least 10/12, followed by one correction pass.
- Static tests verify English metadata, critical product copy, internal anchor
  targets, base-aware assets, reduced-motion CSS, and absence of React.
- Astro type/build checks pass.
- Browser checks cover desktop 1440 px and mobile 390 px, overflow, navigation,
  copy behavior, hover, focus, reduced motion, and broken assets.
- Lighthouse performance target is 95–100 where the local environment permits
  measurement.

## Non-goals

- Product pricing, testimonials, newsletter capture, or analytics.
- A browser runtime, client-side framework, or runtime content API.
- A fictional TUI, terminal emulator, model benchmark, or unsupported product
  claim.
- Changes to Rust runtime behavior.
