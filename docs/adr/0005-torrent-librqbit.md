# ADR-0005 — Torrent con librqbit

- Stato: Accettato (2026-11-12)

## Contesto
Serve un motore BitTorrent completo (magnet, DHT, PEX, trackers, UPnP, selezione file, seeding) in Rust.

## Decisione
Usare **`librqbit`** (progetto rqbit, Apache-2.0, ≥ 9.x, edition 2024), incapsulato in `ruscello-torrent`
dietro un'interfaccia nostra (così l'engine non dipende dai tipi di librqbit e un cambio futuro resta locale).

## Alternative
- `libtorrent-rasterbar` via FFI (C++, quello di qBittorrent): più maturo ma non Rust, build complessa.
- `cratetorrent` e altri: meno completi/mantenuti.

## Conseguenze
- Funzionalità mancanti in librqbit (es. filtri IP, alcuni limiti) vanno verificate in M4; se necessario
  contribuire upstream o implementare nel wrapper.
