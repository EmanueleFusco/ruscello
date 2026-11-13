# Changelog

Format based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), following
[Semantic Versioning](https://semver.org/). Versions follow `0.<milestone>.x` until the 1.0 release.

## [Unreleased]

### Added
- M3.1 and M3.7: Native desktop queue with virtualized rows, search, filters, persisted details and columns,
  system-aware light and dark themes, bundled Inter and Lucide fonts, and live English and Italian switching.
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
- M1.9–M1.10: Mirror scheduling and MD5, SHA-1, SHA-256, and CRC32 verification before publishing a download.
- M1.12: Bilingual developer CLI with terminal progress, transfer options, and a reproducible aria2 benchmark.
- M2.1: Shared download, package, source, category, tag, and priority types with a validated download lifecycle.
- M2.2: SQLite queue persistence with versioned WAL migrations, batched writes, automatic backups, and
  reconciliation with HTTP checkpoints after restart or database corruption.
- M2.3–M2.4: Persistent queue scheduling with priority, manual ordering, force start, retry and connection limits,
  automatic startup resume, and an asynchronous engine handle with aggregated progress events.
- M2.5–M2.6: Automatic file types, configurable category rules and limits, a separate temporary directory option,
  and checkpoint-preserving pauses when available disk space is low.

### Changed
- Improved segmented throughput with bounded queued writes, faster connection ramp-up, and fewer redundant syncs.
- Isolated rotating CLI logs from the GUI and browser helper log retention.
- Moved FTP/FTPS support from M1.11 to M9.8.
