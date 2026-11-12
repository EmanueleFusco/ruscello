# Ruscello

> Desktop download manager written **entirely in Rust**: BitTorrent + HTTP/hoster + media streaming — fast,
> crash-safe, resumable, and lightweight. Unites the capabilities of qBittorrent and JDownloader in a single,
> minimal, professional interface.

**Status:** The M1 HTTP engine is available through the developer CLI. See the [Work Plan & Roadmap](docs/PLAN.md).

## Key Features

- 🧲 **BitTorrent**: magnet links, `.torrent` files, selective file downloading, seeding limits (`librqbit`).
- ⚡ **High-Speed HTTP**: dynamic multi-connection segmentation with work stealing, mirror support, bandwidth limiting.
- 🛡️ **Zero Lost Downloads**: crash-safe resume after PC power-off, process crash, or network drop; strict data integrity.
- 🔗 **Broad Site & Container Support**: native hoster plugins (`fuckingfast`, `XFileSharing`, `datanodes`, etc.), JDownloader **`.dlc` container decryption**, declarative TOML recipes, native HLS/DASH, optional `yt-dlp` sidecar.
- 🔎 **LinkGrabber**: analyzes web pages and highlights the files you most likely want to download with explainable scoring.
- 📱 **LAN Remote Web UI**: optional qBittorrent-style web interface accessible from your phone or another device on the local network (with or without password, plus instant QR code pairing).
- 🤖 **Hands-Off Automation**: queues, retries, checksum verification, multi-part archive extraction, post-queue actions.
- 🧑‍💻 **Clean Professional UI**: built with `iced`, organizing downloads by automatic File Types and user Categories.
- 🌐 **Bilingual Interface**: available in **English** and **Italian** (user-selectable in Settings).
- 🪶 **Lightweight**: low idle RAM footprint, ~0% CPU when idle.

Target platforms: Windows and Linux (first-class priority), macOS.

## Development

```bash
cargo run -p ruscello-gui            # GUI application
cargo run -p ruscello-cli -- --help  # Headless CLI
./scripts/check.sh                   # (or scripts/check.ps1 on Windows) run all CI checks locally
```

Developer & agent documentation: [AGENTS.md](AGENTS.md), [Architecture](docs/ARCHITECTURE.md),
[UI Specs](docs/UI.md), [Testing Strategy](docs/TESTING.md), [ADRs](docs/adr/).

### Download an HTTP file

```bash
cargo run -p ruscello-cli -- get <url> -o downloads -x 16
cargo run -p ruscello-cli -- --language it --help
```

Progress appears on standard error in a terminal. Use `--progress` to display it when redirected or
`--quiet` to suppress it. Standard output contains the path to the completed file.
Press Ctrl+C to interrupt a download. Run the same command to resume when the server supports validated byte ranges.
Use `--checksum sha256:<hex>` to verify the completed file, and repeat `--mirror <url>` to add verified sources.

## License

[GPL-3.0-or-later](LICENSE)
