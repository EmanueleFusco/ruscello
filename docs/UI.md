# UI Specifications

Goal: a **professional, calm, and reliable** appearance (inspirations: qBittorrent for information density,
modern apps like Motrix / AB Download Manager / Linear for visual cleanliness). Minimal does not mean empty:
all essential information is visible at a glance, while infrequent actions live in context menus or Settings
rather than cluttering toolbars.

## Localization (i18n) Requirement

- The entire user interface **must support both English (`en`) and Italian (`it`)**.
- On first launch, the application defaults to the OS locale (`it` if the system is in Italian, `en` otherwise).
- Users can manually choose **English**, **Italiano**, or **System Default** in *Settings → General*, and the UI
  updates immediately without requiring a restart.

## Main Window Layout

```
┌──────────────────────────────────────────────────────────────────────────────┐
│  [+ Add]   🔍 Search downloads…                           ⏸ All   🐢   ⚙     │  ← single-row toolbar
├───────────────┬──────────────────────────────────────────────────────────────┤
│ STATUS        │ Name                        Size     Progress     Speed  ETA │
│  All       42 │ ▸ Package: Game X (12)      48.2 GB  ███████░ 71% 38 MB/s 9m │
│  Active     3 │   ubuntu-24.04.iso           5.7 GB  ████████ 100%   —     — │
│  Queued     5 │   movie.mkv (torrent)       12.1 GB  ███░░░░░ 34% 11 MB/s 30m│
│  Completed 30 │   report.pdf                 2.1 MB  ⚠ Verification required │
│  Failed     1 │                                                              │
│  Seeding    3 │                                                              │
│ FILE TYPES    │                                                              │
│  Video        │                                                              │
│  Audio        │                                                              │
│  Archives     │                                                              │
│  Programs     │                                                              │
│  Documents    │                                                              │
│  Images       │                                                              │
│  Torrents     │                                                              │
│ CATEGORIES    ├──────────────────────────────────────────────────────────────┤
│  Games        │ Details: General · Segments · Files · Peers · Trackers · Log │ ← collapsible panel
│  Movies & TV  │ (contextual to the selected download)                        │
│  + New        │                                                              │
├───────────────┴──────────────────────────────────────────────────────────────┤
│ ↓ 49.3 MB/s  ↑ 1.2 MB/s   Limit: off   Disk D: 412 GB free   DHT: 312 nodes  │ ← status bar
└──────────────────────────────────────────────────────────────────────────────┘
```

- **Sidebar** with three sections (*Status*, *File Types*, *Categories*) and item counters; sections act as
  combinable filters (e.g., *Active* + *Video*). Optional fourth *Tags* section shown only if the user creates tags.
- **Virtualized list** (handles thousands of rows smoothly); configurable columns via header right-click;
  expandable Packages; file-type icon next to the name; slim progress bar colored by state
  (accent = active, gray = paused, green = completed, amber = waiting for user, red = failed).
- **Row hover actions**: reveals 2–3 subtle icons (pause/resume, open folder, `⋯`); all other operations live
  in the right-click / `⋯` context menu.
- **Toolbar**: only Add, Search, Pause/Resume All, Alternative Speed Mode (`🐢`, qBittorrent-style), and Settings.
- **Collapsible details panel** (remembers open/closed state): *General* (URL, folder, hashes, timestamps),
  *Segments* (visual map of segments/pieces and active connections), *Files / Peers / Trackers* (shown for torrents),
  and per-download *Log*.

## Unified "Add Download" Dialog

1. Multi-line input box: paste URLs, magnet links, free-form text containing links, or drag & drop `.torrent` / `.dlc` / `.txt`
   files (or pick them via file dialog). Pre-populated from clipboard if valid URLs are detected (never auto-started without
   confirmation unless an auto-accept rule matches).
2. Immediate background probing (per-row spinner): each entry resolves into a detected type (*Direct File*,
   *Hoster «name»*, *Container `.dlc` (decrypted Package)*, *Torrent*, *Media Stream*, or *Web Page → LinkGrabber*).
3. For web pages: switches to the **LinkGrabber** view (see `PLAN.md` M5.5) with collapsible groups:
   *Likely Targets ⭐* (pre-selected), *Other Files*, *Media*, *Linked Pages*; each row displays filename, size,
   host, and explainable score badge.
4. Bottom bar: Category (auto-suggested by rules), Destination Folder, "Start Immediately" toggle (on by default),
   and collapsed Advanced Options (max connections, speed limit, expected checksum, archive password).

## User Verifications & Prompts

- Never pop up unexpected modal windows over the user's work. When a download requires user interaction, its status
  changes to *"⚠ Verification required"*, accompanied by a system notification and a sidebar badge. Clicking the row
  action opens the browser helper window directly on the challenge page; once completed, the window closes
  automatically and the queued batch resumes.

## Visual Style

- Light and dark themes (follows OS by default, overridable in Settings). Neutral warm-gray palette, single accent
  color (blue/teal), semantic status colors. No flashy gradients, subtle elevation borders, 6–8 px corner radius,
  short feedback transitions (≤ 150 ms) for hover/expand only.
- Typography: bundled Inter font, 13–14 px body text, tabular numerals (`tnum`) for speeds, sizes, and ETAs.
- Icons: monochrome Lucide icon font (ISC license).
- Density: 32–36 px row height; optional compact density mode.
- Accessibility: WCAG AA contrast, full keyboard navigation, tooltips on all icon-only buttons.

## Keyboard Shortcuts

`Ctrl+N` Add · `Ctrl+V` (in main window) Paste & Add · `Space` Pause/Resume ·
`Del` Cancel/Remove · `Ctrl+F` Search · `Enter` Open File · `Ctrl+Enter` Open Folder · `Ctrl+,` Settings.

## Settings Pages

- **General**: Language (*System Default / English / Italiano*), Theme, **UI Renderer** (*Hardware GPU `wgpu` [Default] / Software 2D `tiny-skia` [prevents G-SYNC/OLED flicker & NVIDIA game overlay]*, applied on restart), Launch at startup, System tray behavior, Queue completion action.
- **Downloads**: Default folder, max concurrent downloads, max connections per file/host, temporary folder.
- **Speed**: Global limits, Alternative speed limits (`🐢`), Time-of-day scheduler.
- **BitTorrent**: Listening port, DHT/PEX, UPnP/NAT-PMP, seed ratio/time targets, network interface binding.
- **Categories & Rules**: User categories, default folders, auto-assignment rules.
- **Integrations & Remote Web UI**:
  - **LAN Remote Web UI (qBittorrent-style)**: toggle *Enable Remote Web UI on local network*, port number, **Require Password** toggle (user can choose password-protected or no-password access on LAN), clickable LAN URL (`http://192.168.x.x:9666`), and a **QR Code** to open the interface immediately on a smartphone browser.
  - **Browser Extension**: pairing token and instructions.
  - **Sidecars & Clipboard**: `yt-dlp`/`ffmpeg` sidecar manager & auto-update, optional clipboard monitor.
- **Hoster Accounts**: Credentials stored in OS keyring.
- **Advanced**: Logs, proxy, custom User-Agent, DNS, reset.
