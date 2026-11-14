# Security policy

Use GitHub's [private vulnerability reporting form](https://github.com/EmanueleFusco/ruscello/security/advisories/new)
for suspected vulnerabilities in the latest stable Ruscello release or current `main`.
Include the version, operating system, impact, and a minimal reproduction with synthetic data. Redact download
URLs containing credentials, cookies, API tokens, local paths, and personal files. For fixes and supported
builds, see the [security advisories](https://github.com/EmanueleFusco/ruscello/security/advisories)
and [releases](https://github.com/EmanueleFusco/ruscello/releases).

The updater uses GitHub's HTTPS release API and matches the exact platform asset for the installed package.
It verifies the asset size and SHA-256 reported by GitHub before closing the queue and replacing the application.
Treat access to the GitHub repository and its release workflow as release-signing authority. Release assets also
include `SHA256SUMS` for manual verification.

The local API uses a per-installation token. The LAN interface is opt-in and should remain on a trusted network.
Hoster credentials are kept in the operating system credential store. Keep OS webview runtimes current.
