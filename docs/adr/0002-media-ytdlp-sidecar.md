# ADR-0002 — Video & Streaming Sites: Native Extractors + Optional yt-dlp Sidecar

- Status: Accepted (2026-11-12)

## Context
Requirement: "download from as many sites as possible". `yt-dlp` supports 1800+ sites and is continuously updated by a
large community, but is written in Python. Rewriting and maintaining 1800+ site extractors in pure Rust is unsustainable.

## Decision
- All **download execution, HLS/DASH segment merging, queue management, and UI** remain in pure Rust.
- **yt-dlp** is supported as an *optional external sidecar*: with explicit user consent in Settings, the app downloads
  the official standalone release binary (verifying its SHA-256 checksum), keeps it updated automatically, and invokes
  it for metadata/format extraction (`yt-dlp -J`). Whenever direct format URLs + headers allow, our native Rust engine
  performs the download; otherwise `yt-dlp` executes the download and Ruscello parses its progress output.
- **ffmpeg** is supported as an optional sidecar for audio+video muxing/remuxing.
- Native Rust extractors are built in-house for generic HLS/DASH streams and file hosters (plugins and TOML recipes).

## Consequences
- Ruscello works out of the box without any sidecar (with fewer video streaming sites supported); the Settings →
  Integrations page clearly explains and manages sidecars.
- External binaries are invoked as subprocesses and never linked → full license isolation.
