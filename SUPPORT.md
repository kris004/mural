# Support and bug reports

Mural is pre-1.0 alpha software and currently has no guaranteed support window.
GitHub issues are the public place for reproducible bugs, focused feature
requests, documentation problems, and compatibility reports.

Before opening an issue:

1. Read the relevant manual or documentation under `docs/`.
2. Reproduce with the current `main` branch when practical.
3. Run `muralctl health --json` and inspect the user journal:

   ```sh
   journalctl --user -u murald.service --since today
   ```

4. Check whether a direct `cut` works before diagnosing an animated transition.

A useful bug report includes:

- the Mural version or Git commit;
- compositor name and version;
- GPU and graphics-driver version;
- distribution and Rust version when the failure is build-related;
- monitor/output layout when relevant;
- exact reproduction steps, expected behavior, and actual behavior;
- the smallest relevant log excerpt and health response.

Mural health output and diagnostics can contain wallpaper paths, output names,
process IDs, and local state paths. Redact personal filenames, usernames, host or
network details, tokens, and unrelated log content before posting. Use synthetic
images when a reproducer needs an asset.

The `quarantine` command moves a wallpaper file into the configured quarantine
directory before selecting a replacement. Test file-moving behavior with copies,
not irreplaceable originals.

Do not report a vulnerability or suspected exploit in a public issue. Follow
[SECURITY.md](SECURITY.md) instead.
