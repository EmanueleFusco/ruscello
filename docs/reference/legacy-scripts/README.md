# Owner's Legacy Scripts (Reference Use Cases)

These two Python scripts represent workflows the project owner currently runs manually. **They must not be ported
verbatim**: instead, Ruscello must support these workflows natively in a much faster, crash-safe, and reliable way.
They are preserved here strictly as behavioral references.

| File | Behavior |
|------|----------|
| `fuckingfast_autodownloader.py` | Reads `links.txt` (one fuckingfast URL per line), opens each page in Chrome (`SeleniumBase` UC Mode), attempts to pass full-page Cloudflare challenges if present, clicks the `DOWNLOAD` button (the first click often opens an ad popup window → closes popup windows and clicks a second time), lets Chrome download the file, and polls the folder until `.crdownload`/`.part` files disappear. |
| `xfilesharing_autodownloader.py` | Follows the same pattern for an **XFileSharing**-based hoster: clicks `#method_free` → waits → closes ad popups → clicks `#downloadbtn` → polls the download folder to verify a new `.crdownload` file appeared. Uses randomized delays between links. |

## Pain Points of the Legacy Scripts That Ruscello Solves

1. Downloads are handled by Chrome's built-in downloader → if the connection drops or the PC turns off, the download
   is lost; single connection (slow); no bandwidth or queue control.
2. Ad popups are handled reactively by switching and closing browser windows after they open.
3. Completion detection relies on polling directory filenames.
4. Fragile setup: hardcoded selectors, Chrome/ChromeDriver version mismatches, fixed sleep timers.

## How Ruscello Covers These Workflows

| Need | Solution in Ruscello | Milestone |
|------|----------------------|-----------|
| Batch link list from file or clipboard | Multi-line Add dialog, `.txt` file import, watch folder, deduplication; all links in a batch grouped into a **Package** | M3, M5.6, M9.6 |
| `fuckingfast` | Native Rust plugin: recognizes `fuckingfast.co/...` share URLs as hoster pages (never failing with a "browser-only" error like simple IDM clones), fetches the page over HTTP, and extracts the direct `/dl/…` URL from the inline script (`window.open("https://fuckingfast.co/dl/...")` / `hx-redirect`) → downloads via the segmented, resumable HTTP engine with a safe per-host connection cap (avoiding HTTP `429`) and automatic link re-resolution if a `/dl/…` token expires after 24h. If Cloudflare blocks plain HTTP, automatically falls back to the persistent-profile browser helper (M6.4). Zero popups | M5.4, M6.4 |
| XFileSharing hosters | Generic XFS plugin (`op=download1`/`method_free` form → respects countdown → `op=download2`/`downloadbtn`) over HTTP; TOML recipes for site variants | M5.3, M5.4 |
| Ad popups and new windows | Non-existent in plain HTTP mode; blocked natively (`new_window_req_handler`) before opening when running in browser helper mode | M6.4 |
| Cloudflare / hoster verification pages | Download enters `WaitingUser` ("Verification required"); the user completes the check in the `ruscello-webview` helper window (typically **once per batch** while the session cookie remains valid), and all queued links for that domain resume automatically (see ADR-0004) | M6.2, M6.3 |
| Browser-initiated downloads | Intercepted by `ruscello-webview` (`download_started_handler`) and handed off to the segmented, crash-safe Rust engine | M6.4 |
| Knowing when the batch finishes | Explicit state machine, desktop notifications, and queue-completion actions (sleep/shutdown) | M2, M9.2 |
| Multi-part archives (typical of these batches) | Automatic extraction once all parts of the Package finish downloading; automatic checksum verification if `.md5`/`.sfv` files are present | M9.4, M9.5 |

## Automated Acceptance Tests (Powered by `ruscello-testkit`)

- 20 local "fuckingfast-like" pages → imported from `.txt` → all files downloaded with matching SHA-256,
  surviving mid-batch process kills and simulated network drops.
- 10 local "XFS-like" pages with countdowns and `window.open` ad popups → zero popups opened, countdowns respected,
  all downloads completed.
- Mock verification challenge in front of 5 links on the same domain → a single simulated user verification unlocks
  all 5 downloads automatically.
