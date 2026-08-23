# mural

Mural is a GPU-accelerated, scriptable wallpaper daemon for Linux Wayland
compositors that support `wlr-layer-shell`.

It keeps one persistent background surface per output, renders transitions with
EGL/OpenGL ES, and exposes a small command-line and JSON IPC control plane for
shell scripts, keybindings, bars, and launchers.

![Synthetic push transition rendered by Mural](docs/assets/mural-demo.gif)

This privacy-safe demo was rendered from generated gradients in a headless Sway
session; it is not a capture of a user's desktop. See the
[compatibility matrix](docs/compatibility.md) before treating a compositor or
GPU combination as supported.

> [!WARNING]
> Mural is pre-1.0 alpha software. Sway is the primary tested compositor. Other
> layer-shell compositors may work, but they are not yet release-tested. See the
> compatibility matrix for the current evidence. GNOME/Mutter is not supported
> because it does not expose the required layer-shell protocol.

## Features

- Per-output JPG, PNG, and WebP wallpapers with `fill`, `fit`, `center`, and
  `stretch` scaling.
- Immediate `cut` changes plus configurable GPU-rendered `fade`, `push`,
  `canvas`, and experimental cache-backed `world` transitions.
- Per-command transition, direction, duration, easing, mode, output, and scale
  overrides.
- Native directory navigation, layout history, favorites, quarantine, and
  weighted shuffle state.
- Script-friendly `muralctl` commands, compact JSON query/health output, and
  daemon-reported transition capabilities.
- A supervisor/renderer process boundary that isolates Wayland/EGL failures and
  restores saved wallpapers after the renderer restarts.
- Output-power awareness when the compositor provides wlroots output-power
  management.
- XDG-aware configuration, state, runtime socket, and systemd user-service
  locations.

## Requirements and compatibility

Mural currently targets Linux. The compositor must provide `wl_compositor`,
`wl_output`, and `zwlr_layer_shell_v1`. Output-power management is optional.

Build requirements:

- Rust 1.95 or newer and Cargo;
- `pkg-config`;
- Wayland client and Wayland EGL development files;
- xkbcommon development files;
- EGL and OpenGL ES development files.

Runtime requirements:

- a layer-shell-capable Wayland compositor;
- Wayland client/Wayland EGL libraries;
- a working EGL/OpenGL ES 2 driver.

Optional helpers:

- `vipsthumbnail` for faster persistent thumbnail-cache generation;
- `gdb` for best-effort renderer diagnostics after a hang.

Mural does not currently claim support for every compositor or GPU stack. The
[compatibility matrix](docs/compatibility.md) explains the current support
boundary and test bar. Please include the compositor, GPU, driver, and Mural
version when reporting a problem.

## Install from source

Clone the repository and run:

```sh
cargo test --workspace --locked
make install
```

The default user-local install places binaries in `~/.local/bin`, manual pages
under `~/.local/share/man`, and the systemd user unit under
`${XDG_DATA_HOME:-$HOME/.local/share}/systemd/user`. `PREFIX`, `BINDIR`,
`MANDIR`, `DOCDIR`, `SYSTEMD_USER_DIR`, and `DESTDIR` can be overridden for
packaging.

Create a wallpaper directory and install the sample configuration:

```sh
config_home="${XDG_CONFIG_HOME:-$HOME/.config}"
mkdir -p "$HOME/Pictures/wallpapers" "$config_home/mural"
cp examples/config "$config_home/mural/config"
```

Then reload the user manager and start Mural from an active graphical session:

```sh
systemctl --user daemon-reload
systemctl --user enable --now murald.service
```

The supplied unit is attached to `graphical-session.target` and requires
`WAYLAND_DISPLAY` in the user manager environment. Compositors or session
managers that do not activate that target should start `murald.service` from
their own session startup. Running `murald` directly is also supported; systemd
is not required.

See [the packaging guide](docs/packaging.md) for staged installs, uninstalling,
and service integration details.

## Quick start

Check the daemon and discover output names:

```sh
muralctl ping
muralctl capabilities
muralctl health --json
muralctl query --json
```

Navigate the configured wallpaper library:

```sh
muralctl next
muralctl back
muralctl shift forward
muralctl shift back
muralctl current
```

Set explicit paths on a named output:

```sh
muralctl set --output DP-1 /path/to/wallpaper.jpg --transition cut
muralctl set --output DP-1 /path/to/next.jpg --transition fade
muralctl set --output DP-1 /path/to/next.jpg --transition push:left
```

Run `muralctl COMMAND --help` for command-specific options. Installed reference
manuals are available as `mural(7)`, `murald(1)`, `muralctl(1)`, and
`mural-config(5)`.

## Transitions

| Transition | Scope | Status |
| --- | --- | --- |
| `cut` | explicit paths and library actions | Stable fast path; replaces immediately and clears queued work. |
| `fade` | explicit paths and library actions | Built in; blends the complete old and new scenes, including letterboxes and image transparency. |
| `push:DIR` | explicit paths and library actions | Built in; `DIR` is `up`, `down`, `left`, or `right`. |
| `canvas` | library actions | Built in; zooms through a bounded preview layout derived from history and shuffle state. |
| `world` | configured-library paths and library actions | Experimental; the current and target must be indexed, with real precomputed cache coverage for the route. |

Push supports `portal`, `screen`, and experimental `pan` modes. Canvas supports
`clipped`, `morph`, `overlap`, `collage`, and `span` layouts. `collage` and
`span` require the `strip` canvas walk.

Examples:

```sh
muralctl next --transition fade --duration-ms 500
muralctl next --transition push:up --duration-ms 700
muralctl next --transition canvas --mode morph
muralctl next --transition canvas --mode collage --canvas-walk strip
```

Scripts and integrations should query the running daemon rather than duplicate
this table:

```sh
muralctl capabilities
muralctl capabilities --json
```

Capability schema version 1 reports the protocol version, compiled-in
daemon mode, transition class, effective request scopes, stability, runtime
requirements, and typed parameters. It describes compiled support and endpoint
availability; runtime requirements such as world-cache coverage are still
checked when a transition starts.

`world` is deliberately cache-gated instead of drawing synthetic intermediate
content. Inspect and prepare its cache with:

```sh
muralctl world cache status
muralctl world cache compute --scope all --background --progress
```

## Configuration

Mural reads `$MURAL_CONFIG`, then `$XDG_CONFIG_HOME/mural/config`, then
`~/.config/mural/config`. Environment variables override matching file values.

A minimal configuration is:

```ini
wall_dir = ~/Pictures/wallpapers
scale_mode = fill

transition.push.duration_ms = 900
transition.push.easing = ease-out-cubic
transition.push.mode = portal

action.next = push:up
action.back = push:down
action.shift_forward = push:left
action.shift_back = push:right
action.replace = cut
action.quarantine = cut
action.startup = cut
```

See [`examples/config`](examples/config) for a fuller safe default and
`mural-config(5)` for every key. The wallpaper library is intentionally scanned
and watched only at its top level.

## Extending mural

Named transition profiles customize built-in transition kinds; they are not a
runtime plugin system. In the current alpha, adding a new transition means
contributing Rust code and rebuilding Mural. A compiled-in registry centralizes
public names, classes, scopes, parameters, requirements, and capability output.
Pairwise effects such as fade and push share decode, queue, acceleration,
rollback, and texture-ownership machinery; scene transitions such as canvas and
world keep their additional planning paths. Pure math and EGL drawing remain
separate so new effects can be tested without weakening those lifecycle rules.

There is no stable runtime shader-package format yet. Native dynamic-library
plugins are not planned: a future versioned, validated shader interface inside
the supervised renderer would be safer than loading arbitrary native code.

See [the transition authoring guide](docs/transition-authoring.md) for the
current source-level checklist and [the architecture guide](docs/architecture.md)
for the invariants new effects must preserve.

## Known limitations

- Static JPG, PNG, and WebP images only; animated images and video are not
  supported.
- EXIF orientation is not applied yet.
- The library watcher is non-recursive.
- Immediately-started images can still decode synchronously when they were not
  prepared ahead of time.
- There is no reusable full-resolution texture cache or stable runtime effect
  plugin API yet.

See [the roadmap](docs/next-steps.md) for active work. Historical design context
is retained in [PLAN.md](PLAN.md) and the longer design documents under
[`docs/`](docs/).

## Contributing and support

Contributions are welcome. Read [CONTRIBUTING.md](CONTRIBUTING.md) before
opening a pull request. For help or a bug report, follow [SUPPORT.md](SUPPORT.md)
and redact local paths or other private data from logs. Report security issues
using [SECURITY.md](SECURITY.md).

## License

Mural is available under either the [Apache License 2.0](LICENSE-APACHE) or the
[MIT license](LICENSE-MIT), at your option.
