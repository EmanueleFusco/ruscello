# Ruscello Browser Extension (Manifest V3)

Cross-browser WebExtension (Chrome / Chromium / Edge and Firefox) connecting to the local Ruscello desktop API (`127.0.0.1:9667` by default).

## Features

- **One-click pairing** with desktop Ruscello (*Settings → Integrations → Connect browser extension*), supporting both desktop-armed pairing windows (+ optional 6-digit PIN) and extension-initiated approval prompts.
- **Optional download interception** toggled from the popup, with configurable minimum file size and MIME type prefix filters (`options.html`).
- **Context menus**:
  - `Download with Ruscello` / `Scarica con Ruscello` on links and text selections.
  - `Analyze page with LinkGrabber` / `Analizza pagina con LinkGrabber` on pages.
- **Automatic domain session forwarding**: forwards domain cookies, `User-Agent`, and `Referer` to Ruscello's resolver session store so authenticated hoster and direct links resume seamlessly without extra verification.
- **Bilingual UI**: localized in English (`en`) and Italian (`it`).

## Loading Unpacked for Development

### Chromium / Chrome / Edge
1. Open `chrome://extensions` and enable **Developer mode**.
2. Click **Load unpacked** and select `extensions/browser/`.

### Firefox (MV3 `background.scripts`)
1. Copy `manifest.firefox.json` over `manifest.json` in a staging folder (keeping `background.js`, `popup.*`, `options.*`, `i18n.js`, `styles.css`, `_locales/`, and `icons/`).
2. Open `about:debugging#/runtime/this-firefox`, click **Load Temporary Add-on…**, and pick the staged `manifest.json`.
