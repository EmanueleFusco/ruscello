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
- **Toolbar**: only Add, Search, Pause/Resume All, Alternative Speed Mode (`🐢`, qBittorrent-style), Settings, and (when a new GitHub Release is detected) a subtle **"⬆ Update available (`vX.Y.Z`)"** pill that opens the in-app updater dialog with release notes, download progress bar, and *Update & Restart* button.
- **Collapsible details panel** (remembers open/closed state): click the Details heading or the toolbar toggle
  to expand or collapse the panel. The heading remains visible when collapsed. Tabs include *General* (URL, folder, hashes, timestamps),
  *Segments* (visual map of segments/pieces and active connections), *Files / Peers / Trackers* (shown for torrents),
  and per-download *Log*.

## Unified "Add Download" Dialog

1. Paste HTTP, HTTPS, FTP, or FTPS URLs, magnet links, or text containing links. Drop or choose `.torrent`, `.dlc`, `.txt`, Metalink,
   M3U, or link shortcut files. The Add dialog can prefill URLs from the clipboard. The optional hoster
   auto-accept setting starts batches containing only recognized hoster links.
   The GUI accepts dropped link shortcuts when the window system delivers them as files. On native Wayland, use paste
   or the file picker until iced exposes drop events.
2. Immediate background probing (per-row spinner): each entry resolves into a detected type (*Direct File*,
   *Hoster «name»*, *Container `.dlc` (decrypted Package)*, *Torrent*, *Media Stream*, or *Web Page → LinkGrabber*).
3. **LinkGrabber** appears inside Add. Search or filter the collapsible groups *Likely targets* (preselected),
   *Other files*, *Media*, and *Linked pages*. Hover over a score to see how it was calculated. Imported
   packages keep their names, and **Download selected** queues the checked entries. Media streams (HLS,
   DASH, and optional `yt-dlp` sites) offer quality/format and playlist item selection directly in Add.
4. Choose a category and destination or use the automatic defaults. **Start immediately** is on by default.
   Advanced options include max connections, bandwidth limit, and expected checksum.

## User Verifications & Prompts

When a host requires verification, the queue shows **Verification required** and unrelated downloads can
continue. **Open verification** is available in the row controls, context menu, and Details. The browser
helper opens the site in a separate window where the user completes the verification. When the site
returns to the verified page or initiates the file transfer, the window closes and waiting downloads
for that origin resume. Add also offers **Analyze in browser** for pages with script-loaded resources.

## Visual Style

- Light and dark themes (follows OS by default, overridable in Settings). Neutral slate surfaces, blue accents,
  and semantic status colors. Use 8 px corner radii for panels, buttons, and inputs, and 4 px for checkboxes
  and slim progress bars. Only the selected background fades when switching tabs or Settings pages. The Details
  tabs fade as the panel expands or collapses, and the heading stays visible. Transitions last at most 150 ms,
  stop repainting when settled, and can be disabled in General settings.
- Typography: bundled Inter font, 13–14 px body text, tabular numerals (`tnum`) for speeds, sizes, and ETAs.
- Icons: monochrome Lucide icon font (ISC license).
- Density: 32–36 px row height; optional compact density mode.
- Accessibility: WCAG AA contrast, full keyboard navigation, tooltips on all icon-only buttons.

## Keyboard Shortcuts

`Ctrl+N` Add · `Ctrl+V` (in main window) Paste & Add · `Space` Pause/Resume ·
`Del` Cancel/Remove · `Ctrl+F` Search · `Enter` Open File · `Ctrl+Enter` Open Folder · `Ctrl+,` Settings.

## Settings Pages

- **General**: Language (*System Default / English / Italiano*), Theme, **UI Renderer** (*Hardware GPU `wgpu` [Default] / Software 2D `tiny-skia` [prevents G-SYNC/OLED flicker & NVIDIA game overlay]*, applied on restart), Launch at startup, and System tray behavior.
  Windows also offers an optional **Fixed refresh rate (NVIDIA)** setting for hardware rendering. It shows existing
  fixed-refresh profiles and can restore an override applied by Ruscello. Restart the app after changing the profile.
- **Downloads**: Default folder, max concurrent downloads, max connections per file/host, temporary folder.
- **Speed**: Global limits and Alternative speed limits (`🐢`).
- **Automation**: OS sleep inhibition, Queue completion actions (with cancellable countdown banner), Time-of-Day scheduler rules, automatic checksum verification (`.sfv`/`.md5`/`.sha256`), automatic archive extraction (`.zip`, `.7z`, `.tar.*`, external `.rar` sidecar, password list), post-download command hook, and Watch Folders (`.torrent`, `.dlc`, `.txt`).
- **BitTorrent**: Listening port, DHT/PEX, UPnP/NAT-PMP, seed ratio/time targets, network interface binding.
- **Categories & Rules**: User categories, default folders, auto-assignment rules.
- **Integrations & Remote Web UI**:
  - **Browser helper**: enable the helper and explicitly import cookies for a website from Firefox or
    a readable Chromium profile. Cookies stay in Ruscello's browser profile and in memory during resolution.
    For encrypted Chromium profiles, use the verification window or send a link with the paired browser extension.
  - **Link imports**: the clipboard monitor, hoster auto-accept, and dcrypt.it fallback are opt-in.
    The alternate DLC service receives the encrypted container when the primary key service fails.
  - **LAN Remote Web UI (qBittorrent-style)**: toggle *Enable Remote Web UI on local network*, port number, **Require Password** toggle (user can choose password-protected or no-password access on LAN), clickable LAN URL (`http://192.168.x.x:9666`), and a **QR Code** to open the interface immediately on a smartphone browser.
  - **Browser Extension**: start a pairing window, approve or reject pairing requests, or revoke paired extensions.
  - **Media Sidecars**: opt in to managing `yt-dlp` or `ffmpeg`, or supply an executable path. The `yt-dlp` auto-update option is available here.
- **Hoster Accounts**: save or remove Pixeldrain and Gofile API tokens in the operating system credential store.
- **Advanced & About**: Open logs, copy a diagnostics report, and check for GitHub releases. About shows the
  current version, release date and notes, and a startup check toggle. **Update & restart** shows progress,
  verifies the package, and saves active downloads before installation. The toolbar pill opens this page.
