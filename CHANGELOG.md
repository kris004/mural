# Changelog

All notable user-visible changes to Mural will be recorded here. The project is
pre-1.0; protocol, configuration, and state-format changes will be called out
explicitly before a release.

## Unreleased

## 0.1.0 - 2026-08-24

This is the first versioned source release of Mural.

### Added

- Public contribution, support, security, transition-authoring, and sample-config
  documentation.
- Structured bug, feature, documentation, and pull-request templates for public
  contribution.
- `--version` output for both command-line programs.
- A compiled-in transition registry plus `muralctl capabilities [--json]` for
  versioned discovery of transition classes, scopes, requirements, and typed
  parameters.
- A GPU-rendered `fade` transition for explicit sets and wallpaper actions. It
  interpolates complete scenes so scaling, letterboxes, clear color, and source
  alpha behave consistently.
- A privacy-safe generated transition demo and an explicit compositor/support
  compatibility matrix.
- CI coverage for the declared Rust 1.95 minimum, stable Rust, Clippy, rustdoc,
  manual pages, the generated systemd unit, staged installation, and scheduled
  RustSec dependency audits.

### Changed

- Reworked the source-install path to support `DESTDIR`, configurable service
  paths, pure file installation, and a compositor-neutral
  `graphical-session.target` user unit.
- Reframed the README around external users, explicit alpha maturity, platform
  requirements, compatibility limits, and the current source-level extension
  model.
- Consolidated fade and push preparation, FIFO queueing, acceleration,
  rollback, and texture ownership behind a shared pairwise-effect lifecycle.
- Transition configuration now validates every defined profile, including
  unreferenced profiles, and rejects fields that do not belong to its type.

### Fixed

- Public sockets now reject supervisor-to-renderer planning request types instead
  of forwarding them to the renderer child.
- Public sockets are owner-only and reject requests larger than 1 MiB; the
  emergency `/tmp` path now derives its user ID from the operating system.
- JSON request parsing now accepts valid escaped non-BMP Unicode characters and
  rejects excessive container nesting without exhausting the process stack.
- Empty or relative XDG config, state, and runtime roots no longer produce
  unintended relative paths; documented home or secure socket fallbacks apply.
- EGL implementations that require a positive minimum swap interval now fall
  back to driver throttling instead of failing surface setup.
- Updated locked transitive dependencies to versions patched for the current
  RustSec `memmap2` and `quick-xml` advisories.
- Corrected stale readiness, transition, service, and man-page documentation.
- Pairwise first-frame failures restore the last successfully displayed
  wallpaper before acknowledgement; deferred pairwise renderer failures now
  restart the supervised renderer so committed state and queued work cannot
  silently diverge.
