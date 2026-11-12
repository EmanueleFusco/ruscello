# Browser Extension (M8)

Manifest V3 browser extension for Chrome/Chromium and Firefox. Kept intentionally minimal (JS/TS is required by
WebExtension APIs); all core logic lives in the Rust desktop application.

Planned features (see `docs/PLAN.md` M8):
- context menu "Download with Ruscello" (links, selection, full page → LinkGrabber);
- optional interception of browser downloads (filtered by file type / minimum size);
- forwarding of domain cookies, referer, and User-Agent from the user's active browser session for authenticated or verified downloads;
- simple pairing with the desktop app (local token-authenticated API or Native Messaging — to be finalized in an ADR).

Not scheduled for implementation prior to M8.
