# ADR-0012 — BitTorrent peer budget and transport

- Status: Accepted (2026-11-17)

## Context

librqbit 9.1.0 limits connections per torrent. Ruscello needs a session-wide limit so several downloads or seeds can share a bounded pool. The library also supports µTP, but defaults to TCP while µTP matures.

## Decision

Keep the single Rust session from ADR-0005. Use defaults of 100 peers per torrent and 500 across the session. The session budget covers incoming peers admitted to a torrent, outgoing peer attempts, and outgoing magnet metadata probes. Incoming handshakes before torrent admission retain librqbit's separate handshake limit. Connection slots release with the peer tasks.

Patch the pinned Apache-2.0 librqbit locally until the session limit can be consumed from upstream. TCP remains enabled in both modes. Offer TCP + µTP as an experimental override. In this mode, use a separate ephemeral DHT UDP port because the library runs DHT and µTP on independent sockets.

## Alternatives

Dividing a fixed budget into per-torrent limits would reserve slots for idle torrents and need repeated torrent restarts when the active set changes. Replacing librqbit with a C++ engine would conflict with ADR-0005.

## Consequences

The patch must be reviewed when updating librqbit. The pinned UPnP helper requests TCP mappings. Local µTP interoperability does not establish inbound reachability behind a NAT. Compare transport reachability, sustained throughput and recovery after a macOS wake before changing the transport default.
