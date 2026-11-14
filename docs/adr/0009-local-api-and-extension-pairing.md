# ADR-0009 — Local API, LAN Remote Web UI, and Browser Extension Pairing

- Status: Accepted (2026-11-14)

## Context

Milestone M8 introduces remote control and browser integration for Ruscello:

1. A local loopback HTTP/WebSocket API for the browser extension and CLI (`M8.1`).
2. An optional LAN Remote Web UI served directly from the desktop binary (`M8.2`).
3. A Manifest V3 browser extension for Chrome/Chromium and Firefox supporting download interception, context menus, domain cookie forwarding, and one-click pairing (`M8.3`–`M8.4`).

Exposing an HTTP server on a desktop machine creates specific security and usability constraints:

- Arbitrary websites visited in the user's browser must not be able to reach local API routes via cross-origin requests or DNS rebinding.
- LAN remote control must remain disabled by default, support both frictionless home-LAN access (no password) and password protection, and resist brute-force password guessing.
- Extension pairing must work without manual JSON editing or secret files, while still preventing arbitrary local or web clients from obtaining long-lived credentials silently.

## Decision

### 1. Dual-Listener `axum` Architecture (`ruscello-api`)

`ruscello-api` manages two independent Tokio HTTP listeners sharing a single `EngineHandle` and telemetry hub:

- **Loopback listener (`127.0.0.1:<local_port>`, default `9667`, fallback ephemeral when port is `0`)**:
  - Active while the desktop engine is running.
  - Enforces strict CORS and `Origin` validation: requests presenting an `Origin` header are accepted only for browser extension origins (`chrome-extension://*` or `moz-extension://*`); web origins (`http://*`, `https://*`) are rejected with `403 Forbidden`.
  - Authenticated REST/WebSocket/SSE endpoints require a Bearer token (`Authorization: Bearer <token>` or WebSocket query/subprotocol token for clients unable to set custom WS headers).
  - Writes loopback discovery metadata (`<data_dir>/local-api.json` with `0600` Unix permissions containing port, PID, and a CLI Bearer token) so same-user CLI invocations can connect without exposing tokens to untrusted web pages.
- **LAN Remote Control listener (`0.0.0.0:<lan_port>`, default `9666`)**:
  - Disabled by default (`lan_enabled = false`) and dynamically started, stopped, or re-bound at runtime from desktop Preferences without restarting the app.
  - Serves the mobile-friendly Web UI SPA embedded directly via `include_str!` alongside `/api/v1/*` REST, SSE (`/api/v1/events`), and WebSocket (`/api/v1/ws`) endpoints.

### 2. LAN Authentication & Brute-Force Protection

- When `lan_auth = false`, all LAN API and Web UI routes are accessible immediately without login (trusted home LAN mode).
- When `lan_auth = true`, protected routes require either a valid `HttpOnly; SameSite=Lax; Path=/` session cookie (`ruscello_session`) or Bearer token issued by `POST /api/v1/auth/login`.
- Passwords/PINs are hashed with salted PBKDF2-HMAC-SHA256 (`100_000` iterations, 16-byte random salt, pure Rust `sha2` + `getrandom`, constant-time equality check) and verified in `spawn_blocking` so the async runtime never blocks.
- Login and pairing endpoints enforce a sliding-window per-client-IP rate limiter (`5` failed attempts per `60` seconds → `429 Too Many Requests` with `Retry-After`).

### 3. One-Click Browser Extension Pairing Protocol

Extension pairing uses a dedicated `ExtensionToken` (256-bit random Bearer secret persisted hashed on disk with `0600` permissions) distinct from the CLI token:

- **Desktop-armed one-click**: The user clicks **Start 1-click pairing** in *Settings → Integrations*, opening a time-limited (`120s`) pairing window (and optionally displaying a 6-digit verification PIN). When the extension sends `POST /api/v1/pair/request` (strict extension `Origin` required), if either the pairing window is armed without requiring a PIN or the submitted 6-digit PIN matches, the server immediately issues and persists the extension Bearer token.
- **Extension-initiated one-click**: If the desktop pairing window is not yet armed, `POST /api/v1/pair/request` creates a bounded pending request and returns a `request_id` plus `202 Accepted`. The desktop GUI surfaces a one-click **Approve / Reject** prompt under *Settings → Integrations*. While waiting, the extension polls `GET /api/v1/pair/status/{request_id}` (or completes when approved) and stores the returned Bearer token in `chrome.storage.local`.
- Paired tokens can be inspected and revoked at any time from desktop Settings.

### 4. Cookie Forwarding & LinkGrabber Integration

Authenticated extension requests to `POST /api/v1/cookies`, `POST /api/v1/downloads`, and `POST /api/v1/linkgrabber/analyze` forward domain cookies (formatted as HTTP `Cookie` pairs), `User-Agent`, and `Referer` into `Resolver`'s domain session store (ADR-0004) via `EngineHandle::import_session_cookies`, immediately unlocking matching downloads in `WaitingUser` status. LinkGrabber analysis requests can return parsed candidates directly and optionally emit a UI bridge event to open the desktop *Add* dialog.

## Alternatives

- **mDNS/Zeroconf discovery on LAN**: Deferred; displaying the resolved LAN URL (`http://<lan-ip>:<port>`) and scannable QR code in Settings works across Android/iOS cameras without extra platform firewall rules or runtime dependencies.
- **Native Messaging hosts (`stdio`)**: Requires per-browser OS registry/JSON manifests outside the portable binary and complicates unpacked installation; localhost HTTP + strict extension-origin CORS + explicit pairing works identically on Linux, Windows, and macOS for both Chrome/Chromium and Firefox.
- **Unauthenticated localhost API**: Rejected because any local web page or script could queue downloads or overwrite settings without consent.

## Consequences

- `ruscello-api` remains cleanly downstream of `ruscello-engine` and `ruscello-core`, introducing no cyclic dependencies and requiring no Node.js/Python runtime in the desktop binary.
- All user-facing strings in the Web UI and browser extension are maintained in both English (`en`) and Italian (`it`), synchronized with desktop Preferences.
- Extension manifest origins (`chrome-extension://` and `moz-extension://`) plus explicit pairing prevent drive-by browser exploitation while preserving single-click setup.
