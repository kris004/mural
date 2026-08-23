# mural IPC protocol

Status: supervised Wayland implementation. The public daemon process owns the
Unix socket, systemd readiness, wallpaper state, and renderer health. It starts
a same-binary renderer child for Wayland, layer-shell, EGL/GLES, frame
callbacks, textures, and swaps. `cut` is immediate; fade and push transitions
are animated with Wayland frame callbacks. Push defaults to portal mode.
High-level wallpaper actions can also use `canvas`, which zooms out to a capped, stable
preview tape of layout history plus forward shuffle-bag entries and then into
the selected wallpaper. Canvas can render that preview as clipped tiles,
morphing aspect-ratio tiles, overlapping full-image thumbnails, or a
focus-centered collage.
Explicit `set` requests reject `canvas` because
they bypass that history/shuffle state. Canvas uses a persistent, nonblocking
thumbnail cache; cold misses schedule warming and may fall back to `cut` instead
of stalling.
`world` is the full-library virtual world transition. It is parsed by the
protocol/config layer, uses a compact
supervisor-to-renderer request shape, and currently draws a bounded selected-LOD
prototype only when the required real cache tiles are ready. Far routes require
generated deeper LOD coverage. The daemon also owns a native, top-level-only
wallpaper library for high-level
next/back/shift/favorites/quarantine actions.

Transport:

- Unix stream socket.
- Default path: a nonempty `$MURAL_SOCKET`, otherwise an absolute nonempty
  `$XDG_RUNTIME_DIR/mural/mural.sock`, otherwise an effective-UID-scoped
  `/tmp/mural-UID/mural.sock` for ad-hoc test environments. Resolution fails
  closed and asks for an explicit environment path if the operating-system UID
  cannot be determined.
- The daemon creates new socket directories with owner-only permissions and
  binds the public socket with mode 0600. An existing `/tmp/mural-UID` fallback
  directory must be owned by that UID and have no group or other permissions.
- Public requests are limited to 1 MiB.
- One JSON request per connection.
- The client closes the write side after sending the request.
- The daemon replies with one compact JSON object and closes the connection.
- Request types prefixed with `renderer_` are reserved for the inherited private
  supervisor/renderer channel and are rejected on public and standalone sockets.

## Requests

### `ping`

```json
{"type":"ping"}
```

Successful response:

```json
{"status":"ok","type":"pong","version":1}
```

### `capabilities`

```json
{"type":"capabilities"}
```

The response describes the transition implementations compiled into the
running daemon. This abridged example shows one entry; the real response lists
every compiled-in transition in stable display order:

```json
{
  "status": "ok",
  "type": "capabilities",
  "schema_version": 1,
  "protocol_version": 1,
  "daemon_mode": "supervisor",
  "transitions": [
    {
      "name": "fade",
      "class": "pairwise",
      "scopes": {
        "explicit_set": true,
        "wallpaper_actions": true
      },
      "experimental": false,
      "requirements": [],
      "parameters": [
        {
          "name": "duration_ms",
          "type": "integer",
          "allowed_values": [],
          "required": false,
          "default": 900,
          "constraint": "positive integer milliseconds"
        },
        {
          "name": "easing",
          "type": "enum",
          "allowed_values": [
            "linear",
            "ease-out-cubic",
            "ease-in-out-cubic"
          ],
          "required": false,
          "default": "ease-out-cubic",
          "constraint": null
        }
      ]
    }
  ]
}
```

Capability schema versioning is independent from the IPC protocol version.
The response reports compiled support and static requirements, not runtime
readiness: for example, a world request can still fail its library/cache gate.
`daemon_mode` is `supervisor` or `standalone`, and `scopes` reports effective
availability for that endpoint. Standalone mode still lists the compiled world
metadata but sets both world scopes to `false` because it cannot perform
supervisor route planning. Parameter defaults use their advertised JSON type;
`required` distinguishes a required field from an optional field with no
default.
Clients should tolerate unknown fields and should not assume that named config
profiles appear here. An older daemon that does not understand this additive
request returns an ordinary error response. This client rejects capability
schema versions newer than the one it understands rather than guessing at
their meaning.

### `health`

```json
{"type":"health"}
```

Successful response:

```json
{
  "status": "ok",
  "type": "health",
  "role": "supervisor",
  "supervisor_pid": 1000,
  "renderer_pid": 1001,
  "renderer_generation": 2,
  "renderer_state": "running",
  "restart_count": 1,
  "last_error": "renderer exited",
  "last_diagnostic": "/home/user/.local/state/mural/diagnostics/renderer-1000-1780000000.txt",
  "outputs": [
    {
      "name": "DP-1",
      "layout_x": 0,
      "layout_y": 0,
      "width": 3840,
      "height": 2160,
      "power_state": "on",
      "render_state": "renderable",
      "restore_pending": false,
      "current_image": "/home/user/wall.jpg",
      "transition_target_image": null,
      "scale_mode": "fill",
      "transition_state": { "state": "idle" },
      "queue_depth": 0,
      "frame_callback_pending": false,
      "render_pending": false
    }
  ]
}
```

The supervisor answers `ping` itself. `health` reports the supervisor and
renderer process boundary, restart generation, last restart/error details, and
the renderer's current per-output renderability state. During an active
transition, `current_image` remains the settled wallpaper while
`transition_target_image` reports the active target path when one is known.

### `query`

```json
{"type":"query"}
```

Successful response:

```json
{
  "status": "ok",
  "type": "query",
  "outputs": [
    {
      "name": "DP-1",
      "current_image": "/home/user/wall.jpg",
      "scale_mode": "fill",
      "transition_state": { "state": "idle" },
      "queue_depth": 0
    }
  ]
}
```

### `set`

```json
{
  "type": "set",
  "outputs": {
    "DP-1": "/home/user/a.jpg",
    "DP-2": "/home/user/b.jpg"
  },
  "transition": {
    "type": "push",
    "direction": "up",
    "duration_ms": 900,
    "easing": "ease-out-cubic",
    "mode": "portal"
  },
  "scale_mode": "fill",
  "allow_partial": false
}
```

Current daemon behavior:

- validates that each requested image path exists and is a file;
- requires requested output names to match live Wayland outputs unless
  `allow_partial` is true;
- decodes immediately-started images as RGBA8 and uploads them as GL textures;
- queued animated requests store validated paths, decode up to four upcoming
  images per output on a background thread, and upload decoded textures on the
  main EGL thread;
- renders accepted outputs immediately when `transition.type` is `cut`;
- animates accepted outputs when `transition.type` is `fade` or `push`;
- rejects `canvas` for explicit `set` requests because `set` has no wallpaper
  action history context;
- supports `world` for explicit `set` requests through the supervisor: the
  supervisor reads current renderer health, maps current/target paths into the
  ordered library, checks real route cache coverage, and sends compact
  `renderer_world_set` metadata to the renderer. The renderer enforces bounded
  per-route and per-request world tile upload caps before starting. If an output
  is actively transitioning, the supervisor uses that active target as the
  queued `world` route start when the renderer reports it;
- if an animated request targets an output that is already transitioning, queues
  the request FIFO, accelerates the active transition, and then drains queued
  transitions at accelerated duration;
- `cut` is an immediate override and clears any queued work for its target
  outputs;
- returns an acknowledgement.

For `set`, `transition` may also be a compact string: `cut`, `fade`,
`push:up`, `push:down`, `push:left`, `push:right`, or `world`. Canvas compact
strings are valid only on high-level wallpaper actions.

Push transition `mode` is optional and defaults to `portal`:

- `portal`: translate a screen-sized virtual page behind the monitor viewport,
  with the full scaled wallpaper attached to that page so cropped fill-mode
  content can slide through;
- `screen`: translate only the visible output crop as a flat screen-sized tile.
- `pan`: translate each full scaled wallpaper independently until it fully exits
  its side of the viewport, clipped at the moving boundary.

Fade has `duration_ms` and `easing` fields. It interpolates complete old and new
scenes: each input uses its own scaling rectangle, pixels outside that rectangle
use the configured clear color, and in-rectangle pixels retain the same raw RGBA
texture semantics as a normal wallpaper draw. This keeps transparent-image
endpoints identical to non-transition rendering. The current contract is
numeric RGBA interpolation in the renderer's existing texture space; Mural does
not yet perform color-managed linear-light blending.

### `preload`

```json
{"type":"preload","outputs":{"DP-1":"/home/user/a.jpg"}}
```

Current daemon behavior validates paths only. Use `cache warm` to warm canvas
thumbnails.

### `clear`

```json
{"type":"clear","outputs":["DP-1"],"color":"#000000"}
```

An empty `outputs` array clears all outputs known to the daemon. `color` must
be `#rrggbb` and is rendered immediately through EGL/GLES.

Canvas transition object:

```json
{
  "type": "canvas",
  "zoom_out_ms": 180,
  "pan_ms": 80,
  "zoom_in_ms": 260,
  "easing": "ease-out-cubic",
  "mode": "clipped",
  "walk": "paged",
  "pan_axis": "auto",
  "overview_scale": 0.333333,
  "tile_count": "auto",
  "max_tile_count": 48
}
```

`max_tile_count` is optional and only caps `tile_count: "auto"`. A numeric
`tile_count` is an exact manual count, bounded by the internal safety maximum
(currently 256).
`mode` is optional and defaults to `clipped`; accepted values are `clipped` for
screen-sized clipped tiles, `morph` for aspect-ratio tiles that clip/unclip
during the animation, `overlap` for overlapping full-image thumbnails,
`collage` for the focus-centered collage layout, and `span` for a shared
desktop-spanning morph canvas. `collage` and `span` require `walk: "strip"`.
`walk` is optional and defaults to `paged`; accepted values are `paged` for the
bounded row/column canvas and `strip` for the older centered infinite-right
walk.
`pan_axis` is optional and defaults to `auto`; accepted values are `auto`,
`horizontal`, and `vertical`.

For high-level wallpaper actions, canvas builds a stable preview tape from the
layout's flattened wallpaper history around the old and selected windows, then
fills after known history with forward shuffle-bag entries from the current bag
cursor. It uses cached thumbnails opportunistically for preview tiles, reflows
to the actual ready tile set, and warms missing thumbnails for later
transitions without blocking the current request.

The canvas camera starts with the current wallpaper centered, zooms out, pans
along the configured natural-order axis to the target wallpaper's preview
position, then zooms in on the target wallpaper.

### `cache`

Canvas thumbnail cache status:

```json
{"type":"cache","action":"status"}
```

Warm the current preview window or the whole top-level library:

```json
{
  "type": "cache",
  "action": "warm",
  "scope": "current",
  "workers": 8,
  "backend": "auto"
}
```

Fields:

- `scope`: `current` or `all`; defaults to `current`.
- `workers`: positive worker count for this warm request; defaults to 1.
- `backend`: `auto`, `vips`, or `internal`; `auto` prefers `vipsthumbnail`
  when available.

Clear cached canvas thumbnails:

```json
{"type":"cache","action":"clear"}
```

Successful response:

```json
{
  "status": "ok",
  "type": "cache",
  "action": "warm",
  "message": "ready\t12\tpending\t8\tscheduled\t8\tfailed\t0",
  "ready": 12,
  "pending": 8,
  "scheduled": 8,
  "failed": 0,
  "backend": "vips"
}
```

### Offline `muralctl world cache`

The `world cache` CLI commands are local/offline helpers, not IPC requests.
They read mural config with the same environment/config precedence as the
daemon and scan the same non-recursive top-level wallpaper library.

- `muralctl world cache status [--json]` reports the world cache manifest,
  library count, row-major grid dimensions, fingerprint, order policy,
  readiness, and the last compute failure count/log path.
- `muralctl world cache index [--json]` writes the current manifest under
  `$state_dir/cache/world-v1/manifest` and a fingerprinted ordered path snapshot
  beside it.
- `muralctl world cache failures [--json]` prints the current
  `last-compute-failures.tsv` records directly, or an empty result when no
  failures have been recorded.
- `muralctl world cache compute --scope all [--dry-run] [--background]
  [--limit N] [--tile-limit N] [--workers N] [--progress] [--json]` generates
  real per-wall cell thumbnails and the all-library world tile LOD pyramid, or
  reports planned work with `--dry-run`. Plans report
  `estimated_remaining_bytes`.
- `muralctl world cache compute --scope route --from PATH --to PATH
  [--dry-run] [--background] [--tile-limit N] [--workers N] [--progress]
  [--json]` or repeated `--route FROM TO` pairs limits thumbnail/tile
  generation to the selected bounded route LOD for one or more rectangles
  between library paths. Selected LOD tiles are deduplicated across route pairs
  and generated directly from real source imagery or existing cell thumbnails
  without forcing persistent per-wall thumbnail generation for the route scope.
  `--workers N` parallelizes LOD tile child-block generation. Undecodable
  source cells are left transparent in the generated tile and recorded in the
  last-compute failure log instead of blocking the whole route tile. Plans
  report `estimated_remaining_bytes`.
- `muralctl world cache compute --scope neighborhood --center PATH
  [--center PATH ...] [--radius N] [--lod N] [--dry-run] [--background]
  [--tile-limit N] [--workers N] [--progress] [--json]` generates only the
  selected LOD tiles around one or more library paths. A radius of 0 warms the
  tile containing the center; larger radii include neighboring tiles for
  current/target prefetch. Plans report `estimated_remaining_bytes`.
- `--background` starts the same compute as a detached local process and
  reports the child PID plus a cache-local log path; it is valid for real
  compute only, not `--dry-run`.
Successful compute runs rewrite `$state_dir/cache/world-v1/last-compute-failures.tsv`;
`world cache status` reports the count and path so failed sources or tiles from
the last real compute are visible without keeping the full output scrollback.
Real `--progress` computes also end with `elapsed_ms` and
`work_rate_per_sec` fields.
World tile directories include the ordered-library fingerprint and grid
geometry, and tile filenames include a hash of the covered cell cache keys, so
tiles from an older library ordering or changed source thumbnail set are not
reused after cache churn.
The v1 world order policy is `path-snapshot-v1`: positions are stable for the
current manifest snapshot. Cache indexes keep fingerprinted path snapshots so a
stale-but-existing cache can still serve routes whose endpoints remain in that
snapshot; rescans can intentionally produce a new layout if canonical path order
changes.

Explicit high-level `world` requests stage the target, check the required route
tiles, schedule a best-effort background route warmup on missing coverage only
when the missing work fits the small automatic LOD0 budget, roll the staged
state back, and start `renderer_world_set` only when real cache tiles are ready.
Explicit `set --transition world` follows the same route/cache gate without
modifying mural's wallpaper history. Larger missing routes report the manual
warmup command without spawning a long cache job. After a successful `world`
transition, the supervisor also schedules a best-effort radius-0 LOD0
neighborhood warmup around the current/target paths plus one upcoming shuffle
candidate to make nearby or follow-up world moves more likely to be ready.
If `transition.world.fallback` is configured and an existing cache cannot cover
the requested route, the supervisor may use that immediate cut/push fallback
while the background route warmup prepares a future retry.

### `wallpaper`

High-level library/history/favorites control:

```json
{
  "type": "wallpaper",
  "action": "next",
  "transition": null,
  "scale_mode": null
}
```

When `transition` or `scale_mode` is `null` or omitted, `murald` uses the
daemon config defaults for that action.
Wallpaper action transition strings may be `cut`, `fade`, `push:DIR`,
`canvas`, `canvas:auto`, `canvas:horizontal`, `canvas:vertical`, or the
`world` spelling. `world` requests are cache-gated: covered routes render
through the bounded world prototype, while missing coverage either schedules a
small bounded background route warmup or reports that automatic warmup was
skipped by budget. The request then returns a clear warmup error or uses the
configured immediate fallback. It never falls back to fake world visuals.

Indexed actions use an object:

```json
{
  "type": "wallpaper",
  "action": { "type": "replace", "index": 0 },
  "transition": "cut",
  "scale_mode": "fill"
}
```

Supported actions:

- `next`
- `back`
- `shift-forward`
- `shift-back`
- `replace`
- `quarantine` (`quarentine` is accepted as an alias)
- `favorite`: add the current wall at the requested output index to the
  favorites list, rebuild the shuffle bag, and leave the current display
  unchanged. Favorites appear `favorite_weight` total times in rebuilt shuffle
  bags, so future random choices select them more often.
- `unfavorite`: remove the current wall at the requested index from favorites,
  rebuild the shuffle bag, and leave the current display unchanged.
- `favorites`: list favorite paths
- `current`
- `rescan`

Successful response:

```json
{
  "status": "ok",
  "type": "wallpaper",
  "action": "current",
  "message": "",
  "entries": [
    {
      "index": 0,
      "output": "DP-1",
      "favorite": false,
      "path": "/home/user/Pictures/wallpapers/a.jpg"
    }
  ],
  "favorites": []
}
```

Current behavior:

- chooses the wallpaper directory from `$MURAL_WALL_DIR`, then the config-file
  `wall_dir`, then legacy `$WALL_DIR`, then `~/Pictures/wallpapers`;
- reads daemon defaults from `$MURAL_CONFIG`, then
  `$XDG_CONFIG_HOME/mural/config`, then `~/.config/mural/config`;
- indexes only direct top-level JPG/JPEG/PNG/WebP files;
- stores fresh mural-owned state under `$MURAL_STATE_DIR`, then the config-file
  `state_dir`, then `$XDG_STATE_HOME/mural`, then `~/.local/state/mural`;
- watches the top-level wallpaper directory non-recursively and appends newly
  closed or moved-in image files to the shuffle bag;
- displays saved current wallpapers on daemon startup, or chooses the first set
  if no current state exists;
- defers rendering that would call `eglSwapBuffers` while
  `zwlr_output_power_v1` reports the target output powered off;
- orders outputs by Wayland logical position for shift semantics;
- keeps renderer-level `set` behavior all-or-nothing.

### `stop`

```json
{"type":"stop"}
```

Asks the daemon to exit cleanly. This is mainly useful for tests and ad-hoc
manual sessions.

## Errors

Errors use status `error` and a human-readable message:

```json
{"status":"error","message":"missing required field 'outputs'"}
```

Low-level commands print the raw response JSON. High-level wallpaper commands
print human-readable rows by default and raw JSON with `--json`. `muralctl`
exits with status `2` when the daemon returns protocol-level error status.
