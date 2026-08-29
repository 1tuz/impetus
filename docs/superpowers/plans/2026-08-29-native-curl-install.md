# Native curl installation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Publish verified native Impetus archives for Ubuntu x86_64 and macOS
Apple Silicon/Intel, and install the matching release through one curl command.

**Architecture:** A POSIX-shell installer selects a target from `uname`, verifies
the target-specific GitHub release archive, then atomically installs both
binaries. GitHub Actions checks native release builds on PRs and builds all
three archives before publishing a version tag release.

**Tech Stack:** Rust 1.98, POSIX `sh`, GitHub Actions, GitHub CLI, `tar`,
`sha256sum` / `shasum`.

**Spec:** `docs/superpowers/specs/2026-08-29-linux-curl-install-design.md`

## Global Constraints

- Supported targets: `x86_64-unknown-linux-gnu`, `aarch64-apple-darwin`, and
  `x86_64-apple-darwin` only.
- Archives contain only `bin/impetus` and `bin/impetus-cli`.
- Installer validates a SHA-256 sidecar before changing the destination and
  never uses `sudo`.
- `IMPETUS_RELEASE_BASE_URL` is test-only; `IMPETUS_INSTALL_DIR` is the user
  destination override.
- No Rust dependencies are added; finish with `task verify` and `task security`.

---

### Task 1: Installer behavior test and implementation

**Files:**
- Create: `scripts/install.sh`
- Create: `scripts/test-install.sh`

**Interfaces:**
- Consumes: a release base URL containing `<archive>.tar.gz` and its `.sha256`.
- Produces: executable `impetus` and `impetus-cli` in `IMPETUS_INSTALL_DIR`.

- [ ] **Step 1: Write the failing installer test**

Create a fixture archive containing two executable shell binaries. Run the
installer with `IMPETUS_RELEASE_BASE_URL=file://<fixture>` and a temporary
destination; assert both files exist and run. Replace the checksum with a bad
digest and assert the installer exits non-zero without creating files.

- [ ] **Step 2: Run the test to verify it fails**

Run: `sh scripts/test-install.sh`

Expected: FAIL because `scripts/install.sh` does not exist.

- [ ] **Step 3: Write the minimal installer**

Implement target selection for Linux x86_64 and Darwin arm64/x86_64, required
tool checks, checksum verification, archive-layout validation, and temp-dir
extraction. Install only after all validation succeeds.

- [ ] **Step 4: Run the test to verify it passes**

Run: `sh scripts/test-install.sh`

Expected: PASS for the valid fixture and the bad-checksum rejection case.

- [ ] **Step 5: Commit**

```text
ATM-124 feat: Добавлен проверяемый curl installer
```

### Task 2: Native CI and release publication

**Files:**
- Modify: `.github/workflows/check.yml`
- Create: `.github/workflows/release.yml`

**Interfaces:**
- Consumes: workspace crates `impetus`, `impetus-cli` and a `v*` tag.
- Produces: target-specific tar.gz and `.sha256` GitHub Release assets.

- [ ] **Step 1: Write the failing structural checks**

Add a shell validation that parses both workflow files as YAML when Ruby is
available and asserts the CI matrix has exactly the three supported targets;
assert the release workflow builds archives before `gh release create`.

- [ ] **Step 2: Run structural checks to verify they fail**

Run: `sh scripts/test-release-workflows.sh`

Expected: FAIL because the release workflow does not exist.

- [ ] **Step 3: Implement CI and release workflows**

Add a native build matrix to the existing check workflow. Add a tag-triggered
release matrix that packages both binaries, writes SHA-256 sidecars, uploads
matrix artifacts, validates installer fixtures, and creates one GitHub Release
from the gathered assets using the GitHub CLI.

- [ ] **Step 4: Run structural checks to verify they pass**

Run: `sh scripts/test-release-workflows.sh`

Expected: PASS. GitHub executes the native builds after push.

- [ ] **Step 5: Commit**

```text
ATM-124 ci: Добавлена native release матрица
```

### Task 3: Public installation documentation

**Files:**
- Modify: `README.md`
- Modify: `README.ru.md`

**Interfaces:**
- Consumes: `scripts/install.sh` at the repository default branch.
- Produces: one verified installation command and exact supported targets.

- [ ] **Step 1: Write a failing documentation assertion**

Extend the installer test to require the exact curl command and all three
supported targets in both README files.

- [ ] **Step 2: Run the assertion to verify it fails**

Run: `sh scripts/test-install.sh`

Expected: FAIL because README still calls distribution planned work.

- [ ] **Step 3: Update both README files**

Replace the planned-only note with the curl command, target list, destination,
and a short note that the first actual release is created by a `v*` tag.

- [ ] **Step 4: Run the assertion to verify it passes**

Run: `sh scripts/test-install.sh`

Expected: PASS.

- [ ] **Step 5: Commit**

```text
ATM-124 docs: Описана native установка
```

### Task 4: Final verification and push

**Files:**
- Verify only.

- [ ] **Step 1: Run product and security gates**

Run: `task verify && task security`

Expected: all Rust format, test, check, lint, audit, and deny gates pass.

- [ ] **Step 2: Inspect the complete diff and commits**

Run: `git diff origin/main...HEAD --check && git log --oneline origin/main..HEAD`

Expected: only installer, CI/release, documentation, and their focused tests.

- [ ] **Step 3: Push main**

Run: `git push origin main`

Expected: branch is synchronized with the remote.
