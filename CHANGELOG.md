# Changelog

Format based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), following
[Semantic Versioning](https://semver.org/). Versions follow `0.<milestone>.x` until the 1.0 release.

## [Unreleased]

## [1.1.0]

### Added
- macOS bundle associations for `magnet:`, `.torrent`, and `.dlc`, using the same import flow as desktop startup arguments.
- Balanced, Limited connections, and Large files presets in Download Settings, with manual values and a scoped FuckingFast batch override. New desktop profiles start with Balanced.
- A default BitTorrent session budget of 500 peers, a 100-peer per-torrent default, and experimental TCP + µTP support in Torrent settings. TCP remains the default.

### Changed
- Smooth the displayed download and upload rates over a 2.5-second window independently of bandwidth limits.

### Fixed
- Keep the ordinary per-file connection limit when a host override redirects to an unrelated host and when it uses unrelated mirrors.
- Retransmit a lost µTP connection probe within the current attempt so a single dropped datagram does not trigger the peer reconnect backoff.
- Recover verified torrent pieces written after the last checkpoint and reject a saved completion claim when a sampled piece fails its hash check.
- Avoid reading unallocated regions of Windows sparse torrent files during resume when the expected piece hash requires nonzero data.
- Recover valid complete HTTP responses when a server ignores a matching conditional range or a partial probe cannot establish resumable framing. Preserve independent checksum verification and durable checkpoint ordering.
- Share the global download rate between HTTP and BitTorrent, with fair access across different request sizes. Use a negotiated fallback port when the default-managed BitTorrent listener is occupied, report occupied fixed ports and invalid interfaces, and honor PEX and local discovery independently.
- Resume saved torrent transfers after validation, with fresh peer discovery. In TCP + µTP mode, select the first transport that completes a valid BitTorrent handshake.
- Keep BitTorrent UDP discovery listening through temporary macOS address and routing errors.
- Keep the Windows BitTorrent µTP listener running when a peer disconnects and its old UDP port becomes unreachable.
- Require both sockets when TCP + µTP is selected. An occupied UDP port triggers the same automatic port selection or fixed-port error as an occupied TCP port.
- Remove the unmaintained Chrono dependency from the active graph while retaining DHT persistence, HLS date tags, and existing FTP resume validators.
- Process completed hoster and torrent jobs even when the transfer reaches completion between progress samples. Verify nested torrent manifests within the download root and accept TAR root directory markers.
- Keep exact download and package IDs in the Web UI and deduplicate normalized URLs within one submitted batch.
- Keep Web UI live updates connected when an earlier health request reports session state from before sign-in.
- Detect self-closing DASH representations, keep audio-only HLS renditions out of video selections, and match the audio selection shown during inspection to the downloaded rendition.
- Find assets on GitHub release pages, preserve extensionless text filenames, secure implicit FTPS data channels, and publish downloads to Windows long paths.
- Improve localized transfer and container errors, pairing layout, and macOS update rollback and startup registration.
- Report unsupported Windows BitTorrent interface binding explicitly and require the user to clear the setting before retrying over the system route.
- Start the Windows renderer in the main desktop process and preserve the saved renderer and explicit graphics backend selection.
- Keep the previous macOS app bundle until the replacement opens a window. Restore both executables if the replacement cannot run or exits during startup.
- Start intercepted downloads after Firefox or Chrome confirms cancellation of the native copy. Preserve browser downloads when filtering or handoff cannot complete safely.

### Validation
- Windows workspace regression tests passed. Native Linux and macOS runtime checks and BitTorrent validation across physical peers remain pending. The Windows hardware-renderer idle memory target remains open.

## [1.0.0]

The M9 feature set is on `main` via [PR #57](https://github.com/EmanueleFusco/ruscello/pull/57).

### Added
- GitHub Releases updater with automatic and manual checks, localized in-app release notes and progress,
  SHA-256 verification, checkpoint-safe shutdown, and restart on NSIS, AppImage, and macOS app bundles.
- Tagged desktop packages with the native browser helper and SHA-256 sums for Linux (`x86_64`/`aarch64` `.AppImage`, `.deb`, and Fedora `.rpm`), Windows (`x86_64`/`aarch64` NSIS), and macOS (`aarch64`/`x86_64` `.dmg`), plus an Arch Linux `PKGBUILD` and draft release publishing.
- Pre-release parser fuzzing, an offline mixed-transfer soak, resource-budget tooling, and a personal-path audit.
- Installation quickstart, FAQ, and coordinated vulnerability reporting policy.
- Ruscello application icon for Windows, macOS, and Linux, including the launcher and system tray.
- Ruscello Download Manager is the public name shown in the app, desktop launchers, and installers.
- M9.1–M9.8: Automation & Post-Processing (`ruscello-platform`, `ruscello-http`, `ruscello-engine`,
  `ruscello-api`, `ruscello-cli`, `ruscello-gui`):
  - Time-of-day scheduler (`ScheduleSettings`, `ScheduleRule`, `WeekdayMask`, injectable `Clock`) with
    speed-cap windows and automatic queue start/pause profiles, status bar indicator, and SQLite settings persistence.
  - Queue completion actions (`DoNothing`, `Notify`, `Sleep`, `Shutdown`) with cancellable countdown banner,
    notification/power backends across Windows/Linux/macOS (`SetThreadExecutionState`, `logind` D-Bus / `systemctl`,
    `IOPMAssertion` / `pmset`), and active-download OS sleep inhibition.
  - Sidecar checksum verification (`.sfv` CRC32, `.md5`, `.sha256` in GNU/BSD/bare formats with path-traversal
    rejection), automatic post-download archive extraction in pure Rust (`.zip` deflate/AES/ZipCrypto, `.7z` with
    password list, `.tar` / `.tar.gz` / `.tar.bz2` / `.tar.xz`, split `.001` parts), optional external `7z`/`unrar`
    sidecar extraction for `.rar` (documented in `docs/adr/0010-archive-extraction-and-os-automation.md`),
    multi-part package wait barrier, and optional archive cleanup.
  - Watch folder scanner importing `.torrent`, `.dlc`, and `.txt` batches with stability checks and
    configurable processed-file behavior (`MoveToProcessed`, `Delete`, `KeepTracked`).
  - Quote-safe post-download user command hook with `{file}`, `{dir}`, `{name}`, `{package}`, `{category}`, `{url}`,
    `{extracted_dir}` placeholders, `RUSCELLO_*` environment variables, and cancellation/timeout enforcement.
  - Plain FTP and explicit FTPS (`suppaftp` + `rustls`) transfers with `SIZE`, `PASV`, TLS upgrades (`AUTH TLS`,
    `PROT P`), and byte-offset resume via `REST`, redacting FTP passwords from logs, events, API responses,
    checkpoints, GUI displays, and SQLite persistence; plus localized `Settings -> Automation` UI in English and Italian.
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
- Added explicit `Stop` (`Status::Stopped`) semantics without removing items from the queue across `ruscello-core`, `ruscello-engine`, `ruscello-api`, and `ruscello-gui` (including stopping active `Seeding` torrents into `Completed` and resuming seeding on demand).
- Enabled right-click text context menus (`Cut`, `Copy`, `Paste`, `Select all`, `Clear`) on text fields and the Add dialog URL editor, plus a direct `Paste` button, row/header checkboxes, left-mouse drag selection across package groups, and a multi-selection action bar in `ruscello-gui`.
- Added draggable, persisted splitters for resizing the sidebar and details panel in `ruscello-gui`, themed the native Windows titlebar to match the active light/dark palette via DWM attributes, added subtle UI transition animations across interactive controls, and enforced single-line table cell alignment.
- Refined the mobile/LAN WebUI (`ruscello-api`) to inherit the desktop app's selected language and theme (`System`/`Light`/`Dark`), display the Ruscello application icon, align colors with the desktop palette, and expose `Stopped` status filtering and stop controls.
- Updated GitHub Actions checkout to use Node.js 24, upgraded the Fedora RPM build container to Fedora 44, and refreshed the README and UI guide for M5.
- Refined the desktop queue, Settings, and Add dialog with a blue accent, clearer toolbar icons, and
  short, optional selection and panel transitions. The Details heading toggles the panel and stays
  visible when collapsed. The tabs fade as the panel expands and collapses.
- Standardized desktop controls and scrollbars, aligned queue headers with their data columns,
  and kept advanced Add fields labeled while editing.
- Improved segmented throughput with bounded queued writes, faster connection ramp-up, and fewer redundant syncs.
- Isolated rotating CLI logs from the GUI and browser helper log retention.

### Known limitations
- On the tested AMD Lucienne Linux desktop, the default GPU renderer used 127 MB of RAM at idle.
  The 1.0 release accepts this result while retaining the 80 MB target. Software 2D used 46 MB in the
  same benchmark and is available in Settings.

### Fixed
- Preserved `Set-Cookie` headers across HTTP redirect hops in `ruscello-http`, detected HTML landing pages on generic URLs without failing prematurely, added generic XFS/countdown HTML recovery and interactive `VerificationRequired` fallback in `ruscello-resolver`, and improved `ruscello-webview` click responsiveness by focusing the WebView, routing same-site `new_window` navigations only during interactive verification (`View::Verify`), and disarming click-hijacking popunder overlays after native third-party popup blocking.
- Fixed download row filename/size overlap, clipped Italian sidebar/header labels (`Dimensione`, `Tempo stimato`, `In condivisione`, `Arrestati`), clipped `Download in corso` status label, and two-line simultaneous upload/download speed clipping in `ruscello-gui`.
- Cleaned up empty `.ruscello-incoming/<id>/.ready-<id>` staging directories after failed or stopped non-torrent transfers and during startup reconciliation while preserving resumable `.rscpart` checkpoints.
- Prevented third-party HTTP/2, GPU, and torrent `DEBUG` traces (such as `h2::codec::framed_read`) from bloating daily log files in `ruscello-platform`.
- Fixed `.dlc` container decryption when AES-CBC plaintext contains zero padding followed by PKCS#7 padding.
- Fixed `fuckingfast.co` resolution to handle `hx-post` endpoints returning `Hx-Redirect` URLs on `dl.fuckingfast.co` (including browser fallback clicking), removed the serial host limit on `fuckingfast.co`, and avoided treating passive Cloudflare `jsd/main.js` scripts on `200 OK` pages as interactive challenges.
- Fixed segmented HTTP range downloads failing at 100% when an HTTP/2 server sends an empty trailing `END_STREAM` chunk.
- Accepted segmented HTTP `206` responses whose `Content-Range` end reports the total resource size when both the start offset and `Content-Length` match the requested byte range.
- Handled XFileSharing `<download-countdown>` pages requiring verification by keeping the verification browser open through redirects until the challenge appears and handing captured downloads directly to the resolver.
- Preserve download staging until SQLite commits completion so a restart reuses the published
  file without downloading it again or creating a numbered duplicate.
- Browser cookie import sends multiple bounded requests when cookies from several profiles exceed a single
  helper request. Added regression coverage for idle exit, crash recovery, and a ten-link XFileSharing batch.
- Prevented the scheduler from treating a removed BitTorrent handle as a failed download while force recheck
  replaces it.
- Fixed a Windows race condition during BitTorrent force recheck where `librqbit`'s fastresume `.bitv` file could
  remain in `DELETE_PENDING` state while its background flusher finished syncing, causing re-add initialization
  to fail with access denied; also logged the underlying `librqbit` failure reason when a torrent errors.
