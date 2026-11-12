# ADR-0003 — Engine in-process, tokio, motore HTTP segmentato proprio

- Stato: Accettato (2026-11-12)

## Contesto
Serve un motore veloce (multi-connessione, work stealing), crash-safe e controllabile dalla GUI senza
bloccarla. Un daemon separato aggiunge complessità (IPC, ciclo di vita) che oggi non serve.

## Decisione
- `ruscello-engine` è una **libreria** eseguita su un runtime tokio multi-thread nello stesso processo della
  GUI (o della CLI). Interfaccia unica: `EngineHandle` (comandi `mpsc`, eventi `broadcast` aggregati).
- Motore HTTP **scritto da noi** sopra `reqwest`/`hyper`, con allocatore segmenti stile aria2/IDM e
  persistenza propria (ADR-0006). Crate come `gosh-dl` possono essere studiati, ma vogliamo pieno controllo
  di resume, re-resolve dei link scaduti e limiti per hoster.

## Alternative
- Daemon separato con API (stile qBittorrent-nox / aria2 RPC): possibile in futuro riusando `ruscello-api`;
  la separazione di crate lo rende fattibile senza riscritture.
- Usare aria2 come sidecar: non Rust, meno controllo.

## Conseguenze
- Un crash della GUI ferma i download: mitigato dalla persistenza (ripresa automatica al riavvio) e da
  `panic = unwind` + cattura dei panic nei task.
