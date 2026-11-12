# Changelog

Format based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), following
[Semantic Versioning](https://semver.org/). Versions follow `0.<milestone>.x` until the 1.0 release.

## [Unreleased]

### Added
- M0: Cargo workspace structure (crates `core`, `http`, `torrent`, `media`, `resolver`, `browser`,
  `storage`, `engine`, `api`, `testkit`; apps `gui`, `cli`, `webview`), work plan, ADRs, UI and testing
  specifications, GitHub Actions CI, `cargo-deny` configuration, and `AGENTS.md`.
- M0.1: Shared platform paths, daily rotating application logs, typed startup errors, and an explicit
  workspace build check on all three CI platforms.
- M1.1: Loopback HTTP testkit with deterministic payloads, configurable server faults, and request recording.
- M1.2: HTTP metadata probing for file size, byte ranges, filenames, redirects, and validators.
- M1.3: Pure byte-range allocation with work stealing and adaptive connection limits.
- M1.4: Buffered positional file writer with preallocation and collision-safe publication.
- M1.5: Versioned, checksummed HTTP download checkpoints and validator-checked resume after process termination.
- M1.6–M1.8: Single-connection fallback, reconnect and retry handling, an expired-link resolver hook, and runtime
  global and per-download bandwidth limits.
