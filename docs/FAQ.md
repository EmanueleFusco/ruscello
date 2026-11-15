# Frequently asked questions

### Can I resume an interrupted download?

Resume works when the server supports byte ranges for the same representation. Ruscello reconciles the queue
against the durable partial file and checkpoint on launch. If the server changes the file or disallows ranges,
the relevant transfer starts again. Leave the `.rscpart` and `.rscpart.ctl` files together.

### Why does a hoster require my input?

For a login or verification page, use **Open verification** and complete the request in the browser helper.
Ruscello reuses that site's session for waiting links. It also respects hoster countdowns. If the helper is
unavailable, check the platform prerequisites in the [quickstart](QUICKSTART.md).

### Do I need yt-dlp or ffmpeg?

HTTP, BitTorrent, and native HLS and DASH work without them. **Settings → Integrations** can manage optional
`yt-dlp` and `ffmpeg` binaries with your consent, or use an executable you provide. Some sites need `yt-dlp`
for metadata, and combining separate audio and video tracks may require `ffmpeg`.

### Why is the one-click update button missing?

Look for a verified package matching your OS, CPU architecture, and installation type. NSIS on Windows,
AppImage on Linux, and installed macOS app bundles support in-app replacement. Debian packages use the
system package manager. Copy the app out of the mounted disk image before using macOS updates,
and keep the package in a location you can write to.

### How can I control downloads from another device?

Enable the LAN Web UI in **Settings → Integrations**, then scan the QR code or open the displayed URL
on your local network. Keep password protection enabled when other people share that network.
Use the [browser extension guide](../extensions/browser/README.md) for pairing and link forwarding.

### What should I include when reporting a problem?

**Settings → Advanced** can open the logs or copy diagnostics with your platform and version.
Remove private paths, links with tokens, cookies, and credentials before attaching logs. Report suspected
vulnerabilities privately using the [security policy](../SECURITY.md).
