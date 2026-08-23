# virtual world canvas plan

Status: bounded route-gated renderer/cache prototype

Review note: this plan was written after the bounded `canvas` transition work.
Later review notes in this file are design risks to validate, not live-tested
requirements. If a better `world` implementation needs a different architecture,
change the architecture deliberately instead of forcing the feature to fit the
current daemon shape.

This is the design and implementation plan for a full-library world transition.
It is not an extension of the bounded `canvas` preview window. The goal is to
make the entire ordered wallpaper library feel like one stable, real world while
doing only bounded runtime work.

The core requirement is that `world` transitions over real wallpaper imagery
from the current wall to the selected target. It must not be a source/target
transition with fake, synthetic, blurred, streaked, or approximate middle
frames. Large-library feasibility comes from a disk-backed precomputed world
cache, not from drawing or loading every source image at transition time.
Every pixel drawn for the world should come from the ordered library through a
known cache level. At far zoom, wallpapers are necessarily miniaturized by the
screen resolution, but the representation is still generated from the real
wallpaper files rather than invented filler.

Current implementation slice:

- `world` is now a bounded prototype transition spelling in IPC, CLI, and
  config.
- Configured `world` transitions still fail startup on a true cold/no-cache
  state, but an existing indexed/tiled cache is allowed to start even if the
  live library has changed or only partial route coverage exists. Covered
  routes render from the indexed cache snapshot; impossible or missing routes
  fail before rendering, optionally fall back to an immediate cut/push
  transition, and schedule a best-effort background route warmup only when the
  missing work is within the small automatic LOD0 budget. Larger misses report
  the manual warmup command without spawning a long cache job.
- The first pure row-major world model lives in `mural-render` so layout/query
  tests can grow separately from bounded `canvas` and EGL drawing. A pure
  `WorldSnapshot` now captures ordered library paths with stable path-to-index
  lookup for route planning and future viewer reuse.
- Pure route-to-cache-tile coverage also lives in `mural-render`, so later
  daemon/cache gates can ask which real world tiles a transition needs before
  starting.
- A simple pure world camera path now lives in `mural-render` as a separate
  zoom-out/zoom-in model for renderer prototyping.
- The supervisor now stages explicit high-level `world` requests far enough to
  identify the current-to-target route and check required real LOD tiles. Missing
  or over-budget coverage rolls the staged plan back; covered routes are handed
  to the renderer with compact route metadata. Explicit `set --transition world`
  uses the same supervisor route/cache gate from current renderer health without
  modifying mural's wallpaper history.
- Daemon startup now allows configured `world` transitions when there is an
  existing world tile cache, even if the manifest is stale for the live library.
  A true missing/invalid/no-tile cache still fails startup with the compute
  command to run.
- The route gate has a deliberately small tile budget and now selects the first
  available cache LOD that keeps the handoff bounded. Missing LOD coverage still
  fails before rendering instead of falling back to fake imagery, and the error
  schedules a best-effort background route warmup only for small LOD0 misses,
  then gives a batch route-scoped warmup command covering each requested output
  route. Larger route warmups are left as explicit user-run commands.
- The supervisor-to-renderer handoff now has a compact `renderer_world_set`
  request carrying library/cache metadata and per-output current/target indices
  instead of serializing the full library path list.
- The renderer has a bounded prototype that uploads the required precomputed
  real world tiles at the selected LOD plus current/target full-resolution
  textures and animates through the pure world camera path. Outputs that are
  already transitioning can queue a covered `world` transition; renderer health
  exposes the active target path so the supervisor validates the queued route
  from the wallpaper that will be current when the queue drains.
- `muralctl world cache status`, `muralctl world cache index`, and
  `muralctl world cache compute --scope all --background --progress` exist as
  offline scaffolding. Route-scoped compute also exists for
  `--scope route --from PATH --to PATH` or repeated `--route FROM TO` pairs.
  These commands index the same top-level library view as murald and generate
  real per-wall cell thumbnails plus a first world-tile LOD pyramid keyed by
  the ordered-library fingerprint, grid geometry, and covered source thumbnail
  keys. Route-scoped compute selects the same bounded LOD as the daemon,
  deduplicates tiles across route pairs, generates those rendered LOD tiles
  directly from real source imagery or existing cell thumbnails without forcing
  persistent per-wall thumbnail generation, and reports ready/missing/planned
  tile work with LOD labels plus a practical estimated remaining byte count.
  Real progress compute runs also print elapsed time and work rate, and rewrite
  a last-compute failure log that `world cache status` reports.
  Undecodable source images inside a generated route tile are left transparent
  and recorded in that failure log rather than blocking the entire route tile.
  Neighborhood-scoped compute can warm the selected LOD tiles around
  current/target paths without warming the whole library or populating the
  full cell-thumbnail cache. Cache indexes now write an ordered path snapshot
  keyed by fingerprint so stale-but-existing cache snapshots can still satisfy
  routes whose current and target wallpapers are present in that snapshot. Real
  route, neighborhood, and all-library computes can be launched with
  `--background`, which reports a
  child PID and cache-local log. After a successful `world` transition, murald
  now schedules a best-effort background radius-0 LOD0 neighborhood warmup around
  the route's current and target paths plus the next shuffle candidate for
  nearby/follow-up world moves.

This is intentionally only a bounded visual prototype. Keeping missing-cache
routes rejected until real cache coverage exists protects existing `cut`,
`push`, and bounded `canvas` behavior and avoids fake middle frames.

## Goals

- Show wallpaper navigation as movement through a stable full-library world.
- Avoid thumbnailing or keeping textures for the entire library in memory.
- Render the middle of the transition from actual wallpaper-derived cache data,
  not placeholders or generated filler.
- Use the canonical ordered library for world positions. The shuffle bag may
  choose a target, but it must not define the world's visual order.
- Reuse the world model later for an interactive wallpaper browser/viewer.
- Keep existing `canvas` modes stable; do not route this through the bounded
  canvas tile-window code unless a helper is clearly generic.
- Preserve `cut` and normal high-level navigation latency: world cache
  generation should run ahead of time or asynchronously, and a `world`
  transition should be cache-gated rather than stalling normal non-world
  actions.
- Fail fast at daemon startup only for a true missing/invalid/no-tile world
  cache. Do not make every library addition/removal break startup; if an indexed
  cache snapshot still exists, start and let route gating decide whether `world`
  can run or should fall back/warm in the background.
- Account for the current supervisor/renderer split during prototyping, or
  intentionally replace it if `world` needs a better ownership model.

## Non-goals for the first version

- No perfect magazine-style packing algorithm.
- No source/target-only transition with fake, blurred, or synthetic middle
  imagery.
- No requirement that ordinary `cut`, `push`, or bounded `canvas` transitions
  wait for world-cache work.
- No recursive library indexing beyond the current mural library behavior unless
  that is changed separately.
- No attempt to decode source files, generate world cache entries, or upload
  thousands of individual wallpapers during an active frame.

## User-visible model

The transition is named `world`. It remains separate from `canvas.mode` rather
than adding another bounded canvas layout.

Current `canvas` means:

- build a bounded preview list around history plus upcoming bag entries;
- arrange that window into a local canvas;
- transition between local positions.

Virtual world means:

- use the full ordered library index as the stable source of truth;
- map every library entry to a deterministic world cell;
- start centered on the current wall's world cell;
- zoom out/pan through real cached imagery for that world;
- zoom in to the target wall's world cell;
- draw the world from precomputed disk-backed tiles or wall thumbnails at the
  needed level of detail.

Current integration note: unlike `canvas`, explicit `set --transition world`
can be supported safely because the supervisor can read current renderer health,
map both current and requested paths into the ordered library, and send the same
compact route metadata used by high-level wallpaper actions. This does not
change mural's history/shuffle state; it is a renderer update only.

## Implementation invariants

These are the handoff requirements for implementation:

- `world` visual placement comes from canonical ordered library paths, never
  shuffle-bag order.
- `world` is a real imagery transition. During an active world transition, every
  visible world tile/cell must come from actual wallpaper-derived cache data.
- Disk-backed precompute is required for large libraries. Do not try to satisfy
  the real-world requirement by live-decoding or live-compositing the full
  library during the transition.
- If config enables `world`, startup validates that an indexed world cache
  exists before reporting ready. A missing/invalid/no-tile cache is a startup
  error with a command to run. A stale-but-existing cache is allowed because
  routes can still use its recorded path snapshot when both endpoints and the
  required real tiles are present.
- The compute command must work without a running daemon and show progress. The
  provisional command shape is:

  ```sh
  muralctl world cache compute --scope all --background --progress
  ```

- Cold cache is not allowed to silently degrade to fake visuals. Configured
  `world` fails startup when no usable tile cache exists, and ad-hoc requests
  return a clear warming/refusal response. With an existing cache, missing or
  impossible routes may use the configured immediate fallback while scheduling
  bounded background warmup for a future retry.
- Implement pure model and cache-manifest validation before wiring GL animation;
  otherwise cache/readiness failures will be hard to distinguish from render
  bugs.

## Core data model

### Library order

Use a deterministic `WorldIndex` built from the same ordered wallpaper library
state that feeds target selection. The shuffle bag is only a target-selection
mechanism; it does not control visual placement.

Initial order should be simple and explainable:

1. canonical path order as indexed by mural;
2. stable integer `world_index` for each path;
3. row-major placement into a fixed-width grid.

The shuffle bag may still choose the next target. After the target path is
chosen, both current and target are located in canonical library order and the
camera travels between those real world positions.

V1 order policy is `path-snapshot-v1`: positions are stable for one indexed
library snapshot and for every transition built from that manifest, but a later
rescan/reindex can move cells if path order changes. That is an explicit v1
tradeoff, not an accidental prototype behavior. A future muscle-memory-stable
browser can add a persistent world-index table that assigns cells once and
leaves holes behind when entries disappear.

### World layout

Start with a row-major grid because it is stable, cheap, and easy to virtualize.

```text
world_index = path index in stable library order
column = world_index % columns
row = world_index / columns
cell = [column, row, 1, 1]
```

First-version `columns` options:

- auto from library size and a preferred world aspect ratio;
- config override later if needed;
- keep it stable for a given library snapshot so positions do not shift during a
  transition.

Future versions can add packed rows or aesthetic placement, but only if they
preserve stable lookup from path/index to world rect.

### World snapshot

Each transition should capture a `WorldSnapshot`:

- library generation/id;
- ordered path list or path-to-index map;
- grid columns/rows;
- current index;
- target index;
- world cache manifest/tile parameters;
- transition distance policy.

The snapshot prevents mid-transition library watcher changes from moving tiles
under the camera.

Integration risk to validate: the current supervisor mode means the supervisor
owns wallpaper actions and may have newer state than the renderer child's
independently loaded wallpaper state. A prototype can handle that by moving
snapshot construction into `mural-core`/supervisor code and sending either:

- a compact snapshot/generation reference to a synced renderer-side world index;
  or
- a measured, bounded snapshot payload that stays well under the private control
  frame limit even for large libraries.

Or it can replace that ownership/handoff model with something better. The
important part is to deliberately design and test the large-library handoff
rather than accidentally JSON-serializing a 10k-100k path vector per transition.

### Multi-output behavior

Wallpaper actions can select one current/target pair per output. A conservative
starting point is one shared `WorldSnapshot` for the library layout plus a
per-output focus record:

- output name;
- old path/index/rect;
- target path/index/rect;
- scale mode and timing.

The existing transition model suggests independent per-output cameras with one
shared start timestamp as a low-risk prototype. That is not a hard requirement:
if the world effect needs a desktop-spanning/group camera, design that directly
and add tests around multi-output geometry.

## Virtualized rendering

The renderer should never iterate the whole library per frame. It also should
not fake the middle of the transition. Per-frame rendering should sample real
precomputed world cache tiles or ready per-wall thumbnails that were generated
from the user's actual library before the frame started.

For each frame:

1. compute the camera transform from current rect to overview/path to target
   rect;
2. invert the transform to get the visible world rectangle;
3. expand by a prefetch margin;
4. convert visible world rect to row/column ranges;
5. clamp to world bounds;
6. choose the cache level whose world tiles map cleanly to the current screen
   pixel density;
7. draw only the intersecting real world-cache tiles or ready cell thumbnails.

For row-major grids this is O(visible rows * visible columns), not O(library).
Review note: define and test a maximum visible-cell budget. If a very zoomed-out
or compressed-travel frame would expose too many cells for individual-thumbnail
drawing, switch to precomputed lower-detail world tiles. Do not switch to fake
imagery. **The pure world model now has a route LOD selector that tests near and
extreme jumps against a maximum route-tile budget, and the supervisor uses that
selector for bounded world route planning.**

Per-frame rendering must not decode images or synchronously read the full disk
cache. It should consume already-ready textures and queue future cache work
outside the active frame. If required real cache coverage is missing, the
`world` transition schedules a background route warmup only when the missing
work is within the automatic small-LOD0 budget and does not start; a configured
immediate fallback may run instead. Larger route warmups are reported as manual
commands rather than spawned automatically.

Pseudo-shape:

```rust
struct VisibleWorldQuery {
    world_rect: Rect,
    margin_cells: f32,
    screen_size: Size,
}

struct VisibleWorldTile {
    lod: usize,
    x: usize,
    y: usize,
    rect: Rect,
    state: WorldTileState,
}
```

## Real world cache policy

Use a disk-backed world cache designed for large libraries. The cache should
make the world real without requiring the renderer to hold the full library in
memory.

Requirements:

- per-wall thumbnail cache by source path plus mtime/size plus requested
  thumbnail edge;
- precomputed world tile cache keyed by library snapshot/grid parameters,
  level-of-detail, tile coordinate, and the source thumbnail keys covered by
  that tile;
- a manifest that records the ordered file list, grid dimensions, cache version,
  generated levels, and tile readiness;
- bounded in-memory decoded/GL texture cache for currently visible world tiles;
- async precompute queue with priority for the LOD tiles needed by likely
  transitions and the current/target neighborhoods;
- optional warm-all background generation for the whole ordered library;
- cancellation/coalescing for stale visible-world work after fast navigation;
- small upload budget on the EGL thread for ready world tiles, with texture
  deletion also on the EGL thread;
- no placeholder/background inside an active `world` transition. Missing real
  cache coverage should make the command schedule cache generation and return a
  clear warming/refusal response;
- no full-library memory residency.

The cache should have at least two logical layers:

1. **Cell thumbnails**: real thumbnails for individual wallpapers, used near
   the beginning/end of the zoom and as source material for world tiles.
2. **World tile pyramid**: composited image tiles for blocks of the ordered
   world at multiple levels of detail. Far zooms draw a small number of
   low-detail world tiles that still derive from the actual wallpapers. Closer
   views draw higher-detail world tiles or individual cell thumbnails.

For a 10k-100k image library, the full world in cell-thumbnail pixels is much
larger than any output. It should be treated like a map: split into disk tiles,
build lower-detail levels, and render only the tiles intersecting the camera.

Potential cache sizes:

- cell thumbnails around 256-512px maximum edge;
- world tiles around 1024-2048px square;
- multiple LODs down to a full-world overview level.

The exact sizes should be measured on the real library. Cache commands must
report estimated disk use before full warm-all jobs.

### Precompute and readiness

`world` needs explicit precompute/readiness semantics:

- `world cache index` builds or refreshes the ordered world manifest;
- `world cache compute --scope all --background --progress` generates enough
  cell thumbnails and world tiles to make the whole ordered library traversable
  for real;
- `world cache compute --scope route --from CURRENT --to TARGET --progress`
  generates the selected bounded LOD tiles needed for a specific transition
  route directly from real source imagery or existing cell thumbnails without
  requiring the full persistent cell-thumbnail cache first; **initial
  route-scoped LOD compute exists, and `--workers N` can parallelize the
  generated tile's child blocks for larger LOD routes.**
- `world cache status` reports manifest generation, ready/missing tiles per LOD,
  pending work, the last real compute failure count/log path, and estimated
  remaining disk use on compute plans.

The all-library compute command must be runnable offline, without an already
running daemon, because a `world`-enabled daemon should refuse startup when the
cache is missing or stale. The command can live in `muralctl`, a dedicated
helper, or `murald --compute-world-cache`, but it must read the same config and
state paths as the daemon and print a useful progress indicator:

```text
indexed 63,964 wallpapers
cell thumbnails 18,240/63,964
world tiles L0 12/64  L1 48/256  L2 190/1024
estimated remaining 3.8 GiB
```

Progress should include at least:

- ordered library count and manifest status;
- thumbnail count ready/pending/failed;
- world tile count ready/pending/failed per LOD;
- current file or tile being generated when useful;
- estimated remaining disk use when practical.
- elapsed time and rate when practical.

A `world` transition should compute its required camera path and required cache
tiles before starting. If those real tiles are not ready, it should schedule
them and refuse rather than showing fake content.

### Startup/readiness gate

If any configured action or default transition references `world`, daemon
startup should validate that a usable indexed world cache exists before
reporting ready. If the cache is missing, invalid, has no real world tiles, or
was built with incompatible parameters, startup should fail with a direct command
to run, for example:

```text
murald: world transition is configured, but the world cache is not ready for
the current ordered library.
Run: muralctl world cache compute --scope all --background --progress
```

This startup error is intentional for a true cold cache. It prevents the system
from silently falling back to fake visuals after the user has opted into the
real `world` transition. It should not fire merely because the live wallpaper
library changed after a cache was built; stale indexed snapshots may still serve
routes that they cover. Non-world configurations should still start normally and
should not require a world cache.

## Camera/transition policy

The hardest UX issue is distance. In a 64k-wall library, two random shuffle-bag
picks may be thousands of cells apart.

Initial policy:

- choose the target by the requested wallpaper action;
- resolve current and target to canonical ordered-library cells;
- compute an overview camera that can show both positions with margin, up to a
  full-world overview if needed;
- zoom out from the current full-resolution wallpaper into the cached world;
- pan through cached real world tiles at the chosen overview/LOD;
- zoom into the target cell and hand off to the target full-resolution
  wallpaper.

For far jumps, the camera may zoom farther out and use lower-detail precomputed
world tiles so the transition remains bounded in time. It should not replace the
middle with streaks, fades, dimmed fake cells, or source/target-only animation.

Possible config later:

```toml
transition.world.max_direct_cells = 40
transition.world.overview_scale = 0.08
transition.world.thumbnail_edge = 384
transition.world.tile_edge = 1536
transition.world.required_lods = auto
transition.world.prefetch_margin = 2
transition.world.memory_cache_mb = 256
transition.world.fallback = push:up
```

The implemented fallback setting currently accepts only immediate low-risk
`cut` or `push:*` transitions. It is used when an existing world cache cannot
cover a requested route; it is not used for a true no-cache startup failure.

Review note: validate `overview_scale` and compressed travel against the
visible-cell budget, not only distance. A smaller scale makes jumps look more
global but can square the number of visible cells. **The current bounded
prototype validates routes by selected real tile count rather than raw cell
distance, so far jumps must pick a lower-detail real tile LOD before rendering.**

## Relation to current canvas modes

Do not run virtual world through `canvas.mode` internals. It should share only
small, well-named primitives:

- easing/interpolation helpers;
- rect/camera transform helpers;
- thumbnail cache building blocks if the cache API is generalized;
- world-cache manifest/tile primitives if they are genuinely generic;
- draw-in-rect GL adapter.

Avoid sharing these concepts with bounded canvas:

- bounded preview tile order;
- `paged`/`strip` walk semantics;
- span monitor-slot morphing;
- collage focus-pair spiral composition.

This separation is important because previous canvas breakage came from using
one helper for multiple concepts such as walk axis and pack axis.

## Implementation phases

### Phase 0: explicit enablement and cache contract

- Add the `world` transition/config spelling without rendering it yet.
- Add startup detection for configured `world` and return the intended
  "cache not ready; run compute command" error behind a temporary cache-ready
  predicate.
- Add `world cache status` and a dry-run `world cache compute --scope all`
  command that can read the ordered library offline and report planned work.
  **Initial CLI scaffold exists; full image generation does not.**
- Test the command against a synthetic 64k-entry library before adding GL work.

### Phase 1: pure world model

- Add a pure layout module, preferably outside `egl_render`:
  - `WorldSnapshot`; **pure ordered path/index snapshot exists in
    `mural-render` and is used by supervisor world route planning**
  - path/index lookup;
  - row-major index-to-rect; **initial scaffold exists in `mural-render`**
  - visible-cell query; **initial scaffold exists in `mural-render`**
  - cache-tile coverage query for a camera path; **initial rectangular route
    coverage exists in `mural-render`**
  - camera transform planning; **initial simple world camera path exists in
    `mural-render`**
- Unit tests:
  - stable index-to-cell mapping;
  - shuffle-bag order never changes world placement;
  - visible query clamps correctly at edges;
  - route cache coverage is computed for near and far jumps;
  - current/target rects remain stable for a snapshot;
  - multi-output focus records map to the expected world cells;
  - large-library visible query does not iterate all cells;
  - visible-cell budget is honored for extreme zoom-out. **Initial route tile
    budget selection tests cover near and extreme jumps.**

### Phase 2: disk world cache and precompute

- Add the world manifest format and cache versioning.
  **The cache index now also writes an ordered path snapshot file keyed by the
  manifest fingerprint, so stale-but-existing cache directories can still be
  used safely for routes whose endpoints remain in that snapshot.**
- Add per-wall thumbnail generation keyed by source identity.
  **Initial offline PNG cell-thumbnail generation exists.**
- Add world tile pyramid generation keyed by manifest/tile content.
  **Initial offline L0 and all-library deeper LOD tile generation exists, with
  tile directories keyed by ordered-library fingerprint and grid geometry and
  tile filenames keyed by covered cell cache keys to avoid stale tile reuse
  across library or source-content churn.**
- Add the offline full-library compute command with progress reporting.
  **All-library LOD compute and route-scoped LOD compute exist for bounded
  warmups; dry-run and generation progress now report ready/missing/planned
  tile counts with LOD labels and an estimated remaining byte count. Real
  progress runs also report elapsed time and work rate. Route-scoped LOD tiles
  are generated directly from real source imagery or existing cell thumbnails
  instead of forcing lower-LOD tile dependencies or persistent cell-thumbnail
  generation for route/neighborhood scopes. Neighborhood-scoped compute warms
  selected LOD tiles around one or more center paths for current/target
  prefetch. Real compute runs rewrite a failure log that status reports and can
  run as detached background
  jobs.**
- Add priority queues:
  - required route tiles; **missing explicit `world` routes now trigger a
    best-effort background route warmup through `muralctl world cache compute
    --scope route --background` only when the missing work is within the small
    automatic LOD0 budget, while still refusing the transition until real tiles
    exist. Larger route misses report the manual command without starting a long
    background job.**
  - current/target neighborhood tiles; **manual background neighborhood jobs
    exist through `muralctl world cache compute --scope neighborhood --center
    PATH --background`, and successful `world` transitions now schedule a
    best-effort radius-0 LOD0 current/target neighborhood warmup.**
  - near-future/prefetch tiles; **successful `world` transitions now include
    one upcoming shuffle candidate in the bounded radius-0 LOD0 neighborhood
    warmup.**
  - optional warm-all background. **Manual background all-library jobs exist
    through `muralctl world cache compute --scope all --background`.**
- Add fallback behavior for impossible or missing covered routes.
  **`transition.world.fallback = cut` or `push:*` now lets mural use an
  immediate non-world transition when an existing world cache cannot satisfy the
  requested route, while the route-scoped background warmup prepares a future
  retry. True no-cache startup still fails with the compute command.**
- Ensure cache misses never produce fake content in a `world` transition.
- Evaluate an EGL-thread upload budget for ready thumbnails so cache completion
  cannot cause one large frame hitch. **The bounded renderer now enforces both
  per-route and aggregate per-request world tile upload caps before starting a
  world transition.**
- Add tests for missing/deleted/quarantined source paths. **World-cache tests
  now cover deleted source stat errors plus stale manifest behavior after source
  delete or quarantine moves.**

### Phase 3: daemon transition plumbing

- Add protocol/config shape for the new transition.
- Add startup validation that rejects `world` configs when the world cache is
  missing or stale, with the exact compute command in the error message.
  **Initial startup validation exists and requires the all-library cache-ready
  status.**
- Build `WorldSnapshot` from high-level wallpaper actions after target is known.
  **Initial support also exists for explicit `set --transition world` by using
  current renderer health plus the requested target paths. Supervisor route
  planning now uses the pure `WorldSnapshot` path/index lookup.**
- Design the supervisor-to-renderer snapshot handoff explicitly, or replace that
  handoff if the world transition needs a better architecture. Do not assume
  sending the full library path list over the control socket is acceptable.
  **A compact first handoff exists with metadata plus per-output route indices;
  the renderer consumes it for bounded L0 routes.**
- Check required real cache coverage for the camera path before starting.
  **Initial supervisor-side route tile check exists for explicit high-level
  `world` requests; it selects a bounded LOD, rolls back staged state on
  missing/over-budget coverage, and hands covered routes to the renderer.**
- Start transition using current/target full-resolution textures plus ready
  world-cache tiles and cell thumbnails.
  **Initial renderer prototype starts bounded LOD world routes from
  `renderer_world_set`, including queued covered routes behind active animated
  transitions.**
- Keep rollback behavior identical to other animated transitions.

### Phase 4: EGL draw path

- Keep GL code as an adapter:
  - receive visible world tiles/cell thumbnails and textures;
  - scissor/draw rects;
  - avoid owning world layout policy.
- Add smoke tests/manual test checklist for:
  - small libraries;
  - large libraries;
  - cold cache readiness/warming refusal without fake middle frames;
  - fully warmed whole-library cache;
  - multi-output batches;
  - missing thumbnails;
  - current/target near edges;
  - very distant current/target;
  - renderer restart while world cache state exists.

Initial manual live checklist:

1. Record the current live state and verify the daemon is healthy:

   ```sh
   before=$(muralctl current --json)
   muralctl health --json
   ```

2. Exercise a cold or missing-cache high-level `world` request. It should exit
   non-zero, either report a bounded scheduled background route warmup or explain
   that automatic warmup was skipped by budget, include the manual route warmup
   command, and leave current wallpaper state unchanged:

   ```sh
   world_output=$(muralctl next --transition world --json 2>&1)
   world_exit=$?
   printf '%s\n' "$world_output"
   test "$world_exit" -ne 0
   test "$before" = "$(muralctl current --json)"
   ```

3. Dry-run the suggested route warmup before generating cache data:

   ```sh
   muralctl world cache compute --scope route \
     --route FROM_WALLPAPER TO_WALLPAPER \
     --dry-run --progress
   ```

4. For a known covered route, verify the renderer starts a real `world`
   transition and returns to the original wallpaper with `cut`:

   ```sh
   muralctl set --output OUTPUT=START_WALLPAPER --transition cut
   muralctl set --output OUTPUT=TARGET_WALLPAPER --transition world --duration-ms 120
   muralctl set --output OUTPUT=ORIGINAL_WALLPAPER --transition cut
   ```

5. Re-smoke existing transitions after every runtime-affecting world change:

   ```sh
   muralctl next --transition push:up --duration-ms 100 --json
   muralctl back --transition push:down --duration-ms 100 --json
   muralctl next --transition canvas --mode overlap \
     --canvas-zoom-out-ms 70 --canvas-pan-ms 70 --canvas-zoom-in-ms 70 --json
   muralctl back --transition canvas --mode overlap \
     --canvas-zoom-out-ms 70 --canvas-pan-ms 70 --canvas-zoom-in-ms 70 --json
   muralctl health --json
   ```

### Phase 5: viewer reuse

- Extract world snapshot/query/cache logic so a later viewer can reuse it.
- Add interactive pan/zoom inputs only after transition behavior is stable.

## Risks and mitigations

- **Huge random jumps look fake or disorienting.** Mitigate with max direct
  distance, real lower-detail world tiles, and bounded camera timing.
- **Cache churn on fast navigation.** Mitigate with visible-priority queue,
  request cancellation/coalescing, and a bounded in-memory texture cache.
- **Library changes shift positions.** Use per-transition snapshots and only
  rebuild world positions between transitions.
- **Path-order worlds shift after rescans.** V1 records the
  `path-snapshot-v1` order policy in the world manifest and writes a
  fingerprinted path snapshot. Existing snapshots can keep serving covered
  routes after library churn, but new paths need a fresh index/route warmup
  before they can appear in `world`. Add a persistent world-index table before
  treating positions as user-muscle-memory.
- **Full-library warm cache can consume many GB on disk.** Make warming optional
  and report estimates before doing it.
- **Cold cache cannot satisfy the real-world requirement.** Do not fake the
  transition. Fail startup for `world` configs when no usable tile cache exists.
  With an existing but incomplete/stale cache, use a configured immediate
  fallback when needed and schedule route precompute for a future retry.
- **Snapshot payloads can exceed control IPC limits.** Keep the renderer handoff
  compact or synced by generation; test large libraries before enabling the
  transition by default.
- **Texture upload bursts cause frame hitches.** Budget uploads on the EGL thread
  and require cache readiness before starting.
- **Mode coupling breaks existing canvas again.** Keep virtual world in separate
  modules and add contract tests before sharing helpers.

## Open questions

- After the per-output version, is a desktop-spanning group camera worth adding?
- What is the right default grid width for 10k-100k wallpapers?
- What are the right cell thumbnail, world tile, and LOD sizes for the real
  library?
