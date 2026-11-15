# Desktop packaging

Build the GUI and native helper together with `cargo build --release --locked -p ruscello-gui -p ruscello-webview`.
Run packaging commands at the repository root. Use the workspace version as the tag without adding a
prerelease suffix. Build on the target OS and architecture.

- Linux x86-64 uses Ubuntu 24.04 system dependencies from `scripts/ci-linux-deps.sh`. Install `dpkg-dev` and
  the pinned `appimagetool` from the release workflow, then run
  `APPIMAGETOOL=/path/to/appimagetool bash packaging/linux/package.sh v1.0.0`.
- On Windows x64, pass absolute `/DBIN=`, `/DOUT=` and `/DVERSION=` values to
  `makensis packaging/windows/ruscello.nsi` as shown in the workflow. The installer is per user
  and both binaries live in the same directory.
- On macOS, run `bash packaging/macos/package.sh v1.0.0` on Apple Silicon and Intel. The resulting disk image
  contains an ad hoc signed application bundle.

Outputs go to `dist/`. The release workflow builds the packages together, smoke-checks their contents,
and publishes all assets and SHA-256 sums when a stable `v*` tag matches the workspace version.
Release notes come from the Unreleased section of the changelog. Before pushing a tag, verify the CI
and packaging workflows on the commit being tagged. Package names are part of the updater contract.
