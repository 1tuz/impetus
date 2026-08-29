# Issue-Driven Commits for Impetus

## When to use
Use when preparing commits for the Impetus repository. This skill enforces English commit messages with mandatory issue references.

## Workflow

1. **Check for existing issue** related to your work
2. **If no issue exists**, create one first (brief title, clear acceptance criteria)
3. **Format commit message** following the rules below
4. **Verify** the commit references the issue

## Commit Message Format

```
type: brief summary (closes #N)

Optional body explaining what and why, not how.
Wrap at 72 characters.
```

### Rules

- **Language:** English only
- **Subject line:** <= 72 characters
- **Start lowercase** after `type:`
- **No period** at the end of subject
- **Mandatory issue reference:** `closes #N`, `fixes #N`, or `refs #N`
- **Types:** `feat`, `fix`, `docs`, `refactor`, `perf`, `test`, `build`, `ci`, `chore`, `revert`
- **Optional scope:** `type(scope): summary`

### Examples

```
feat: add subsystem health probes to doctor (closes #42)

Extended impetus doctor with IPC::Diagnostics endpoint that reports
Event Store, Artifact Store, Policy Engine, ProviderRegistry, Sandbox,
and Credential Store status. Both human-readable and JSON output.
```

```
fix(ipc): handle large enum variants with Box (refs #38)

Wrapped SubsystemHealth and ApprovalDetail in Box to satisfy clippy
large_enum_variant lint.
```

```
docs: update implementation history for phase 2
```

## Issue Reference Keywords

- `closes #N` or `fixes #N` — closes the issue when merged
- `refs #N` or `references #N` — mentions the issue without closing

## Before Committing

1. Run `task verify` (or `cargo fmt`, `cargo test`, `cargo clippy`)
2. Ensure issue number is correct
3. Check subject line length: `echo "your subject" | wc -c` (should be ≤ 72)

## What NOT to do

- ❌ Russian commit messages
- ❌ Commits without issue references for significant changes
- ❌ Subject lines starting with uppercase (except proper nouns)
- ❌ Period at the end of subject line
- ❌ Mixing unrelated changes in one commit

## Multi-commit Work

If your work spans multiple commits, you can:
- Reference the same issue in all commits: `(refs #42)`
- Close it only in the final commit: `(closes #42)`
- Or create separate issues for each logical piece
