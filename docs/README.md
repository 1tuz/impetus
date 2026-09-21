# Docs

Human map for Impetus documentation. Architecture truth stays in repo-root
[ARCHITECTURE.md](../ARCHITECTURE.md). Executable backlog:
[TODO.md](../TODO.md).

## Guides

| Doc | Topic |
| --- | --- |
| [getting-started.md](guides/getting-started.md) | Source checkout, first run, uninstall |
| [configuration.md](guides/configuration.md) | Provider profiles, Keychain labels |
| [development.md](guides/development.md) | `task verify`, PR CI, request-flow coverage |
| [troubleshooting.md](guides/troubleshooting.md) | Common failures |
| [ubuntu-smoke.md](guides/ubuntu-smoke.md) | Ubuntu 24.04 PR CI vs clean-machine proofs (#293) |

## Architecture

| Doc | Topic |
| --- | --- |
| [reader-guide.md](architecture/reader-guide.md) | Compact CURRENT/TARGET reader guide |
| [binary-topology.md](architecture/binary-topology.md) | Binary / crate layout |
| [kernel-invariants.md](architecture/kernel-invariants.md) | Kernel rules |
| [roadmap.md](architecture/roadmap.md) | Now / Next / Later narrative |
| [capability-leases-and-repomap.md](architecture/capability-leases-and-repomap.md) | Design-only notes (leases / RepoMap) |

## Reference

| Doc | Topic |
| --- | --- |
| [design-references.md](reference/design-references.md) | Protocols and design principles |
| [reference-store.md](reference/reference-store.md) | Reference pin / audit store |
| [components.md](reference/components.md) | Component / lockfile concepts |
| [browser-provider-protocol.md](reference/browser-provider-protocol.md) | Browser provider seam |
| [tui-ux-audit.md](reference/tui-ux-audit.md) | TUI UX constraints and audit |

## Benchmarks

| Doc | Topic |
| --- | --- |
| [event-log-v0.2.md](benchmarks/event-log-v0.2.md) | SqliteEventStore Criterion baselines (#16) |

## Archive

Historical audits and spikes — not current product truth.

| Doc | Topic |
| --- | --- |
| [todo-audit-2026-08-30.md](archive/todo-audit-2026-08-30.md) | Dated TODO audit |
| [vimtrap-implementation-plan.md](archive/vimtrap-implementation-plan.md) | VimTrap plan |
| [ratatui-spike.md](archive/ratatui-spike.md) | Ratatui GO evaluation (#137) |
| [benchmarks-v0.1-gpui-preview.md](archive/benchmarks-v0.1-gpui-preview.md) | Older idle-RSS / GPUI preview numbers |
