# mural plan

Date: 2026-05-18

This is the original design sketch. For the current short-form guardrails,
use [docs/architecture.md](docs/architecture.md). For active roadmap items,
use [docs/next-steps.md](docs/next-steps.md).

## Summary

Build a small Wayland wallpaper daemon that keeps persistent background
surfaces alive and renders wallpaper changes with GPU transitions. The first
priority is an explicit, flat-edged push transition in four directions:

- `push:up`: the new wallpaper enters from below and pushes the old wallpaper
  off the top.
- `push:down`: the new wallpaper enters from above and pushes the old wallpaper
  off the bottom.
- `push:left`: the new wallpaper enters from the right and pushes the old
  wallpaper off the left.
- `push:right`: the new wallpaper enters from the left and pushes the old
  wallpaper off the right.

The daemon should be controlled at runtime by IPC. Transition type, direction,
duration, easing, output set, scale mode, and image path must be provided per
wallpaper-change request. A renderer that only changes transitions through a
config reload is not acceptable for this use case.

## Goals

1. Zero blanking during wallpaper changes.
   - Never destroy the active layer-shell surface to change an image.
   - Decode and upload the incoming image before starting a transition.
   - If decode/upload fails, keep the existing wallpaper visible.
2. GPU-rendered transitions.
   - Use a persistent EGL/OpenGL ES renderer initially.
   - All transition animation should happen in shaders or GPU draw calls.
   - CPU should only decode images, prepare textures, and drive frame timing.
3. Explicit per-command transition control.
   - `muralctl set --output DP-1 --transition push:up --duration 900 file`
     must work without editing config files.
   - An external wallpaper controller should map actions directly to transitions:
     - next/forward: `push:up`
     - back: `push:down`
     - shift-left: `push:left`
     - shift-right: `push:right`
     - replace/quarantine/favorite changes: likely `fade` or `cut`
4. Efficient multi-monitor behavior.
   - One daemon process for all outputs.
   - One persistent background surface per output.
   - One event loop and one IPC socket.
   - Decode in worker threads, upload on the render thread.
   - Synchronize transition start timestamps across outputs in a batch request.
5. Sway-first, wlroots-compatible architecture.
   - Use `wlr-layer-shell` background layer surfaces.
   - Use `wl_output` and optionally `xdg-output` for output names/geometry.
   - Should work on Sway and likely other wlroots compositors.
6. Script-friendly, stable CLI.
   - CLI must be fast and deterministic.
   - JSON query output for integration with bars/menus/scripts.
   - Clear exit codes and stderr messages.

## Non-goals for version 1

- Owning wallpaper selection logic in version 1. An existing wallpaper controller
  can handle selection, history, favorites, quarantine, and monitor rotation.
  Version 1 should prove the renderer and IPC first. Long term, mural should
  absorb this logic so there is one wallpaper service and one source of truth.
- GNOME support. GNOME does not expose the same layer-shell path.
- Animated GIF/video wallpapers. Static image transitions first.
- Fancy shader transition gallery. The first-class transition is push. Add fade
  and cut for utility. Avoid wavy/noisy/reveal transitions unless explicitly
  requested later.
- Full color management. Respect image orientation and color reasonably, but do
  not make color management the first milestone.

## Proposed implementation language

Use Rust.

Reasons:

- Low startup overhead for the daemon and CLI.
- Good Wayland/client ecosystem.
- Memory safety for daemon code.
- Easy source packaging through Cargo and distribution-specific recipes.
- Direct access to EGL/OpenGL ES bindings and image decoding crates.

Initial crate layout:

```text
mural/
  Cargo.toml
  crates/
    murald/      # daemon binary
    muralctl/     # CLI binary
    mural-ipc/    # shared IPC types
    mural-render/ # renderer abstractions and shaders
  docs/
    protocol.md
    packaging.md
```

The repository now contains this workspace layout. Keep this section as
historical context; use the README and docs for current status.

## Rendering architecture

### Wayland objects

- Connect to the compositor through `WAYLAND_DISPLAY`.
- Discover outputs via `wl_registry` and `wl_output`.
- Use `wlr-layer-shell` to create one layer surface per output:
  - layer: background
  - anchor: top, right, bottom, left
  - exclusive zone: -1 or 0 as appropriate for background surfaces
  - input region: empty, so the wallpaper never captures input
- Track output add/remove/scale/transform events.
- Recreate or resize render targets only when output properties change.

### EGL/OpenGL ES path

Initial backend:

- EGL context shared across output surfaces when possible.
- OpenGL ES 3.0 if available; ES 2.0 fallback if practical.
- One render target/window surface per output layer surface.
- Textures:
  - current texture per output
  - incoming texture per output during transition
  - optional preloaded texture cache for recently used images
- Vertex data:
  - two triangles/full-screen quad per image
  - transition uniforms define progress and direction

Possible crates/libraries to evaluate:

- `wayland-client`
- `wayland-protocols-wlr`
- `smithay-client-toolkit`
- `calloop`
- `khronos-egl` or equivalent EGL bindings
- `glow` or generated OpenGL ES bindings
- `image`, `zune-jpeg`, or `ravif`/`libavif` optional support later
- `serde`, `serde_json`, or a compact IPC encoding if needed later
- `sd-notify` for systemd readiness

### Push transition shader semantics

Use a flat, rigid translation, not a wipe/reveal mask. Both old and new images
move together so the incoming wallpaper physically pushes the current wallpaper
out of the way.

Let `p` be eased progress in `[0, 1]`.

For `push:up`:

- old image translation: `(0, -p)` screen heights
- new image translation: `(0, 1 - p)` screen heights

For `push:down`:

- old image translation: `(0, p)`
- new image translation: `(0, -1 + p)`

For `push:left`:

- old image translation: `(-p, 0)`
- new image translation: `(1 - p, 0)`

For `push:right`:

- old image translation: `(p, 0)`
- new image translation: `(-1 + p, 0)`

Implementation options:

1. Two translated quads.
   - Render old texture quad with old transform.
   - Render new texture quad with new transform.
   - This is simple and guarantees a straight boundary.
2. Single shader sampling old/new based on translated coordinates.
   - More compact draw call but easier to get edge conditions wrong.

Prefer two quads for version 1 because correctness and visual clarity matter
more than saving one draw call.

### Easing

Required easing options:

- `linear`
- `ease-out-cubic` default for push, if it feels natural
- `ease-in-out-cubic`

Easing must be an IPC field, not config-only.

### Scale modes

At minimum:

- `fill`: preserve aspect ratio, cover output, crop overflow
- `fit`: preserve aspect ratio, contain within output, fill margins with color
- `center`: no scale up by default, centered
- `stretch`: fill output without preserving aspect ratio

Default for our setup should be `fill`, matching current wallpaper behavior.

Scaling should be done in vertex/texture coordinate calculation, not by
pre-resizing on CPU, except optional cached downscales for very large images if
memory or upload time becomes a problem.

## IPC and CLI design

### Socket

- Unix socket under `$XDG_RUNTIME_DIR/mural/mural.sock`.
- One request per connection is acceptable initially.
- JSON protocol for debuggability. If JSON overhead ever matters, switch to a
  binary format later while preserving the CLI.

### Request model

Batch requests are important so all outputs can start their transition at the
same monotonic timestamp.

Example JSON request:

```json
{
  "type": "set",
  "outputs": {
    "DP-1": "/home/user/Pictures/wallpapers/a.jpg",
    "DP-2": "/home/user/Pictures/wallpapers/b.jpg",
    "DP-3": "/home/user/Pictures/wallpapers/c.jpg"
  },
  "transition": {
    "type": "push",
    "direction": "up",
    "duration_ms": 900,
    "easing": "ease-out-cubic"
  },
  "scale_mode": "fill"
}
```

Required request types:

- `set`: set one or more outputs to explicit images with transition options.
- `query`: return current image, output geometry, scale mode, and transition
  state.
- `preload`: decode and upload an image for one or more outputs without showing
  it yet. Optional for v1, useful for eliminating first-frame delay.
- `clear`: set solid color or black. Useful for debugging only.
- `ping`: health check.
- `stop`: optional; systemd can handle stopping.

### CLI examples

```sh
muralctl set --output DP-1 /path/a.jpg \
  --transition push:up --duration 900 --easing ease-out-cubic

muralctl set \
  --output DP-1=/path/a.jpg \
  --output DP-2=/path/b.jpg \
  --output DP-3=/path/c.jpg \
  --transition push:left --duration 900

muralctl query --json
muralctl preload --output DP-1 /path/a.jpg
```

The CLI should try to connect to the socket. If the socket is missing, it may
try `systemctl --user start murald.service` once and retry. That gives us
lazy startup behavior without requiring the daemon to run before first use.

## Integration with an existing wallpaper controller

Use a staged migration. First, add mural as a backend renderer for
an existing wallpaper-selection script. After the renderer is proven stable,
move the selection/state logic into mural itself and reduce the legacy
controller to a compatibility wrapper or remove it.

Add a controller-side backend selection after the daemon proves itself:

```text
WALLPAPER_RENDERER=mural
```

Renderer mapping:

- Build a single batch command with all output/path pairs.
- Use action-derived transition:
  - `next`: `push:up`
  - `back`: `push:down`
  - `shift-forward`: `push:left`
  - `shift-back`: `push:right`
  - `replace`: `fade` or `cut`
  - `quarantine`: `fade` or `cut`
  - startup: `cut`
- Fail closed: if `muralctl set` fails, leave state untouched or roll back
  the controller's state write.

Short-term boundary: the existing controller remains responsible for choosing
images. The daemon should not implement favorites/quarantine/history until the
renderer is stable and the IPC/API shape is proven.

Long-term boundary: mural should own the full wallpaper control plane:

- wallpaper library scanning and file metadata
- current image state per output
- history and back/forward navigation
- favorites and weighted favorite selection
- quarantine and replace
- explicit show/open-current support
- per-monitor rotation/shift semantics
- menu/query APIs for bars, launchers, and future UIs

At that point, `muralctl next`, `muralctl back`, `muralctl shift-left`,
`muralctl favorite`, `muralctl quarantine`, and `muralctl show` should replace
most direct legacy-controller invocations.

## State model

Daemon state should be minimal and renderer-focused:

Per output:

- output name
- output description, make/model if available
- geometry, scale, transform
- current image path
- current texture handle
- pending/incoming image path and texture while transitioning
- scale mode
- transition state:
  - idle or running
  - transition kind
  - direction
  - start time
  - duration
  - easing

Persistent state file is optional for renderer-only v1. It becomes required
when mural owns the selection logic. The daemon can restore from a small state
file later:

```text
$XDG_STATE_HOME/mural/state.json
```

For the initial backend phase, an external controller owns current wallpaper
state, and compositor session startup can call it once with transition `cut`.
After migration, mural should own startup restoration directly and the session
should only start `murald`.

## Performance plan

### Image loading

- Use a worker thread pool for decode.
- Main render thread never blocks on disk IO or image decode after receiving a
  request, except optionally for small initial implementation.
- Apply EXIF orientation if supported.
- Convert decoded images to RGBA8 or GPU-friendly format.
- Upload textures on render thread.
- Drop CPU image buffers after upload unless useful for a cache.

### Texture cache

Version 1 can keep only current and incoming textures. Version 2 may add:

- LRU by image path and output size.
- Optional memory cap, e.g. 512 MiB default.
- Preload command to stage likely next images from an external controller.

### Frame scheduling

- Only schedule frame callbacks while a transition is active.
- When idle, commit only on output/config changes.
- Use compositor frame callbacks for pacing.
- Avoid timers that wake continuously while idle.

### Multi-monitor synchronization

For a batch set request:

1. Validate all paths and outputs.
2. Start decode jobs for all images.
3. Upload all textures.
4. If every required output is ready, set a shared `start_time`.
5. Start all output transitions in the same event-loop tick.
6. If one output fails, either:
   - abort the whole batch and keep all old wallpapers, or
   - apply successful outputs only if the request explicitly allows partial.

Default should be all-or-nothing.

## Reliability requirements

- The active wallpaper must remain visible on errors.
- A failed image decode must return an IPC error and not blank the output.
- Output hotplug should create/destroy only the affected surface.
- Config reload should not be required for normal wallpaper changes.
- Daemon should log concise errors to stderr/journal.
- Support `Type=notify` in systemd so dependent units know when the socket and
  Wayland surfaces are ready.
- On compositor disconnect, exit cleanly and let systemd restart if configured.

## Systemd/session model

Provide user units eventually:

```text
murald.service
murald.socket   # optional, if socket activation is implemented
```

Recommended initial service:

```ini
[Unit]
Description=GPU wallpaper daemon
PartOf=sway-session.target
After=sway-session.target
Requisite=sway-session.target

[Service]
Type=notify
ExecStart=%h/.local/bin/murald
Restart=on-failure
RestartSec=1

[Install]
WantedBy=sway-session.target
```

Socket activation is possible, but only worth doing after confirming the user
systemd environment reliably has `WAYLAND_DISPLAY`, `XDG_RUNTIME_DIR`, and any
needed graphics/session variables. The CLI autostart fallback may be simpler and
more predictable.

## Testing plan

### Unit tests

- Transition direction math:
  - push:up old/new transforms at progress 0, 0.5, 1.0
  - push:down, push:left, push:right equivalents
- Easing functions.
- Scale-mode coordinate calculation for common aspect ratios.
- IPC serialization/deserialization.
- Request validation and all-or-nothing batch behavior.

### Headless/render tests

- Use an offscreen OpenGL/EGL path if practical.
- Render frames for push transitions into buffers.
- Verify boundary is flat and moves monotonically.
- Verify no black/blank frame is produced after a valid current texture exists.

### Live Sway tests

- Start daemon under the live Sway session.
- Set distinct wallpapers on all three outputs with `cut`.
- Run slow `push:up`, `push:down`, `push:left`, `push:right` tests using visibly
  different wallpapers.
- Repeat rapid key presses to verify interruption behavior:
  - Option A: finish current transition immediately then start next.
  - Option B: start new transition from the currently rendered interpolated
    frame. This is nicer but more complex; use option A for v1.
- Unplug/disable/re-enable an output if practical.
- Restart Sway/session and confirm startup does not blank.

### Performance checks

- Measure CLI latency for IPC-only path.
- Measure image decode time for common wallpaper sizes.
- Measure GPU upload time.
- Confirm idle CPU is effectively zero.
- Confirm memory use with three outputs and large wallpapers.

## Milestones

### Milestone 0: repository and design

- Save this plan.
- Choose project/crate names.
- Decide initial GL binding stack.

### Milestone 1: minimal daemon

- Connect to Wayland.
- Create one background layer surface per output.
- Clear each output to a solid color.
- Provide `muralctl ping` and `query`.
- Add systemd user service skeleton.

### Milestone 2: static wallpaper set

- Decode image.
- Upload texture.
- Render static wallpaper with `fill` mode.
- Implement `muralctl set --transition cut`.
- Confirm no blanking on repeated sets.

### Milestone 3: push transitions

- Implement two-quad push renderer.
- Add `push:up`, `push:down`, `push:left`, `push:right`.
- Add duration and easing fields.
- Synchronize batch transitions across outputs.

### Milestone 4: external-controller backend

- Integrate `murald` as a renderer backend in an existing controller.
- Map actions to transition directions.
- Keep the previous renderer as a fallback during evaluation.
- Validate all current wallpaper keybindings.

### Milestone 5: absorb controller logic

- Port the existing wallpaper selection/state machine into mural.
- Preserve existing state files or provide a one-time migration.
- Add daemon-native commands for next, back, shift-left, shift-right, favorite,
  unfavorite, quarantine, replace, current/show, and menu/query support.
- Keep a small legacy-controller compatibility wrapper temporarily so existing
  keybindings can migrate gradually.
- Move keybindings to `muralctl` after behavior matches the current script.

### Milestone 6: polish

- Add preload support.
- Add query JSON used by bars, launchers, or menus if useful.
- Add packaging docs and distribution-specific recipes.
- Add tests and CI basics.

## Open questions

1. Should the daemon eventually absorb controller logic, or remain a pure
   renderer? Recommendation: pure renderer first, then absorb the logic once
   transitions and IPC are reliable.
2. Should failed batch requests be all-or-nothing or partial? Recommendation:
   all-or-nothing by default, optional `--allow-partial` later.
3. Which transition should destructive actions use? Recommendation: `fade` or
   `cut`, not directional push, to avoid suggesting normal navigation.
4. Should `muralctl` support true socket activation? Recommendation: maybe,
   but do CLI-triggered `systemctl --user start` first.
5. Should we target OpenGL ES only, or add a `wgpu` backend later? Recommendation:
   OpenGL ES first because layer-shell + EGL is direct and small; evaluate wgpu
   only if it simplifies portability or shader management.
