# ADR-0006 — Persistence: SQLite + Per-Download Control File

- Status: Accepted (2026-11-12)

## Context
Core requirement: never lose or corrupt an in-progress download if the PC powers off, the process crashes, or the
network drops.

## Decision
- **SQLite** (`rusqlite` with `bundled` feature, WAL mode, `synchronous=NORMAL`) stores the download queue, packages,
  categories, rules, settings, and history, accessed via a dedicated actor performing batched writes.
- **Per-download control file** (`<filename>.rscpart.ctl`) stored next to the partial data file (`<filename>.rscpart`)
  records the exact byte ranges flushed to disk, remote validators (`ETag`, `Last-Modified`, total size), and source
  metadata needed to re-resolve expired links. Uses a versioned binary format (`postcard`/`bincode`) protected by a
  CRC32 checksum and updated via atomic *write-temp + fsync + rename*.
- **Strict checkpoint invariant**: call `sync_data()` on `.rscpart` **before** updating `.rscpart.ctl`.
- On startup: reconcile SQLite state with on-disk `.rscpart.ctl` files (the control file is authoritative for persisted
  byte ranges).

## Alternatives Considered
- SQLite only: high-frequency byte-range updates create unnecessary DB write amplification, and a corrupted/deleted DB
  would orphan partial downloads.
- `redb` / `sled`: good embedded KV stores, but SQLite offers relational queries for filtering/search and universal
  inspection tooling.

## Consequences
- Partial downloads can be recovered and resumed even if the central SQLite database is lost or corrupted.
