# Changelog

Format based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), following
[Semantic Versioning](https://semver.org/). Versions follow `0.<milestone>.x` until the 1.0 release.

## [Unreleased]

### Added
- M8.1–M8.4: Local loopback API server & LAN Remote Web UI (`ruscello-api`, `axum`), integrated into
  `ruscello-gui` and `ruscello-cli` (`queue`, `status`), with dual listeners (`127.0.0.1` Bearer auth +
  strict extension-origin CORS and discovery file, plus dynamically toggleable `0.0.0.0:<port>` LAN mode with
  optional passwordless access or salted PBKDF2-HMAC-SHA256 + `HttpOnly` cookie sessions and per-IP rate limiting).
- REST, WebSocket (`/api/v1/ws`), and SSE (`/api/v1/events`) telemetry endpoints for queue/packages/speeds,
  pause/resume/cancel (single or bulk), adding URLs/magnets/`.torrent`/`.dlc`, toggling alternative speed limits (`🐢`),
  LinkGrabber page analysis, domain cookie/header forwarding, and one-click browser extension pairing.
- Responsive mobile-friendly LAN Web UI embedded directly into the Rust binary (localized in English and Italian),
  with live LAN status, copyable/openable URL, scannable QR code, and pairing controls in Settings -> Integrations.
- Manifest V3 browser extension (`extensions/browser`) for Chrome/Chromium and Firefox (Gecko) with optional
  download interception (MIME type and minimum file size filters), localized `en`/`it` context menus
  ("Download with Ruscello" / "Analyze page with LinkGrabber"), automatic domain cookie forwarding, and one-click pairing.
- M7.1–M7.6: Native HLS (`m3u8-rs`) and MPEG-DASH (`quick-xml`) media download engine (`ruscello-media`)
  supporting master playlist quality selection, parallel segment downloads with global/per-download rate limits,
  AES-128-CBC decryption, segment-level checkpoint resume, and deterministic output concatenation.
- Optional, consent-gated sidecar management for official `yt-dlp` GitHub releases and static `ffmpeg` builds
  with SHA-256 checksum verification, periodic auto-update, and custom executable path overrides.
- `yt-dlp -J` metadata/format/playlist inspection with hybrid native HTTP/HLS/DASH or `yt-dlp` progress-parsed
  execution, temporary private Netscape cookie export, `ffmpeg` audio+video remuxing (`Status::PostProcessing`),
  and multi-item playlist/channel grouping into named packages across the engine, CLI (`get --format`,
  `inspect`), and desktop GUI (Add dialog format/playlist pickers and Settings -> Integrations sidecar controls
  localized in English and Italian).
- M6.1–M6.6: On-demand native browser helper with a persistent profile. A user-completed verification
  resumes waiting downloads for that site. Browser recipes block popups and hand file transfers to the
  HTTP engine. LinkGrabber can discover browser resources, and Integration settings allow site-scoped
  cookie import from Firefox or readable Chromium profiles. The desktop and CLI support English and Italian.
- M5.1–M5.7: Hoster and page resolution, reloadable TOML recipes, and renewal of expired links and cookies
  while preserving HTTP checkpoints with matching validators. The desktop LinkGrabber supports scoring
  explanations, filters, selection, and named package imports in English and Italian.
- Native DLC decryption, text/playlist/Metalink import, and hoster API tokens in the OS credential store.
  The CLI can inspect pages, list imported URLs, and download recognized hoster links.
- M4.1–M4.6: Full BitTorrent support powered by a single `librqbit` session (`ruscello-torrent`) using
  `rust-tls`, DHT/PEX/LSD/UPnP/trackers, fastresume state in the data directory, pre-add `.torrent`/magnet
  inspection with file selection, download/upload/seed-ratio/seed-time/max-peers limits, network interface
  binding and eMule/P2P IP blocklist filtering, force piece recheck, unified queue & SQLite persistence, and
  contextual GUI details tabs (`Pieces`, `Files`, `Peers`, `Trackers`), status bar DHT telemetry, and
  BitTorrent settings (localized in English and Italian).
- Optional per-app NVIDIA fixed refresh on Windows, with existing-profile detection and restore, for VRR flicker
  while using the hardware renderer.
- M3.1 and M3.7: Native desktop queue with virtualized rows, search, filters, persisted details and columns,
  system-aware light and dark themes, bundled Inter and Lucide fonts, and live English and Italian switching.
- M3.2 and M3.3: Add dialog with HTTP previews, clipboard and text-file imports, destination and category
  selection, named batches, per-download connection and bandwidth limits, expected checksums, and
  multi-selection actions with keyboard shortcuts.
- M3.4: Settings navigation, persistent GPU/software renderer selection, download paths and speed limits,
  LAN preferences with a QR code and an OS-keyring password, diagnostic details, and GitHub release
  checks and notes.
- M3.5 and M3.6: System tray, desktop notifications, login launch, single-instance forwarding, and
  per-user magnet, Torrent, and DLC associations on Windows and Linux.
- M3.8: Headless desktop workflow tests and view snapshots using iced_test and the software renderer.
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
- Updated GitHub Actions checkout to use Node.js 24 and refreshed the README and UI guide for M5.
- Refined the desktop queue, Settings, and Add dialog with a blue accent, clearer toolbar icons, and
  short, optional selection and panel transitions. The Details heading toggles the panel and stays
  visible when collapsed. The tabs fade as the panel expands and collapses.
- Standardized desktop controls and scrollbars, aligned queue headers with their data columns,
  and kept advanced Add fields labeled while editing.
- Improved segmented throughput with bounded queued writes, faster connection ramp-up, and fewer redundant syncs.
- Isolated rotating CLI logs from the GUI and browser helper log retention.
- Moved FTP/FTPS support from M1.11 to M9.8.

### Fixed
- Prevented the scheduler from treating a removed BitTorrent handle as a failed download while force recheck
  replaces it.
- Fixed a Windows race condition during BitTorrent force recheck where `librqbit`'s fastresume `.bitv` file could
  remain in `DELETE_PENDING` state while its background flusher finished syncing, causing re-add initialization
  to fail with access denied; also logged the underlying `librqbit` failure reason when a torrent errors.
