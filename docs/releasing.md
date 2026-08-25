# Mural release inputs

Mural follows the shared public versioned-release provenance policy for signed
tags, immutable publication, and failure recovery. This file records only the
project-specific release gates, artifacts, and packaging contract.

## Version and required checks

- The first release is `v0.1.0`; each later tag must be
  `vMAJOR.MINOR.PATCH` and match `[workspace.package].version`.
- The matching `CHANGELOG.md` heading must use `X.Y.Z - YYYY-MM-DD`.
- The signed tag's direct commit target must be on protected `main` and have
  these successful check runs:
  - `Test (Rust 1.95.0)`;
  - `Test (Rust stable)`;
  - `Format, lint, docs, and packaging`; and
  - `RustSec advisory scan`.
- The active branch ruleset must require those checks strictly, require pull
  requests, and block deletion and non-fast-forward updates.
- The active tag ruleset must include `refs/tags/v*` and block deletion and
  non-fast-forward updates.

Before signing, a repository administrator must also confirm that both
rulesets have no bypass actors and that immutable releases are enabled. Those
two administrative details are an intentional manual gate: GitHub does not
expose the immutable-release setting or ruleset bypass actors to the minimally
privileged `GITHUB_TOKEN`. The workflow validates the publicly visible rule
conditions and verifies the final release's immutable state.

## Non-publishing dry run

The manual workflow path exercises the same tests, release build, packaging,
checksum validation, and workflow-artifact upload without creating a tag,
attestation, or release:

```sh
export GITHUB_REPOSITORY=kris004/mural
gh workflow run release.yml \
  --ref main \
  --repo "$GITHUB_REPOSITORY"
```

For a checkout-only run, keep Cargo output and assets outside the tree:

```sh
target_dir="$(mktemp -d)"
artifact_dir="$(mktemp -d)"
trap 'rm -rf "$target_dir" "$artifact_dir"' EXIT HUP INT TERM

CARGO_TARGET_DIR="$target_dir" \
  cargo test --workspace --all-targets --locked
CARGO_TARGET_DIR="$target_dir" \
  cargo build --workspace --release --locked
BINARY_DIR="$target_dir/release" \
OUTPUT_DIR="$artifact_dir" \
RELEASE_REF=HEAD \
  scripts/package-release.sh vX.Y.Z linux-x86_64-gnu
scripts/verify-release-assets.sh \
  vX.Y.Z linux-x86_64-gnu "$artifact_dir" HEAD
```

## Asset and installation contract

Each release contains exactly:

- `mural-X.Y.Z-src.tar.gz`, the exact tagged tree;
- `mural-X.Y.Z-linux-x86_64-gnu.tar.gz`, a dynamically linked convenience
  build; and
- `SHA256SUMS`, covering both archives.

The binary archive contains `murald`, `muralctl`, the four manual pages, sample
configuration, both license files, and `murald.service` under a `$HOME/.local`
layout. `scripts/verify-release-assets.sh` checks that exact manifest, file
modes, installed version output, x86-64 PIE format, dynamic-library
resolution, lack of RPATH/RUNPATH, source-tree equality, and checksums.

The GNU/Linux binary is intentionally not static: Mural integrates with the
host Wayland, xkbcommon, EGL/OpenGL ES, and glibc stack. Gentoo and other
distributions should build from source instead of consuming this convenience
archive.

## Workflow behavior

`.github/workflows/release.yml` uses Rust 1.95.0 on Ubuntu 24.04 and the target
name `linux-x86_64-gnu`. It tests and builds once, uploads the three exact files
as one workflow artifact, downloads that artifact by numeric ID for both
attestation and publication, verifies draft asset sizes and SHA-256 digests,
then publishes and checks immutable status. Every release-side `gh` command
passes `--repo "$GITHUB_REPOSITORY"`.

The attestation job omits `artifact-metadata: write` deliberately. Mural
attests ordinary release files with `actions/attest` and does not push linked
artifact metadata to a package registry, so `contents: read`, `id-token: write`,
and `attestations: write` are sufficient and narrower.

## Gentoo overlay contract

The automatic overlay consumes the GitHub archive for the resolved tag commit,
not the attached binary. This release does not change the existing ebuild
contract: Rust 1.95, the committed `Cargo.lock`, optional `vips` support,
Wayland/xkbcommon/EGL runtime dependencies, Cargo workspace build and tests,
and the Makefile's staged `/usr` installation remain unchanged. A future
system dependency, feature, service, or install-path change requires a matching
overlay template review.
