# ADR-0007 — License (GPL-3.0-or-later) & Target Platforms

- Status: Accepted (2026-11-12)

## Decision
- License: **GPL-3.0-or-later**. This allows studying and adapting algorithms/patterns from GPL-licensed reference
  projects (such as qBittorrent and JDownloader) with proper attribution. Dependency license compatibility is enforced
  in CI via `cargo-deny` (`deny.toml`).
- Target platforms: **Windows and Linux as equal first-class targets**, with **macOS** supported and tested in the
  3-OS CI matrix. Platform-specific OS integrations (system tray, sleep inhibition, file associations) are isolated
  behind traits and `cfg` modules.
- Repository visibility: initially private during early development, designed to become public (see `PLAN.md` M10.7).

## Consequences
- Libraries with GPL-incompatible licenses (such as proprietary UnRAR source) must never be statically or dynamically
  linked; external tools (`7z`/`unrar`) may only be invoked as optional separate processes.
