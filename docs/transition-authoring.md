# Transition authoring

Mural currently supports **source-level transition contributions**, not runtime
plugins. Named profiles such as `transition.fast.type = push` create presets for
built-in implementations; they cannot define a new rendering algorithm.

This guide documents the current cross-crate work required to add a compiled-in
transition and the contract that work must preserve. The compiled-in registry
centralizes public metadata and capability reporting. Kind-specific parsing,
lifecycle dispatch, and rendering remain typed, exhaustive Rust code rather
than an untyped callback or native-plugin ABI.

## Choose the transition class first

Mural has three transition classes:

- **Immediate transitions** replace content without an animation lifecycle.
  `cut` is intentionally the only current member.
- **Pairwise effects** need the old texture, new texture, output geometry, time
  progress, and bounded typed parameters. Fade and push share this lifecycle.
- **Scene transitions** need supervisor-owned context and additional assets.
  Canvas depends on history and thumbnail layouts; world depends on canonical
  library indices and verified disk-cache coverage.

Do not force a scene transition into a generic two-texture effect interface.
`cut` also remains a deliberate special case: it must stay the immediate,
minimal-work override and queue reset path.

## Current implementation checklist

A new built-in kind normally requires all of the following.

1. **Registry, wire model, and compatibility (`mural-ipc`)**
   - Add one descriptor to `transition_registry.rs` with a stable name, class,
     request scopes, stability, runtime requirements, and typed parameters.
   - Add the corresponding typed `Transition` variant and fields.
   - Update compact-token parsing when the CLI should expose one.
   - Update JSON parsing and encoding in both directions.
   - Add round-trip tests plus tests for invalid and missing parameters.
   - Confirm `muralctl capabilities --json` reports the intended schema. Keep
     the capability schema version independent from the IPC protocol version.
   - Keep renderer-only planning messages off the public socket.

2. **Configuration (`mural-core::config`)**
   - Add the profile kind and only the fields meaningful for it.
   - Resolve built-in defaults and named-profile overrides in one place.
   - Reject fields from unrelated transition kinds with a specific error.
   - Test built-in, named, action-bound, invalid, and unreferenced profiles.
   - Do not permit built-in profile names to be retyped or accept an invalid
     profile merely because no action currently references it.

3. **CLI and reference surfaces (`muralctl`, docs, man pages)**
   - Parse per-command overrides without silently changing daemon defaults.
   - State whether explicit `set` and high-level library actions are supported.
   - Update command help, protocol documentation, config documentation, and man
     pages together.
   - Keep human help useful, but direct programmatic integrations to daemon
     capability discovery instead of copied transition lists.

4. **Pure math (`mural-render`)**
   - Put easing, geometry, timeline, and layout calculations here whenever they
     do not require textures, Wayland objects, or GL state.
   - Keep inputs explicit and deterministic.
   - Test endpoints, clamping, output aspect ratios, directions, and degenerate
     dimensions.

5. **Lifecycle planning (`murald::transitions`)**
   - For a two-scene effect, extend `pairwise::Effect`; do not copy the fade or
     push decode/upload/queue implementation. Give a scene transition its own
     bounded queued and active state.
   - Validate and decode target assets before replacing the current wallpaper.
   - Define duration, progress, acceleration, queued-start, finish, abort, and
     cleanup behavior.
   - Account for multi-output shared start time and output power-off deferral.
   - Delete every auxiliary texture on success, interruption, output removal,
     renderer restart, and error.
   - Audit public dispatch in `murald/src/apply.rs`, scene planning in
     `murald/src/supervisor.rs`, active/queued kinds in
     `murald/src/transitions/mod.rs`, and start/finish/abort/cleanup matches in
     `murald/src/surface.rs`; a new kind is incomplete until every match has an
     intentional arm.

6. **GPU adapter (`murald::egl_render`)**
   - Keep GL calls and texture ownership on the renderer thread.
   - Bound shader inputs, allocations, uploads, and draw counts.
   - Compile/link programs with actionable errors and a safe fallback.
   - Do not render or swap from Wayland protocol callbacks.
   - Preserve the previous wallpaper if preparation cannot complete.
   - Treat fade-like effects as a mix of complete scenes. Scaling rectangles,
     clear-color regions, and source alpha must be correct for each input; two
     overlapping image quads are not equivalent for `fit` or `center`.

7. **Verification**
   - Run format, unit tests, Clippy, man-page lint, and `git diff --check`.
   - Exercise start, midpoint, completion, interruption/queue acceleration,
     immediate `cut` override, decode failure, and output removal.
   - Perform a small live test on each compositor claimed by the change.
   - Check targeted leak behavior when new GL resources or error paths are added.

## Required invariants

Every transition contribution must preserve these behaviors:

- idle outputs do not request continuous frames;
- `cut` remains immediate and clears queued work for targeted outputs;
- queued animated requests remain FIFO and can be accelerated safely;
- batched outputs use a shared monotonic start time;
- image decode happens off the renderer thread when practical;
- GL upload/deletion stays on the renderer thread;
- powered-off outputs defer EGL work;
- preparation and first-frame failure leave the last successfully displayed
  wallpaper intact; a deferred GPU failure must preserve supervisor/renderer
  consistency, normally by restarting and restoring committed state;
- renderer failures remain isolated, and public sockets reject private
  renderer-planning request types.

## Direction for third-party effects

The completed foundation is:

1. compiled-in descriptors centralize transition metadata;
2. the public capability request exposes the registry;
3. fade and push share one pairwise lifecycle while scene transitions remain
   separate.

Before any third-party effect format is promised, the project would still need
to:

1. add conformance tests plus useful `validate` and `preview` commands;
2. define a versioned manifest and a narrow, typed pairwise shader interface;
3. load and compile third-party shader packages only inside the supervised
   renderer child, with strict source-size, uniform, texture, allocation, and
   draw-count limits plus documented recovery behavior.

A native Rust/C dynamic-plugin ABI is not planned. Rust has no stable plugin ABI,
and arbitrary native code would expand the trusted code and attack surface. The
renderer process boundary contains ordinary process crashes; it is not a sandbox,
and neither it nor validation can guarantee recovery from a GPU/driver hang caused
by a shader. Any shader-package design must state those limits rather than imply
that untrusted effects are safe. Canvas and world should remain compiled scene
transitions even if pairwise shader packages are added later.
