# Desktop packaging

Build the GUI and native helper together with `cargo build --release --locked -p ruscello-gui -p ruscello-webview`.
Run packaging commands at the repository root. Use the workspace version as the tag without adding a
prerelease suffix. Build on the target OS and architecture.

- Linux (`x86_64` and `aarch64`) uses Ubuntu 24.04 system dependencies from `scripts/ci-linux-deps.sh`.
  Install `dpkg-dev` and the pinned `appimagetool` for the target architecture from the release workflow,
  then run `APPIMAGETOOL=/path/to/appimagetool bash packaging/linux/package.sh v1.0.0`.
- Fedora (`x86_64` and `aarch64`) builds inside Fedora 44 with `rpm-build`, `desktop-file-utils`, and the
  Fedora `-devel` packages, then runs `bash packaging/linux/package-rpm.sh v1.0.0`.
- Arch Linux (`x86_64` and `aarch64`) provides `packaging/arch/PKGBUILD` (kept in sync with the workspace
  version) for building from the tagged source archive with `makepkg -si`.
- On Windows (`x86_64` and `aarch64`), pass absolute `/DBIN=`, `/DOUT=` and `/DVERSION=` values to
  `makensis packaging/windows/ruscello.nsi` as shown in the workflow. The installer is per user
  and both binaries live in the same directory.
- On macOS, run `bash packaging/macos/package.sh v1.0.0` on Apple Silicon and Intel. The resulting disk image
  contains an ad hoc signed application bundle.

Outputs go to `dist/`. The release workflow builds all ten packages together, smoke-checks their contents,
and creates a draft GitHub release with all assets and SHA-256 sums when a stable `v*` tag matches the
workspace version. Release notes come from the Unreleased section of the changelog. Before pushing a tag,
verify the CI and packaging workflows on the commit being tagged. Package names are part of the updater contract.
