# Packaging and installation

Mural is currently distributed from source. It targets Linux and requires a
Wayland compositor with `wlr-layer-shell` plus a working EGL/OpenGL ES 2 stack.

## Versioned source

Release tags use `vMAJOR.MINOR.PATCH` and match the single version declared in
the root workspace manifest. For a release `X.Y.Z`, the source archive is:

```text
https://github.com/kris004/mural/archive/refs/tags/vX.Y.Z.tar.gz
```

Release tags are permanent and must not be moved or reused. Maintainers must
enable GitHub immutable releases before publishing a release so its tag and
attached assets are locked. Package released tags rather than the mutable
default branch.

The checked-in `Cargo.lock` is part of the release input and must not be
regenerated during a package build. Use Cargo's `--locked` or stricter
`--frozen` mode and provide all registry crates through the package manager's
offline source mechanism.

## Dependencies

Build-time requirements:

- Rust 1.95 or newer and Cargo;
- `pkg-config`;
- Wayland client and Wayland EGL development files;
- xkbcommon development files.

Runtime requirements:

- `libwayland-client` and `libwayland-egl`;
- an EGL/OpenGL ES driver;
- a layer-shell-capable Wayland compositor.

`vipsthumbnail` is an optional canvas/world cache accelerator. Mural falls back
to its internal image path when it is unavailable. `gdb` is optional and used
only for best-effort renderer diagnostics.

## User-local install

The default Makefile prefix is `~/.local`:

```sh
make install
make enable-service
muralctl ping
```

`make install` builds and copies files but never contacts systemd. The service
activation target, `make enable-service`, reloads the systemd user manager,
enables the unit, and starts it. The installed files are:

- `~/.local/bin/murald`
- `~/.local/bin/muralctl`
- `~/.local/share/man/man7/mural.7`
- `~/.local/share/man/man1/murald.1`
- `~/.local/share/man/man1/muralctl.1`
- `~/.local/share/man/man5/mural-config.5`
- `${XDG_DATA_HOME:-$HOME/.local/share}/systemd/user/murald.service`
- `~/.local/share/doc/mural/examples/config`
- `~/.local/share/licenses/mural/LICENSE-APACHE`
- `~/.local/share/licenses/mural/LICENSE-MIT`

To stop the live service and remove the user-local files:

```sh
make disable-service
make uninstall
make reload-service
```

The separate steps are intentional: file installation/removal remains safe for
staged package builds and never contacts a live user manager.

## Staged package install

Packagers can override the standard paths and stage with `DESTDIR`:

```sh
make \
  DESTDIR="$pkgdir" \
  PREFIX=/usr \
  BINDIR=/usr/bin \
  MANDIR=/usr/share/man \
  DOCDIR=/usr/share/doc/mural \
  SYSTEMD_USER_DIR=/usr/lib/systemd/user \
  install
```

The generated unit records `BINDIR` without `DESTDIR`, so the example produces
`ExecStart=/usr/bin/murald` inside the package while writing files beneath
`$pkgdir`. `CARGO_TARGET_DIR` is honored when a package build keeps Cargo
artifacts outside the source tree. Do not call `enable-service`,
`disable-service`, `restart-service`, or `reload-service` from a package build.

All workspace crates currently have `publish = false`; Mural has not committed
to separately versioned crates.io libraries or a stable Rust API.

## Manual build and run

```sh
cargo build --release --locked
./target/release/murald
```

The binaries are `target/release/murald` and `target/release/muralctl`. Systemd
is optional. A compositor or session manager may start `murald` directly after
its Wayland environment is ready.

## Ad-hoc test session

Use a temporary socket so a development run does not replace an installed
service:

```sh
install -d -m 700 "$XDG_RUNTIME_DIR/mural-test"
cargo run -p murald -- \
  --socket "$XDG_RUNTIME_DIR/mural-test/mural.sock"
```

From another shell:

```sh
cargo run -p muralctl -- \
  --socket "$XDG_RUNTIME_DIR/mural-test/mural.sock" ping
cargo run -p muralctl -- \
  --socket "$XDG_RUNTIME_DIR/mural-test/mural.sock" query --json
cargo run -p muralctl -- \
  --socket "$XDG_RUNTIME_DIR/mural-test/mural.sock" set \
  --output DP-1 /path/to/wallpaper.jpg --transition cut
cargo run -p muralctl -- \
  --socket "$XDG_RUNTIME_DIR/mural-test/mural.sock" set \
  --output DP-1 /path/to/next.jpg --transition push:left
cargo run -p muralctl -- \
  --socket "$XDG_RUNTIME_DIR/mural-test/mural.sock" stop
```

Discover real output names with `muralctl query --json`; `DP-1` above is only an
example.

## systemd user unit

`dist/systemd/murald.service.in` is a compositor-neutral template. The Makefile
substitutes the configured binary directory and installs the result as
`murald.service`.

The unit:

- joins `graphical-session.target` rather than a compositor-specific target;
- starts only when `WAYLAND_DISPLAY` exists in the systemd user manager
  environment;
- uses `Type=notify` and waits for the initial wallpaper display before
  reporting ready;
- enables a 30-second watchdog and restarts after renderer/supervisor failure.

Not every standalone compositor activates `graphical-session.target` or imports
its Wayland environment automatically. In that case, integrate the unit with the
compositor's session target/startup, or run `murald` directly. Do not claim a
compositor as supported until startup, output discovery, cut, an animated
transition, power changes, and shutdown have been exercised there.

Use a user-service drop-in with `WatchdogSec=0` while investigating a hang that
must remain stuck for inspection.
