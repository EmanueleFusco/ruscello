# ADR-0005 — BitTorrent Engine: librqbit

- Status: Accepted (2026-11-12)

## Context
Ruscello requires full BitTorrent support (magnet links, DHT, PEX, HTTP/UDP trackers, UPnP/NAT-PMP, selective file
downloading, piece priorities, seeding limits) in pure Rust.

## Decision
Use **`librqbit`** (the engine behind `rqbit`, Apache-2.0 license, ≥ 9.x, Rust 2024 edition), encapsulated inside
`ruscello-torrent` behind our own domain trait/types so `ruscello-engine` does not leak `librqbit` internals.

## Alternatives Considered
- `libtorrent-rasterbar` via C++ FFI (used by qBittorrent): very mature, but introduces a heavy C++/Boost build chain
  and violates the pure-Rust goal.
- Older Rust torrent crates (`cratetorrent`, `bip_bencode`, etc.): unmaintained or incomplete.

## Consequences
- Any torrent features not yet exposed by `librqbit` (e.g., specific IP blocklist formats or fine-grained seeding quotas)
  will be evaluated in M4 and either implemented in the `ruscello-torrent` wrapper or contributed upstream.
