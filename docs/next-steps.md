# mural next steps

Status: 2026-08-22

This file is the roadmap only. Current behavior belongs in `README.md`,
`docs/protocol.md`, and `docs/architecture.md`.

## 1. Add a reusable texture cache and real preload

`preload` currently validates paths only. The next useful renderer improvement is
an actual bounded cache that can be shared by `preload`, queued transitions, and
later `set` requests.

Useful shape:

- cache by image path plus output/scale-relevant parameters; canvas now has
  a persistent thumbnail cache, but full-resolution reusable texture caching is
  still future work;
- decode off the render thread;
- upload on the EGL/render thread;
- enforce a memory cap and evict least-recently-used textures;
- keep current/incoming wallpapers alive even if cache pressure evicts other
  entries.

## 2. Investigate GPU-assisted canvas thumbnail generation

Compressed image decode is still CPU-side for JPG/PNG/WebP. Canvas now prefers
VIPS for disk-persisted thumbnails when available. A possible future
optimization is to decode once, upload, and render downscaled thumbnails into GL
textures/FBOs so resizing/caching happens on the GPU. This needs careful memory
limits and should be justified by measurements against the VIPS-backed cache.

## 3. Add service, compatibility, and diagnostics polish

Readiness, watchdog notification, renderer supervision, and startup restoration
are implemented. Useful follow-up work:

- publish and maintain a tested compositor/GPU compatibility matrix;
- add clearer journal errors for decode/upload/output failures;
- evaluate an optional CLI autostart fallback if the socket is missing;
- exercise compositor-neutral systemd/session startup beyond the primary Sway
  development environment.

## 4. Polish native wallpaper selection/state

Mural now owns wallpaper selection and state through `muralctl` high-level
commands. Useful follow-up work:

- compatibility wrappers for legacy wallpaper-script keybindings during
  migration;
- optional menu/query output tailored for bars and launchers;
- an optional importer for state from older wallpaper scripts;
- broader live smoke tests around quarantine rollback and hotplugged layouts.

## 5. Continue the virtual full-library world

The bounded `world` prototype now renders covered cache-gated routes from real
world-cache tiles. Small missing LOD0 routes can auto-schedule background route
warmups, larger misses report the manual warmup command, and successful world
transitions schedule current/target plus first-upcoming shuffle-candidate
neighborhood warmups. Remaining work is broader prefetch/full-cache scheduling
polish, viewer reuse, and decisions on the open UX/config questions. See
`docs/virtual-world.md` for the current plan and guardrails.

## 6. Strengthen the transition extension path

Named profiles tune compiled-in transitions. Mural now has a compiled-in
descriptor registry, public capability discovery, a shared pairwise lifecycle,
and fade as the first second effect on that lifecycle. Adding a new kind still
requires intentional typed matches across IPC, config, lifecycle, and rendering
code. Before promising a third-party effect format:

- broaden conformance tests for preparation, interruption, cleanup, output
  removal, and `cut` override behavior;
- consider daemon-backed `validate` and preview tooling for effect authors;
- only then evaluate a versioned, validated pairwise shader-package format
  loaded inside the supervised renderer child.

Native dynamic-library plugins are not planned; canvas and world remain
compiled scene transitions because they require supervisor-owned context.

## Known limitations to address when relevant

- EXIF orientation is not applied yet.
- Immediately-started images still decode synchronously unless already prepared.
- No reusable full-resolution texture/preload cache yet.
- Native library watching is intentionally non-recursive and top-level-only.
