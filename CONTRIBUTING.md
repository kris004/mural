# Contributing to mural

Mural is an early-stage Linux Wayland project. Bug fixes, portability work,
documentation, tests, and focused transition improvements are welcome. For a
large protocol, renderer, or extension-system change, open an issue first so the
compatibility and failure model can be agreed before substantial implementation.

## Development setup

You need Rust 1.95 or newer, Cargo, `pkg-config`, Wayland client/Wayland EGL
development files, xkbcommon development files, and EGL/OpenGL ES development
files.

```sh
git clone https://github.com/kris004/mural.git
cd mural
cargo build --workspace --locked
cargo test --workspace --locked
```

A live renderer test additionally needs a compositor that implements
`wlr-layer-shell`. Use a temporary socket and throwaway images rather than
replacing an installed service while developing:

```sh
install -d -m 700 "$XDG_RUNTIME_DIR/mural-dev"
cargo run -p murald -- \
  --socket "$XDG_RUNTIME_DIR/mural-dev/mural.sock"
```

In another terminal:

```sh
cargo run -p muralctl -- \
  --socket "$XDG_RUNTIME_DIR/mural-dev/mural.sock" ping
cargo run -p muralctl -- \
  --socket "$XDG_RUNTIME_DIR/mural-dev/mural.sock" capabilities
cargo run -p muralctl -- \
  --socket "$XDG_RUNTIME_DIR/mural-dev/mural.sock" query --json
```

## Workspace layout

- `mural-ipc`: public/internal wire types, the compiled-in transition registry,
  capability schema, and JSON encoding/decoding.
- `mural-core`: config, wallpaper-library state, action planning, and cache policy.
- `mural-render`: deterministic transition/layout math without Wayland or GL.
- `murald`: supervisor, renderer child, Wayland surfaces, asset pipeline, and EGL.
- `muralctl`: command-line parsing, transport, cache tools, and output formatting.
- `docs/man`: installed reference manuals.

Start with [the architecture guardrails](docs/architecture.md). Transition work
must also follow [the transition authoring guide](docs/transition-authoring.md).
Named transition profiles tune existing implementations; they are not runtime
plugins.

## Validation

Run the narrowest relevant tests while iterating. Before submitting a pull
request, run:

```sh
cargo fmt --all -- --check
cargo test --workspace --all-targets --locked
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
RUSTDOCFLAGS='-D warnings' cargo doc --workspace --no-deps --locked
mandoc -T lint docs/man/mural.7
mandoc -T lint docs/man/murald.1
mandoc -T lint docs/man/muralctl.1
mandoc -T lint docs/man/mural-config.5
git diff --check
```

Renderer, transition, output, or service changes also need a focused live smoke
test on a supported compositor. Describe the compositor, GPU/driver, exercised
commands, and result in the pull request. When resource ownership changes make a
leak plausible, include a targeted Valgrind run around the affected command or
daemon path when practical.

Do not make ordinary `cut` or idle rendering slower to support an animated path.
Preserve the current wallpaper after decode, upload, planning, or first-frame
setup failures. A deferred renderer failure after acknowledgement must preserve
supervisor/renderer state consistency and queued committed work.

## Documentation and compatibility

A user-visible CLI, config, IPC, transition, file-location, or service change
must update the corresponding README, Markdown reference, `--help` text, and man
page in the same pull request. Prefer additive protocol changes. Call out any
migration requirement explicitly.

Examples and tests must use portable paths and synthetic or clearly licensed
assets. Never commit credentials, private URLs, personal logs, machine-specific
identifiers, or unnecessary home/network details.

## Pull requests

- Keep each change focused and explain both what changed and why.
- Add tests for non-trivial parsing, planning, timing, layout, or failure logic.
- Avoid unrelated formatting or cleanup.
- Include rollback or compatibility notes for changes that affect installed
  services, state, cache formats, or the public protocol.
- Be respectful and assume good faith in review.

Unless explicitly stated otherwise, contributions intentionally submitted for
inclusion in Mural are dual-licensed under MIT OR Apache-2.0, without additional
terms or conditions, matching the project license.
