# Ruscello

> Download manager desktop **tutto in Rust**: torrent + HTTP/hoster + streaming, veloce, riprendibile e
> leggero. Unisce ciò che fanno qBittorrent e JDownloader in un'unica interfaccia minimale.

**Stato:** in sviluppo — fase di bootstrap (M0). Vedi la [roadmap](docs/PLAN.md).

## Obiettivi

- 🧲 **Torrent**: magnet, .torrent, selezione file, seeding con limiti (librqbit).
- ⚡ **HTTP veloce**: segmentazione dinamica multi-connessione con work stealing, mirror, limiti di banda.
- 🛡️ **Mai perdere un download**: ripresa dopo spegnimento, crash o caduta della rete; file mai corrotti.
- 🔗 **Molti siti**: plugin per hoster, ricette configurabili, HLS/DASH, yt-dlp opzionale.
- 🔎 **LinkGrabber**: analizza una pagina ed evidenzia i file che probabilmente vuoi scaricare.
- 🤖 **Automatico**: code, retry, verifica hash, estrazione archivi, azioni a fine coda.
- 🧑‍💻 **Interfaccia** minimale e professionale, download divisi per tipologia e categoria.
- 🪶 **Leggero**: pochi MB di RAM a riposo, CPU ~0 quando non scarica.

Piattaforme: Windows e Linux (priorità), macOS.

## Sviluppo

```bash
cargo run -p ruscello-gui            # GUI
cargo run -p ruscello-cli -- --help  # CLI
./scripts/check.sh                   # (o scripts/check.ps1 su Windows) stessi controlli della CI
```

Documentazione per chi sviluppa (persone e agenti): [AGENTS.md](AGENTS.md), [Architettura](docs/ARCHITECTURE.md),
[UI](docs/UI.md), [Test](docs/TESTING.md), [ADR](docs/adr/).

## Licenza

[GPL-3.0-or-later](LICENSE)
