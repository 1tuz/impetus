# curl installation and native release design

## Goal

Publish and install the Impetus runtime and reference CLI on Ubuntu x86_64 and
macOS (Apple Silicon and Intel) with one `curl | sh` command, while verifying
the downloaded release archive before installing it.

## Scope

- Support `Linux` + `x86_64`, `Darwin` + `arm64`, and `Darwin` + `x86_64` in
  this slice. The native artifact targets are `x86_64-unknown-linux-gnu`,
  `aarch64-apple-darwin`, and `x86_64-apple-darwin`; unsupported systems fail
  before any download.
- Package both workspace binaries, `impetus` and `impetus-cli`, in one tarball.
- Add GitHub Actions native build jobs for Ubuntu x86_64, Apple Silicon, and
  Intel macOS on every pull request and push to `main`.
- Add a tag-triggered GitHub Actions release workflow for `v*` tags. It builds
  the native release binaries, creates a tarball and SHA-256 sidecar, and
  attaches both to the corresponding GitHub Release.
- Provide a POSIX-shell installer at `scripts/install.sh`. It downloads the
  archive and sidecar from GitHub's `releases/latest/download` endpoint,
  verifies SHA-256 with `sha256sum` on Linux or `shasum -a 256` on macOS,
  extracts to a temporary directory, and
  installs both binaries into `IMPETUS_INSTALL_DIR` or `~/.local/bin`.
- Use a temporary installation directory and rename into place only after a
  successful checksum and extraction. The installer never uses `sudo`.
- Add a shell test that serves a locally prepared release fixture through the
  installer override URL. It proves both successful installation and rejection
  of a tampered checksum.
- Update English and Russian README installation sections with the real command
  and the supported-platform boundary.

## Release contract

Each release publishes these target-specific asset pairs:

```text
impetus-x86_64-unknown-linux-gnu.tar.gz
impetus-x86_64-unknown-linux-gnu.tar.gz.sha256
impetus-aarch64-apple-darwin.tar.gz
impetus-aarch64-apple-darwin.tar.gz.sha256
impetus-x86_64-apple-darwin.tar.gz
impetus-x86_64-apple-darwin.tar.gz.sha256
```

The tarball contains executable files at `bin/impetus` and
`bin/impetus-cli`. The checksum sidecar contains the SHA-256 digest and the
archive basename in standard `sha256sum` format.

`scripts/install.sh` accepts `IMPETUS_RELEASE_BASE_URL` only for deterministic
tests; normal users receive the GitHub latest-release URL. `IMPETUS_INSTALL_DIR`
is the explicit destination override.

## CI flow

```text
pull request / main push
  -> Ubuntu x86_64 native build job
  -> Apple Silicon native build job
  -> Intel macOS native build job
  -> cargo build --release -p impetus -p impetus-cli

v* tag
  -> native release matrix (Ubuntu x86_64, Apple Silicon, Intel macOS)
  -> build, archive + SHA-256 for each target
  -> installer fixture test
  -> GitHub Release assets
```

The normal CI job is a build-compatibility gate; release publication is
restricted to version tags and requires `contents: write` only in that
workflow.

## Failure behavior

- Unsupported OS or architecture: explain the supported Ubuntu x86_64, Apple
  Silicon, and Intel macOS boundary.
- Missing `curl`, `tar`, or native SHA-256 command: name the missing
  prerequisite and exit non-zero.
- Download, checksum, archive-layout, or installation failures: leave the
  destination unchanged and exit non-zero.
- A failed GitHub Release upload fails the workflow; no installer documentation
  is updated to claim a release that was not published.

## Verification

- The installer test runs its success and tamper cases locally and in the
  release workflow.
- `task verify` covers all Rust workspace checks.
- `task security` is run because the change touches release/CI tooling but does
  not add Rust dependencies.
- GitHub Actions YAML receives a structural syntax check where available;
  GitHub runs all three native build gates on the pull request.
