# Ruscello — Work Plan

> Desktop download manager written **entirely in Rust** that combines a BitTorrent client (qBittorrent-style)
> and an HTTP/hoster download manager (JDownloader / IDM-style), featuring a minimal, professional user interface.
>
> This document is the **single source of truth** for project progress. Each milestone defines goals, tasks,
> acceptance criteria ("Definition of Done"), and required tests. Milestones map to *GitHub Milestones*;
> major tasks map to *GitHub Issues*. Update checkboxes as tasks are completed (see `AGENTS.md`).
>
> **Language Rule:** All development documentation, code, comments, commits, issues, and pull requests must be
> in **English**. The application itself must support **both English (`en`) and Italian (`it`)**, with a setting
> allowing the user to choose their preferred language at any time.

---

## 0. Requirements (from Project Owner)

| # | Requirement | Where Satisfied |
|---|-------------|-----------------|
| R1 | BitTorrent support (magnet links, `.torrent` files, file selection, seeding) | M4 |
| R2 | HTTP/HTTPS/FTP downloads from "as many sites as possible" | M1, M5, M6, M7 |
| R3 | Pause, resume, and cancel downloads | M1, M2 |
| R4 | Zero lost downloads on PC power-off or connection drop (crash-safe resume) | M1 (core invariant) |
| R5 | Maximum speed: dynamic segmentation, multi-connection, mirrors, connection reuse | M1, M10 |
| R6 | Maximum automation (queues, retries, reconnection, post-processing, rules) | M2, M9 |
| R7 | Page analysis → list of discovered content highlighting likely target files (LinkGrabber) | M5 |
| R8 | Pages protected by anti-bot checks (Cloudflare, etc.) | M6 — **human-in-the-loop session reuse**, see ADR-0004 |
| R9 | Pleasant, minimal, professional UI; downloads organized by file type and category | M3 + `docs/UI.md` |
| R10 | Resource-efficient | Resource budget in §2, verified in M10 |
| R11 | Stable and thoroughly tested | `docs/TESTING.md`, mandatory CI gates on every PR |
| R12 | All Rust (UI + backend) | ADR-0001, ADR-0002 (exceptions: minimal JS browser extension, optional `yt-dlp`/`ffmpeg` sidecars) |
| R13 | Windows and Linux as equal first-class targets; macOS supported | 3-OS CI matrix, ADR-0007 |
| R14 | Owner's legacy script workflows (batch downloads from hosters like fuckingfast / XFileSharing / datanodes) | M5 + M6, see `docs/reference/legacy-scripts/README.md` |
| R15 | Bilingual UI: English (`en`) and Italian (`it`) with user-selectable language in Settings | M3.7 + `AGENTS.md` |
| R16 | JDownloader `.dlc` (Download Link Container) compatibility | M5.6, M3.2, M3.6, M9.6 |
| R17 | LAN Remote Web UI (qBittorrent-style): monitor and control downloads from a phone/browser on the local network (with or without password) | M8.1, M8.2, M3.4 |
| R18 | In-app GitHub Release auto-updater (like `Scaricatore-di-Porto-rust`): detect new GitHub releases, notify in-app with release notes, download with progress bar, verify checksum/signature, install & restart directly from the app | M10.5, M3.4 |

---

## 1. Architectural Decisions (Summary — Details in `docs/adr/`)

- **GUI**: [iced](https://github.com/iced-rs/iced) ≥ 0.14 (Elm architecture, reactive rendering, MIT license). ADR-0001.
- **Async runtime**: `tokio`. Engine as an in-process library; the GUI communicates with the engine via `EngineHandle`
  (commands over `mpsc`, events over `broadcast`). ADR-0003.
- **HTTP**: `reqwest` (`hyper` + `rustls`) with a custom segmentation engine (aria2/IDM-style algorithm with
  dynamic *work stealing*). ADR-0003.
- **BitTorrent**: [`librqbit`](https://github.com/ikatson/rqbit) (Apache-2.0, ≥ 9.x). ADR-0005.
- **Persistence**: SQLite (`rusqlite`, bundled, WAL mode) for state/queue/settings + per-download control file
  for segment bitmaps. ADR-0006.
- **Video/streaming sites**: native Rust extractors for HLS/DASH + **yt-dlp as an optional sidecar** automatically
  managed by the app (download/update). Optional `ffmpeg` sidecar for merge/remux. ADR-0002.
- **Pages with anti-bot challenges / login**: out-of-process `ruscello-webview` helper (`wry`) where **the user**
  completes the verification; cookies/session are then automatically reused across the batch. No automated
  CAPTCHA solving, no stealth/fingerprint spoofing. ADR-0004.
- **License**: `GPL-3.0-or-later`. ADR-0007.

---

## 2. Resource & Performance Budget (Measurable Targets, Verified in M10)

| Metric | Target |
|--------|--------|
| Idle RAM (GUI open, 0 active downloads, webview closed) | < 80 MB |
| RAM with 10 active HTTP downloads | < 150 MB |
| Idle CPU | ~0% (no redraws/polling without events) |
| CPU at 1 Gbit/s on a single file | < 15% of one core (excluding kernel I/O) |
| Cold start to usable window | < 1 s |
| UI progress updates | max 4 Hz per visible row, aggregated |
| Data loss after `kill -9` / power loss | ≤ last ~2 s of downloaded data (re-fetched), never file corruption |
| Segmented throughput vs `aria2` (`-x16 -s16`) on local server with simulated latency | ≥ 95% |

---

## 3. Roadmap

```
M0 Bootstrap ─► M1 HTTP Engine ─► M2 Queue & Storage ─► M3 GUI v1 ─┬─► M4 Torrent
                                                                    ├─► M5 Resolver & LinkGrabber ─► M6 Browser Helper
                                                                    └─► M7 Media (HLS/DASH/yt-dlp)
                                  M8 Browser Extension + Local API (after M5)
                                  M9 Automation & Post-Processing
                                  M10 Hardening, Performance, Packaging, 1.0 Release
```

M4, M5, and M7 can proceed in parallel after M3 (including across multiple agents on separate branches).

---

## M0 — Project Bootstrap ✅ (set up by planning agent)

- [x] Multi-crate Cargo workspace, Edition 2024, workspace lints
- [x] `AGENTS.md`, work plan, ADRs, UI/testing/architecture docs
- [x] GitHub Actions CI (`fmt`, `clippy`, tests on Windows/Linux/macOS, `cargo-deny`)
- [x] GitHub repository, milestones, and issues
- [x] **M0.1** Verify `cargo check --workspace` is green in CI; add
      `tracing` + `tracing-subscriber` with rotating file logs in `%LOCALAPPDATA%`/`$XDG_STATE_HOME`
      (`directories` crate); error handling structure (`thiserror` in libraries, `anyhow` in binaries only).

---

## M1 — Segmented, Crash-Safe HTTP Engine (crates `ruscello-http`, `ruscello-testkit`)

**Goal**: download an HTTP/HTTPS URL quickly, with pause/resume at any point, without ever corrupting the output file.

### Tasks
- [x] **M1.1 Testkit server** (`ruscello-testkit`): local HTTP server (`axum`/`hyper`) with configurable scenarios:
      range supported/unsupported, missing `Content-Length`, chunked transfer, multi-hop redirects,
      `Content-Disposition` (including RFC 5987 `filename*=`), mid-transfer `ETag`/`Last-Modified` changes,
      connection drop after N bytes, per-connection throttling, max connections per IP limit
      (returning 429/503), artificial latency, intermittent 5xx responses. Deterministic seed-generated
      payloads (no binary blobs committed to git).
- [x] **M1.2 Probe**: initial request (`GET` with `Range: bytes=0-` preferred over `HEAD`, as many servers
      mishandle `HEAD`) → size, range support, filename (`Content-Disposition` > final URL >
      MIME → extension), validators (`ETag`, `Last-Modified`), final URL after redirects.
- [x] **M1.3 Segment allocator** (pure, no I/O, tested with `proptest`):
  - initial split into N segments respecting `min_segment_size` (default 1 MiB, adaptive);
  - **work stealing**: when a connection finishes, split the largest remaining segment in half
    (or the one with the highest estimated completion time) if the remaining bytes exceed the threshold;
  - **connection slow start**: add connections while aggregate throughput grows (> +10%) up to the per-host limit;
    scale back on 429/503 or connection resets;
  - invariants: no overlap, complete coverage, monotonically increasing completed bytes.
- [x] **M1.4 Writer**: preallocated `*.rscpart` file (`set_len`; sparse on NTFS when applicable), positional writes
      (`FileExt::seek_write`/`write_at`) from a dedicated writer task with coalescing buffers
      (e.g., 1–4 MiB per segment) to avoid micro-writes; atomic rename to final filename upon completion;
      collision handling (`file (1).ext`) and OS-illegal character sanitization.
- [x] **M1.5 Persistent state / resume**: control file `*.rscpart.ctl` (`bincode`/`postcard` +
      CRC32 checksum + format version) storing completed bitmap/ranges, written via *write-temp + fsync +
      rename* every ~2 s or every X MiB; guaranteed ordering: **fsync data file first, then update control
      file** (so the control file never claims bytes not yet persisted to disk). On resume: validate with
      `If-Range`; if remote file changed → restart cleanly (with warning) instead of corrupting.
- [x] **M1.6 Non-range downloads**: single-connection fallback; if dropped, restart from zero (marked in UI
      as "not resumable").
- [x] **M1.7 Retry & reconnection**: exponential backoff with jitter, error classification
      (transient / permanent / auth / expired link → requests re-resolve from plugin), network loss detection
      and waiting for connectivity restoration without burning retry attempts.
- [x] **M1.8 Bandwidth limiter**: global + per-download token bucket, hot-reconfigurable at runtime.
- [x] **M1.9 Mirrors**: multiple URLs for the same file → segments distributed across matching-size sources
      with usable range validators. An expected file checksum is required before combining mirror data.
- [x] **M1.10 Integrity verification**: optional hash check (MD5/SHA-1/SHA-256/CRC32) supplied by user or plugin.
- **M1.11 FTP/FTPS** is deferred to M9.8.
- [x] **M1.12 Developer CLI** (`apps/cli`): `ruscello-cli get <url> [-o dir] [-x conns]` to test and benchmark
      the engine without the GUI.

### Definition of Done — M1
- Integration tests covering all testkit scenarios, including:
  - **process kill during download → restart → final file matches expected SHA-256** (test spawns
    `ruscello-cli` as a child process and terminates it forcefully);
  - simulated mid-download network drop → automatic resume;
  - changed `ETag` → no corrupted file.
- `criterion` / script benchmark vs `aria2` against the testkit with 50 ms latency: throughput ≥ 95%.

---

## M2 — Queue, Scheduler, Storage, Categories (crates `ruscello-engine`, `ruscello-storage`, `ruscello-core`)

- [x] **M2.1 Domain model** (`ruscello-core`): `Download`, `Package` (download group as in
      JDownloader), `Source` (`Http`/`Torrent`/`Media`/`Hoster`), `Status` (`Queued`, `Resolving`, `WaitingUser`,
      `Downloading`, `Paused`, `Seeding`, `PostProcessing`, `Completed`, `Failed{reason}`), `Category`, `Tag`,
      `Priority`. Explicit state machine with unit-tested transitions.
- [x] **M2.2 SQLite storage** with versioned migrations (`rusqlite_migration` or equivalent), WAL mode,
      batched async writes (dedicated actor), automatic DB backup. Corrupted DB recovery
      (rebuilding state from `.rscpart.ctl` control files).
- [x] **M2.3 Scheduler**: max global concurrent downloads, max connections per host, priorities, manual ordering
      (drag), "force start", automatic retries with cap, automatic resume on startup for downloads that were active.
- [x] **M2.4 Engine API**: `EngineHandle` with commands (`add`, `pause`, `resume`, `cancel{delete_files}`,
      `set_priority`, `move`, `set_limits`…) and events (`Added`, aggregated/throttled `Progress`,
      `StatusChanged`, `Log`). All `Send + Clone`; no locks exposed to the GUI.
- [x] **M2.5 Automatic categorization**:
  - *File Type* (automatic, read-only): Video, Audio, Images, Documents, Archives, Programs,
    Torrents, Other — inferred from extension + MIME + magic bytes (`infer`) upon completion;
  - *Categories* (user-defined): rules matching domain / extension / regex / source → destination folder and
    limits; default categories: Games, Movies & TV, Music, Software, Work.
- [x] **M2.6 Disk management**: free-space check before and during download (automatic pause with warning if disk space
      runs low), optional separate temporary directory for in-progress downloads.

### Definition of Done — M2
State machine unit tests; engine integration tests with testkit (queue of 50 downloads, max 3 concurrent,
engine restart mid-run → all resumed and completed). Zero `unwrap()` on runtime paths.

---

## M3 — GUI v1 (`apps/gui`, iced) — see `docs/UI.md`

- [x] **M3.1** Main window shell: sidebar (Status / File Types / Categories), virtualized download list,
      collapsible details panel, status bar (total speed ↓↑, active limit, free disk space).
- [x] **M3.2** Unified smart "Add Download" dialog: paste one or more URLs / magnets / `.torrent` / `.dlc` paths;
      automatic type detection; category and destination folder picker; drag & drop of files (`.torrent`, `.dlc`, `.txt`) and links.
- [x] **M3.3** Actions: pause/resume/cancel (with confirmation and "delete files" checkbox), open file/folder,
      copy link, priority, reorder; multi-selection; keyboard shortcuts.
- [ ] **M3.4** Settings (clean, concise pages): General (including **Language selector: English / Italiano** and
      **UI Renderer selector: `wgpu` Hardware GPU [default] vs `tiny-skia` Software 2D [G-SYNC/OLED compatibility mode]**, see ADR-0001),
      Downloads, Connection/Limits, BitTorrent, Categories & Rules, Integrations & Remote Control (LAN Web UI toggle, port, optional password, QR code; browser extension; `yt-dlp`/`ffmpeg`), Advanced.
- [x] **M3.5** System tray icon (`tray-icon`), close-to-tray, native desktop notifications (`notify-rust`),
      launch at login, **single-instance IPC** (a second invocation forwards URLs/magnets/`.torrent`/`.dlc` files to the running instance).
- [x] **M3.6** File/protocol associations: `magnet:`, `.torrent`, and `.dlc` (Windows Registry / `.desktop` + `xdg-mime` on Linux).
- [x] **M3.7** Light/dark theme (follows OS with manual override), bundled Inter font, Lucide icons (ISC) as icon font;
      **i18n with `fluent` supporting both English (`en`) and Italian (`it`) from day one, switchable at runtime in Settings**.
- [x] **M3.8** Headless GUI tests with `iced_test` covering primary workflows + view snapshots.

### Definition of Done — M3
Users can add, monitor, pause, resume, and cancel HTTP downloads from the GUI, switch UI language between
English and Italian, and experience ~0% idle CPU with zero UI thread blocking.

---

## M4 — BitTorrent (crate `ruscello-torrent`)

- [ ] **M4.1** Integrate `librqbit` as a single session managed by the engine (DHT, PEX, trackers,
      UPnP/NAT-PMP, configurable listening port, `librqbit` fastresume persistence in data dir).
- [ ] **M4.2** Add from magnet / `.torrent` file / `.torrent` URL; pre-add dialog with file tree,
      file selection, total size, destination folder; metadata fetching state ("Fetching metadata") for magnets.
- [ ] **M4.3** Limits: upload/download speed, seed ratio and seeding time limits (with automatic stop), max peers.
- [ ] **M4.4** Details tabs: files with priorities, peers, trackers, piece map bar, speeds; "Force recheck".
- [ ] **M4.5** Map `librqbit` states → common `Status`; torrents appear in the unified list alongside other
      downloads (File Type "Torrents", assignable to user Categories like any other download).
- [ ] **M4.6** Optional: network interface binding (VPN kill-switch binding), IP filter list (eMule/P2P format).

### Definition of Done — M4
Tests using local tracker + seeder (`librqbit` can create and serve torrents locally) downloading a multi-file
torrent, interrupting, resuming, and verifying piece hashes. Local DHT magnet test if deterministic, otherwise
marked `#[ignore]` and documented.

---

## M5 — Resolver, Hoster Plugins & LinkGrabber (crate `ruscello-resolver`)

**Goal**: given any URL (web page, file hoster, direct link, list of links), determine what to download.
Inspired by JDownloader (host/decrypter plugins, LinkGrabber).

- [ ] **M5.1 Resolution pipeline**: `URL → [Decrypter/Crawler] → candidates → [Host Plugin] →
      direct URL(s) + required headers/cookies + max allowed connections + expiration`.
      The final direct link is always downloaded by **our HTTP engine** (segmented, resumable). If a direct link
      expires mid-download, the engine requests a fresh link from the plugin and resumes from the exact byte offset.
- [ ] **M5.2 `HostPlugin` / `CrawlerPlugin` traits** in Rust (compiled plugins, statically registered) with:
      `matches(url)`, `resolve(ctx) -> Resolution`, `requires_browser() -> bool`, hoster constraints (max connections,
      max parallel downloads per hoster, mandatory countdown waits). Execution context provides shared HTTP client,
      per-domain cookie jar, access to the browser helper (M6), and user-interaction requests.
- [ ] **M5.3 Declarative "Recipes"** (TOML, hot-reloadable, user-extensible without recompiling) for
      simpler sites: steps like `get`, `submit_form{selector|fields}`, `wait{seconds}`, `extract{css|regex}`,
      `follow`, and in browser mode `click{selector}`, `wait_for{selector}`. This turns custom scripts
      (see legacy-scripts) into concise recipes.
- [ ] **M5.4 Initial built-in plugins** (each backed by recorded HTML fixtures and offline tests):
  - generic "direct link" detector (identifies non-HTML responses);
  - **fuckingfast** (recognizes `fuckingfast.co/...` share pages as hoster pages rather than raw files; extracts the
    direct `/dl/...` download URL from the inline JavaScript `window.open("https://fuckingfast.co/dl/...")` or
    `hx-redirect` response over HTTP without opening ad popups; enforces a safe per-host concurrency limit to avoid
    `429 Too Many Requests`; automatically re-resolves expired `/dl/...` links on resume; falls back seamlessly to the
    persistent-profile browser helper M6.4 if Cloudflare blocks plain HTTP) — high-priority owner use case;
  - **generic XFileSharing & `datanodes.to`** (`op=download1`/`method_free` → countdown → `op=download2`/`downloadbtn`),
    pattern shared by many file hosters including `datanodes.to` (commonly distributed inside `.dlc` containers) — high-priority owner use case;
  - pixeldrain, gofile, mediafire, Google Drive (public files including large-file confirmation), public
    Dropbox/OneDrive links, GitHub Releases, SourceForge, archive.org;
  - generic "index page" crawler (extracts all supported hoster links from a page and groups them into a **Package**).
- [ ] **M5.5 LinkGrabber with explainable scoring** (requirement R7). Page analysis (HTML via `scraper`, inline
      scripts via regex, JSON-LD/OpenGraph, `<video>/<source>`, player iframes, known hoster URLs,
      `.m3u8`/`.mpd` URLs, and network resources captured if opened in the browser helper). Each candidate
      receives an explainable score:
  - `+` downloadable file extension (archives, video, ISO, executables, PDF…), `+` large file size (bounded
    parallel probe), `+` link text/ARIA contains "download/get", `+` filename matches page title,
    `+` matches a known hoster plugin, `+` `Content-Disposition: attachment`;
  - `−` ad/tracker domains (updatable EasyList/hosts filter), `−` tiny files, UI icons/sprites, CSS/JS,
    `−` social/login links.

  UI: grouped candidate list (*Likely Targets ⭐* / *Other Files* / *Media* / *Linked Pages*), with *Likely Targets*
  pre-selected and score reasons shown in tooltips (e.g., *"4.2 GB archive · name matches page title"*).
  Type filters, search box, multi-select, "Download Selected". Configurable **auto-accept rules**
  (e.g., "if a known hoster plugin matches, start immediately without prompting").
- [ ] **M5.6 Bulk import & `.dlc` container decryption**:
  - **`.dlc` (Download Link Container) support** (requirement R16): decrypt JDownloader `.dlc` files natively in Rust
    (extract 88-char key tail + Base64 payload, exchange key via `service.jdownloader.org/dlcrypt/service.php` with
    `dcrypt.it` fallback, AES-128-CBC decrypt `<rc>` and XML body — see exact specification in `docs/ARCHITECTURE.md`)
    to extract package name, file URLs, filenames, and sizes into a **Package**;
  - paste free-form text (extracts all URLs), `.txt` link lists, optional clipboard monitor (off by default),
    `.torrent`/`.metalink`/`.m3u` containers; automatic deduplication.
- [ ] **M5.7 Hoster accounts** (premium/login credentials) stored in the OS keyring (`keyring` crate), never in
      plaintext in SQLite.

### Definition of Done — M5
Offline fixture tests for every plugin; LinkGrabber scoring benchmark across a corpus of ≥ 30 saved pages with
expected targets (true target ranked in top 3 in ≥ 90% of cases); end-to-end test: batch of 20
fuckingfast-like pages served by `ruscello-testkit` → all files downloaded and verified.
Live network tests isolated behind the `live-tests` feature flag, never required in default CI.

---

## M6 — Browser Helper & User-Completed Verifications (app `apps/webview`, crate `ruscello-browser`)

See ADR-0004. The browser helper is a **separate process** (`wry` + `tao`: WebView2 on Windows, WebKitGTK on
Linux, WKWebView on macOS) spawned on demand and shut down after inactivity (saving RAM). It communicates with
the engine over stdin/stdout JSON-lines defined in `ruscello-browser::protocol`.

- [ ] **M6.1 Process lifecycle, persistent profile & IPC protocol**: spawn on demand, idle timeout shutdown, crash
      recovery/restart, and a **persistent browser profile on disk** (`<data_dir>/webview_profile/`) so clearance cookies
      (e.g., `cf_clearance`) survive across batches and application restarts.
- [ ] **M6.2 "Verification Required" window**: when a plugin/recipe detects an anti-bot challenge page
      (full-page Cloudflare challenge, hoster CAPTCHA, login wall), the engine puts the download into
      `WaitingUser`, shows a non-intrusive notification; the user opens the helper window, **completes the
      check themselves**, and the helper detects navigation to the expected target page and closes automatically.
      Queued downloads for the same domain wait on the same verification (one check unlocks the entire batch
      for as long as the session remains valid).
- [ ] **M6.3 Session reuse**: cookies (`cookies_for_url`, including `HttpOnly`) and the exact `User-Agent` from
      the webview are exported to the engine's HTTP client for that domain. If a site rejects requests outside
      the browser context, fallback to M6.4.
- [ ] **M6.4 Browser-assisted resolution**: execute recipe steps *inside* the webview DOM (`click`,
      `wait_for`, `extract`), with **popups and new windows blocked natively** (`new_window_req_handler`),
      and any browser-initiated download **intercepted** (`download_started_handler`): cancel the webview's
      built-in download and hand off the final URL + cookies + referer to our segmented Rust HTTP engine.
      The webview can run hidden when no user interaction is needed.
- [ ] **M6.5 Resource capture**: injected script collecting `performance.getEntriesByType('resource')`,
      `<video>` sources, and `fetch`/`XHR` URLs → fed into LinkGrabber candidates (useful for embedded video players).
- [ ] **M6.6 Cookie import from installed browsers** (best-effort: Firefox SQLite import; Chromium on Windows
      with App-Bound Encryption may not be accessible → recommend the M8 browser extension). Triggered only by
      explicit user action.

### Definition of Done — M6
Integration test with a local testkit page simulating "challenge page → simulated user click in test →
download page": engine automatically resumes and completes the batch. Download interception and popup blocking
tested against a local page firing multiple `window.open` popups (replicating the exact behavior of the hosters
in the legacy scripts).

---

## M7 — Media: Native HLS/DASH + Optional yt-dlp Sidecar (crate `ruscello-media`)

- [ ] **M7.1** HLS (`m3u8-rs`): master playlist parsing → quality selection, parallel segment downloads via our
      engine, AES-128 decryption support, segment-level resume, `.ts` concatenation.
- [ ] **M7.2** Basic DASH (MPD): segment template/list, separate video + audio track downloading.
- [ ] **M7.3** Sidecar manager: download official `yt-dlp` releases from GitHub (with SHA-256 verification),
      periodic auto-update, and static `ffmpeg` builds — **only with explicit user consent** in Settings;
      custom binary paths also supported.
- [ ] **M7.4** `yt-dlp` integration: invoke `yt-dlp -J` to extract title/formats/playlists → clean format picker
      ("Best", "1080p", "Audio Only MP3"…); download via our native engine whenever direct format URLs + headers
      allow, falling back to `yt-dlp` execution with progress parsing (`--progress-template`, `--newline`).
      Pass domain cookies via temporary Netscape cookie file (`--cookies`).
- [ ] **M7.5** Audio + video merge/remux via `ffmpeg` sidecar when available; otherwise default to progressive formats.
- [ ] **M7.6** Playlists/channels → grouped into a selectable **Package**.

### Definition of Done — M7
HLS/DASH tests against locally generated stream fixtures; `yt-dlp` JSON parser tests using recorded fixtures;
optional live tests behind feature flag.

---

## M8 — Local API, LAN Remote Web UI & Browser Extension (crate `ruscello-api`, `extensions/browser`)

- [ ] **M8.1** HTTP/WebSocket API server (`axum` in `ruscello-api`):
  - **Loopback mode (`127.0.0.1`)** for the browser extension and CLI (token-authenticated, strict CORS);
  - **LAN Remote Control mode (`0.0.0.0:<port>`)** when enabled by the user in Settings (requirement R17, qBittorrent Web UI style);
  - **Optional authentication**: user can choose **no password** (instant access on a trusted home LAN) or **password protection**
    (username/password or PIN → session cookie / token, with brute-force rate limiting).
  - Endpoints: real-time queue & package status, speed telemetry (SSE/WebSocket), pause/resume/cancel (single or all),
    add URLs / `.torrent` / `.dlc` containers, toggle alternative speed limit (`🐢`).
- [ ] **M8.2** **Embedded Mobile-Friendly LAN Web UI**:
  - Clean, lightweight, responsive HTML/CSS/JS web interface bundled directly into the Rust binary (zero external runtime dependencies)
    so opening `http://<pc-lan-ip>:<port>` from a smartphone, tablet, or another PC browser shows live download progress, speeds, ETAs,
    and packages, and allows pausing/resuming/adding downloads remotely;
  - Settings page in the desktop app displays the LAN URL (`http://192.168.x.x:<port>`) and a scannable **QR Code** for instant phone access;
  - Localization in both **English (`en`)** and **Italian (`it`)** matching the desktop app.
- [ ] **M8.3** Manifest V3 browser extension (Chrome + Firefox) with minimal TS/JS code: optional browser download
      interception (filtered by MIME/file size), context menu "Download with Ruscello" on links/selection/page
      ("Analyze page with LinkGrabber"), automatic forwarding of domain cookies for authenticated downloads.
- [ ] **M8.4** Simple one-click pairing between extension and desktop app in Settings.

---

## M9 — Automation & Post-Processing

- [ ] **M9.1** Scheduler: time-of-day speed limit profiles and automatic queue start/stop schedule.
- [ ] **M9.2** Queue completion actions: do nothing / notify / sleep / shut down PC (with cancellable countdown).
- [ ] **M9.3** Prevent OS sleep while downloads are active (Windows `SetThreadExecutionState`, Linux
      `org.freedesktop.login1` inhibit via `zbus`, macOS `IOPMAssertion`).
- [ ] **M9.4** Automatic archive extraction: `.zip`, `.7z` (pure Rust crates), `.tar.*`; `.rar` via external
      `7z`/`unrar` binary if available (UnRAR license is incompatible with static GPL linking — document in ADR).
      Multi-part archives (`.part1.rar`, `.7z.001`) extracted only once all parts in the Package complete;
      password list support; optional archive deletion after successful extraction.
- [ ] **M9.5** Automatic checksum verification when a site/package provides `.sfv`, `.md5`, or `.sha256` files.
- [ ] **M9.6** Watch folder rules for `.torrent`, `.dlc`, and `.txt` link lists.
- [ ] **M9.7** Optional post-download user command hook.
- [ ] **M9.8 FTP/FTPS** (`suppaftp` crate) with resume via `REST`, deferred from M1.11.

---

## M10 — Hardening, Performance, Packaging & 1.0 Release

- [ ] **M10.1** Fuzzing (`cargo-fuzz`) for untrusted inputs: `Content-Disposition`, `m3u8`/MPD, TOML recipes, control files.
- [ ] **M10.2** 24-hour soak test: 200 mixed downloads, random network drops, random process kills → zero file
      corruption, zero unbounded memory growth.
- [ ] **M10.3** Verify resource budget (§2) with repeatable benchmark scripts; profile with `tracy`/`perf`/`samply`.
- [ ] **M10.4** Packaging: Windows (MSI or NSIS via `cargo-packager`/`cargo-dist`, optional code signing),
      Linux (AppImage + `.deb` + optional Flatpak), macOS (`.dmg`). Automated GitHub Releases on `v*` tags.
- [ ] **M10.5** **In-app GitHub Release auto-updater** (modeled after `EmanueleFusco/Scaricatore-di-Porto-rust`, requirement R18):
  - Check GitHub Releases (`api.github.com/repos/EmanueleFusco/ruscello/releases/latest` or release manifest JSON) automatically on startup (configurable toggle, enabled by default) and on demand via *Settings → About / Updates*;
  - Compare `semver` against current build version and return `UpdateCheck { available, current_version, version, notes, date }`;
  - Display a non-intrusive in-app update pill/banner in the main window when a newer build is available, showing version & release notes;
  - **One-click in-app update (`download_and_install`)**: if downloads are active, cleanly pause and flush all checkpoints (`.rscpart.ctl`) first (or prompt to pause queue before updating, just like `Scaricatore-di-Porto-rust`); download the platform release asset with a live progress bar (`UpdateProgress { downloaded, total, percent }`), verify SHA-256 checksum / Ed25519 signature, apply the update in-place (`self-replace` crate for standalone binaries / silent installer handoff for packaged installs), and relaunch the app.
- [ ] **M10.6** User documentation (`README.md`, quickstart guide, FAQ) and screenshots.
- [ ] **M10.7** Public repository readiness: scrub any personal path references, audit dependency licenses
      (`cargo-deny`), add `SECURITY.md`.

---

## 4. Key Risks & Mitigations

| Risk | Mitigation |
|------|------------|
| Hosters frequently change HTML structure | Declarative TOML recipes updatable without recompiling; fixture tests that fail clearly; browser-assisted fallback mode. |
| Anti-bot session cookies rejected outside browser TLS stack | Browser-assisted resolution mode (M6.4) executes clicks in webview and intercepts the final download URL for our engine. |
| Dual event loops (`iced`/`winit` and `wry`/`tao`) in one process | Run `ruscello-webview` as a separate on-demand child process (ADR-0004). |
| Memory footprint of WebView2 / WebKitGTK | Spawn helper process only when needed and terminate after idle timeout. |
| `yt-dlp` / `ffmpeg` are not written in Rust | Optional external sidecars; the app works standalone without them (ADR-0002). |
| License compatibility (GPL vs UnRAR, ffmpeg) | Enforced by `cargo-deny` in CI; external binaries invoked as subprocesses, never linked. |
| File corruption on sudden power-off | Strict *fsync data → fsync control file* ordering; mandatory `kill -9` tests in CI. |

## 5. GitHub Workflow

- Every `Mx.y` task maps to a GitHub Issue (or checklist item inside a milestone Epic issue). Branches follow
  `feat/m1-segment-allocator`, `fix/...`; PRs target `main` with green CI and squash merge.
- All commits, PRs, issues, code comments, and docs must use **English** (Conventional Commits: `feat(http): ...`, `fix(gui): ...`).
- At the end of each milestone: update checkboxes in `docs/PLAN.md`, update `CHANGELOG.md`, close the GitHub Milestone, and tag `v0.<M>.0`.
