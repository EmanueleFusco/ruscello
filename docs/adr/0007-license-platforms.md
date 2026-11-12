# ADR-0007 — Licenza GPL-3.0-or-later e piattaforme

- Stato: Accettato (2026-11-12)

## Decisione
- Licenza **GPL-3.0-or-later**: permette di studiare e riusare codice da progetti GPL (qBittorrent,
  JDownloader) con attribuzione. Dipendenze verificate con `cargo-deny` (`deny.toml`).
- Piattaforme: **Windows e Linux con priorità pari**, macOS supportato (CI su tutti e tre, test su tutti e
  tre). Funzioni specifiche di piattaforma isolate dietro trait/moduli `cfg`.
- Repository GitHub inizialmente privato, poi pubblico (vedi PLAN M10.7).

## Conseguenze
- Niente linking statico di librerie con licenze incompatibili (es. UnRAR): usare binari esterni opzionali.
