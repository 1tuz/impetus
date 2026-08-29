# Linux curl installation design

## Goal

Publish and install the Impetus runtime and reference CLI on Ubuntu x86_64 with
one `curl | sh` command, while verifying the downloaded release archive before
installing it.

## Scope

- Support only `Linux` + `x86_64` in this slice. The native artifact target is
  `x86_64-unknown-linux-gnu`; unsupported systems fail before any download.
- Package both workspace binaries, `impetus` and `impetus-cli`, in one tarball.
- Add a GitHub Actions Linux build job for every pull request and push to
  `main`.
- Add a tag-triggered GitHub Actions release workflow for `v*` tags. It builds
  the native release binaries, creates a tarball and SHA-256 sidecar, and
  attaches both to the corresponding GitHub Release.
- Provide a POSIX-shell installer at `scripts/install.sh`. It downloads the
  archive and sidecar from GitHub's `releases/latest/download` endpoint,
  verifies SHA-256 with `sha256sum`, extracts to a temporary directory, and
  installs both binaries into `IMPETUS_INSTALL_DIR` or `~/.local/bin`.
- Use a temporary installation directory and rename into place only after a
  successful checksum and extraction. The installer never uses `sudo`.
- Add a shell test that serves a locally prepared release fixture through the
  installer override URL. It proves both successful installation and rejection
  of a tampered checksum.
- Update English and Russian README installation sections with the real command
  and the supported-platform boundary.

## Release contract

Each release publishes exactly these assets:

```text
impetus-x86_64-unknown-linux-gnu.tar.gz
impetus-x86_64-unknown-linux-gnu.tar.gz.sha256
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
  -> Ubuntu build job
  -> cargo build --release -p impetus -p impetus-cli

v* tag
  -> Ubuntu release job
  -> build both binaries
  -> archive + SHA-256
  -> installer fixture test
  -> GitHub Release assets
```

The normal CI job is a build-compatibility gate; release publication is
restricted to version tags and requires `contents: write` only in that
workflow.

## Failure behavior

- Non-Linux or non-x86_64 hosts: explain the supported Ubuntu x86_64 boundary.
- Missing `curl`, `tar`, or `sha256sum`: name the missing prerequisite and
  exit non-zero.
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
  GitHub runs the Ubuntu build gate on the pull request.
