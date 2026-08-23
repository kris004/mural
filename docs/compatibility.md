# Compatibility matrix

This matrix records the support claims Mural can make today. A compositor is
not considered supported merely because it implements Wayland: Mural needs
`zwlr_layer_shell_v1`, Wayland EGL, and an EGL/OpenGL ES 2 driver.

| Compositor or environment | Status | What is covered | What is not claimed |
| --- | --- | --- | --- |
| Sway | **Primary tested target** | Development and integration testing of output discovery, persistent layer-shell surfaces, cut, animated transitions, IPC, and shutdown. | No version-independent guarantee; GPU/driver behavior still matters. |
| Other wlroots compositors with layer-shell | **Potentially compatible; unverified** | The protocol and rendering path are intended to be compositor-neutral. | No release-tested matrix for output power, hotplug, session startup, or EGL quirks. |
| Weston | **Unverified** | May work if the selected shell/backend exposes the required layer-shell and EGL interfaces. | Do not report Weston support without exercising the exact backend and version. |
| GNOME/Mutter | **Unsupported** | None. | Mutter does not provide the required wlr-layer-shell protocol. |
| Headless or nested compositors | **Development-only** | Useful for protocol and startup experiments when the compositor exposes all required interfaces. | A headless smoke test does not establish real GPU, output-power, suspend, or multi-monitor support. |

## Minimum compatibility test

When adding a compositor or GPU/driver combination, record its versions and
run the following sequence from an active session:

1. Start Mural and confirm `muralctl ping`, `health --json`, and `query --json`.
2. Display a known PNG or JPEG with `--transition cut`.
3. Exercise `fade`, one `push` direction, and one library `canvas` transition.
4. Verify a second output if available, then test output removal/addition.
5. If supported by the compositor, power an output off and on again.
6. Stop and restart Mural, confirming that saved wallpapers are restored.
7. Record the compositor, graphics stack, driver, GPU, output topology, and
   Mural commit in the report. Redact home paths and usernames.

Results from this checklist should be added here rather than broadening the
support claim based only on protocol inspection.
