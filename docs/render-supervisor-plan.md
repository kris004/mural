# Wake-safe rendering and renderer supervision plan

Status: implementation plan, 2026-06-03.

This document preserves the context behind the renderer redesign so future work
does not collapse back into one-off suspend/wake mitigations.

## Confirmed failure evidence

Two separate suspend/wake freezes have now put `murald` inside Mesa's Wayland
EGL swap path while the daemon's public IPC stayed queued behind the same
single-threaded event loop:

1. Previous fixed freeze: `eglSwapBuffers -> wl_display_dispatch_queue` while
   rendering from a Wayland callback. That was fixed by deferring swaps until
   the outer event-loop pass.
2. Current freeze: `eglSwapBuffers -> wl_display_dispatch_queue` from the
   deferred outer-loop `render_pending_surfaces` path. That proved callback
   deferral alone was not sufficient.

In both cases `muralctl` timeouts were a symptom: the public IPC socket had
queued clients, but the daemon could not service them because rendering and
control-plane work shared one process and thread.

## Rendering policy

The EGL/Wayland client path should follow the same broad pattern used by mature
clients:

- Do not render or swap from Wayland protocol callbacks.
- Use `wl_surface.frame` callbacks only as animation pacing hints.
- Do not make startup, configure, cut, clear, IPC handling, or recovery depend
  on receiving a frame callback.
- Disable EGL's blocking swap throttle with `eglSwapInterval(0)` when murald is
  doing its own frame-callback pacing.
- Treat missing frame callbacks and output-power state as ordinary state-machine
  conditions, not as reasons to block the daemon.
- Treat EGL/context/surface errors as reasons to recreate renderer resources and
  preserve the last known wallpaper state.

The current `eglSwapInterval(0)` patch is therefore an intentional interim
rendering-policy change, not the complete fix. It removes a known blocking
throttle in Mesa's Wayland EGL path, but it does not by itself make public IPC
recoverable if a driver/EGL call never returns.

Useful source anchors:

- Khronos `eglSwapInterval` reference:
  <https://registry.khronos.org/EGL/sdk/docs/man/html/eglSwapInterval.xhtml>
- Weston `simple-egl` and `subsurfaces` examples:
  <https://gitlab.freedesktop.org/wayland/weston/-/blob/main/clients/simple-egl.c>
  <https://gitlab.freedesktop.org/wayland/weston/-/blob/main/clients/subsurfaces.c>
- Alacritty Wayland/EGL display setup:
  <https://github.com/alacritty/alacritty/blob/master/alacritty/src/display/mod.rs>
- mpv Wayland frame-wait and OpenGL/Vulkan presentation paths:
  <https://github.com/mpv-player/mpv/blob/master/video/out/wayland_common.c>
  <https://github.com/mpv-player/mpv/blob/master/video/out/opengl/context_wayland.c>
  <https://github.com/mpv-player/mpv/blob/master/video/out/vulkan/context_wayland.c>
- wpaperd Wayland/EGL wallpaper daemon:
  <https://github.com/danyspin97/wpaperd>
- Chromium multi-process and GPU-process architecture precedent:
  <https://www.chromium.org/developers/design-documents/multi-process-architecture/>
  <https://www.chromium.org/developers/design-documents/gpu-accelerated-compositing-in-chrome/>

## Target architecture

`murald` should default to a supervisor process and spawn a renderer child from
the same binary:

```text
systemd --user -> murald supervisor -> murald --renderer-child --renderer-fd FD
```

The supervisor owns:

- the public mural IPC socket;
- config loading;
- wallpaper library/history/favorites/quarantine state;
- high-level command planning and durable state commits;
- renderer process lifetime, restart policy, health, and diagnostics;
- systemd readiness/watchdog notifications.

The renderer child owns:

- Wayland connection and registry state;
- layer-shell surfaces and output metadata;
- EGL display/context/surfaces;
- GL textures/shaders;
- frame callbacks and animation queues;
- all draw, damage, and swap calls.

The private supervisor/renderer channel is inherited from the parent and is not
a public socket path. A stuck renderer can then be killed by the parent, which
lets the kernel close Wayland/EGL file descriptors and lets the compositor drop
that client cleanly.

## Transaction rule

For wallpaper actions that mutate durable wallpaper state:

1. Supervisor validates/plans the action with `mural-core`.
2. Supervisor sends the render transaction to the renderer child.
3. Renderer reports success only after it has accepted/displayed the request.
4. Supervisor commits durable wallpaper state only on renderer success.
5. On render failure, timeout, or child crash, supervisor rolls back quarantine
   moves when needed and does not commit the wallpaper transaction.

Explicit low-level renderer commands (`set`, `clear`, `preload`, cache control)
may remain renderer operations, but public IPC still goes through the supervisor
so `ping` and `health` remain responsive.

## Recovery and diagnostics

On renderer timeout, crash, or failed health check, the supervisor should:

1. Record the reason and renderer generation.
2. Try same-user debugger stack capture for the renderer child.
3. Send `SIGABRT` to request a systemd-coredump artifact.
4. Send `SIGKILL` if the child does not exit promptly.
5. Spawn a fresh renderer child.
6. Wait for renderer output readiness.
7. Restore the saved/current wallpaper layout with an immediate cut render.

Root should not be required for normal diagnostics because the renderer is a
same-user child of the supervisor.

## Completion criteria

This redesign is not complete if murald is left as only a thin forwarding proxy
whose child still owns public command semantics and durable wallpaper state. A
complete stage must leave the repo buildable, installable, and live-testable,
with supervisor-owned public IPC and wallpaper state plus renderer-owned
Wayland/EGL resources.

Acceptance checks:

- `cargo fmt --check`
- `cargo test`
- `cargo clippy --all-targets --all-features -- -D warnings`
- `git diff --check`
- live `muralctl ping`, `query`, and `health`
- live cut, push, and canvas transitions
- DPMS off/on test
- repeated suspend/resume tests where `muralctl ping` remains responsive
- forced renderer hang/crash test proving supervisor restart and wallpaper
  restoration
