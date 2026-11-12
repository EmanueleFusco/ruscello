# ADR-0002 — Siti video/streaming: estrattori nativi + yt-dlp come sidecar opzionale

- Stato: Accettato (2026-11-12)

## Contesto
"Scaricare da quanti più siti possibile". yt-dlp supporta più di 1800 siti ed è aggiornato di continuo,
ma è scritto in Python. Riscrivere gli estrattori in Rust non è sostenibile.

## Decisione
- Il **motore di download, merge HLS/DASH, gestione coda, UI** restano in Rust.
- **yt-dlp** è un *sidecar opzionale*: con il consenso dell'utente l'app scarica il binario ufficiale
  (verifica checksum), lo aggiorna automaticamente e lo usa per l'estrazione (`-J`). Dove possibile il
  download vero viene eseguito dal nostro motore usando gli URL/header dei formati; altrimenti yt-dlp
  scarica e l'app ne legge il progresso.
- **ffmpeg** sidecar opzionale per merge/remux.
- Estrattori nativi in Rust per HLS/DASH generici e per gli hoster di file (plugin/ricette, ADR resolver).

## Conseguenze
- L'app funziona senza sidecar (solo con meno siti video supportati); l'UI lo spiega nelle Integrazioni.
- I binari esterni non vengono linkati → nessun problema di licenza.
