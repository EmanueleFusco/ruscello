# ADR-0004 — Anti-Bot Challenges: Human-in-the-Loop Browser Helper & Session Reuse

- Status: Accepted (2026-11-12)

## Context
Several file hosters and websites sit behind anti-bot challenges (full-page Cloudflare checks, hoster CAPTCHAs, login
walls). The project owner wants downloads from these sites to succeed reliably with minimal manual effort.

## Decision
- We **do not** implement automated CAPTCHA solvers, automated clicks on verification widgets, third-party solving
  services, TLS/fingerprint spoofing (JA3), or "stealth/undetected" browser patches. Reasons: extreme fragility
  (constant cat-and-mouse breakage), IP/account ban risks, Terms of Service, and keeping the project clean and publishable.
- Adopted architecture (matching JDownloader 2's browser solver and `yt-dlp` cookie workflows):
  1. When the resolver detects a verification challenge, the download transitions to `WaitingUser` while unrelated
     downloads continue uninterrupted.
  2. The user opens the **browser helper** window (`ruscello-webview`, built on `wry`: WebView2 on Windows, WebKitGTK
     on Linux, WKWebView on macOS) and completes the check themselves.
  3. Cookies (including `HttpOnly` clearance cookies such as `cf_clearance`) and the exact `User-Agent` are extracted
     and reused by the engine's HTTP client for that domain; **a single user check unlocks all queued downloads for
     that domain** while the session remains valid.
  4. If a hoster binds the session to the browser context, recipe steps execute inside the webview DOM (with ad popups
     blocked natively via `new_window_req_handler`) and the resulting file download is intercepted
     (`download_started_handler`) and handed off to our segmented Rust HTTP engine.
  5. Zero-click alternatives: the browser extension (M8) forwards links together with cookies from the user's already-verified
     daily browser session; optional cookie import; hoster premium accounts.
- `ruscello-webview` runs as a **separate child process** spawned on demand (`tao`'s event loop cannot share the main
  thread with `iced`'s `winit` loop, and terminating the helper when idle saves RAM).

## Consequences
- For batch downloads behind a challenge, the user typically performs at most one check per session; the rest of the
  batch proceeds automatically without popups or ChromeDriver breakage.
