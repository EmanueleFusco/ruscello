# Specifiche UI

Obiettivo: aspetto **professionale, calmo e affidabile** (riferimenti: qBittorrent per la densità
informativa, app moderne come Motrix/AB Download Manager/Linear per la pulizia). Minimale non vuol dire
vuota: tutte le informazioni utili ci sono, ma i comandi rari stanno nei menu contestuali o nelle
impostazioni, non in barre di pulsanti.

## Layout finestra principale

```
┌──────────────────────────────────────────────────────────────────────────────┐
│  [+ Aggiungi]   🔍 Cerca download…                       ⏸ Tutti  🐢  ⚙        │  ← toolbar (unica riga)
├───────────────┬──────────────────────────────────────────────────────────────┤
│ STATO         │ Nome                     Dimensione  Progresso   Velocità ETA │
│  Tutti     42 │ ▸ Pacchetto: Gioco X (12)   48.2 GB  ███████░ 71%  38 MB/s 9m │
│  Attivi     3 │   ubuntu-24.04.iso           5.7 GB  ████████ 100%    —     — │
│  In coda    5 │   film.mkv (torrent)        12.1 GB  ███░░░░░ 34%  11 MB/s 30m│
│  Completati30 │   report.pdf                 2.1 MB  ⚠ Verifica richiesta     │
│  Errori     1 │                                                              │
│  Seeding    3 │                                                              │
│ TIPOLOGIE     │                                                              │
│  Video        │                                                              │
│  Audio        │                                                              │
│  Archivi      │                                                              │
│  Programmi    │                                                              │
│  Documenti    │                                                              │
│  Immagini     │                                                              │
│  Torrent      │                                                              │
│ CATEGORIE     ├──────────────────────────────────────────────────────────────┤
│  Giochi       │ Dettagli: Generale · Segmenti · File · Peer · Tracker · Log   │ ← pannello richiudibile
│  Film & Serie │ (contenuto contestuale al download selezionato)             │
│  + Nuova      │                                                              │
├───────────────┴──────────────────────────────────────────────────────────────┤
│ ↓ 49.3 MB/s  ↑ 1.2 MB/s   Limite: off   Disco D: 412 GB liberi   DHT: 312 nodi │ ← status bar
└──────────────────────────────────────────────────────────────────────────────┘
```

- **Sidebar** con tre sezioni (Stato, Tipologie, Categorie) e contatori; le sezioni sono filtri combinabili
  (es. Attivi + Video). Tag opzionali in una quarta sezione solo se l'utente li usa.
- **Lista** virtualizzata (migliaia di righe senza lag); colonne configurabili con menu sul header;
  Package espandibili; icona di tipologia a sinistra del nome; progress bar sottile e colorata per stato
  (accento = attivo, grigio = in pausa, verde = completato, ambra = attende utente, rosso = errore).
- **Azioni sulla riga**: hover mostra 2–3 icone discrete (pausa/riprendi, apri cartella, ⋯); tutto il resto
  nel menu contestuale (tasto destro / ⋯).
- **Toolbar**: solo Aggiungi, ricerca, pausa/riprendi tutti, modalità velocità ridotta (🐢, come
  qBittorrent "alternative speed limits"), impostazioni.
- **Pannello dettagli** richiudibile (stato ricordato): Generale (URL, cartella, hash, date), Segmenti
  (visualizzazione grafica dei segmenti/pezzi e connessioni), File/Peer/Tracker solo per torrent, Log del
  download.

## Dialog "Aggiungi" (unico punto di ingresso)

1. Campo multilinea: incolla link, magnet, testo libero con link, o trascina file `.torrent`/`.txt`.
   Precompilato dagli appunti se contengono link (senza aggiungere nulla in automatico).
2. Analisi immediata (spinner per riga): ogni input diventa una riga con tipo rilevato (Diretto, Hoster
   «nome», Torrent, Video, Pagina web → LinkGrabber).
3. Per pagine web: vista **LinkGrabber** (vedi PLAN M5.5) con gruppi *Probabili ⭐* (pre-selezionati),
   *Altri file*, *Media*, *Pagine collegate*; ogni riga mostra nome, dimensione, host, motivo del punteggio.
4. Sotto: Categoria (auto-suggerita dalle regole), Cartella, "Avvia subito" (default on), opzioni avanzate
   collassate (connessioni, limite velocità, checksum, password archivio).

## Verifiche e interazioni richieste

- Mai finestre modali improvvise. Un download che richiede l'utente passa a "⚠ Verifica richiesta"; una
  notifica di sistema e un badge nella sidebar lo segnalano; click → si apre la finestra del browser helper
  già sulla pagina giusta. Completata la verifica la finestra si chiude e il batch riparte.

## Stile visivo

- Tema chiaro/scuro che segue il sistema (override in impostazioni). Palette neutra (grigi caldi), un solo
  colore di accento (blu/teal), colori semantici per stato. Niente gradienti vistosi, ombre leggere,
  angoli 6–8 px, animazioni brevi (≤ 150 ms) solo per feedback (hover, espansione).
- Tipografia: Inter (inclusa), 13–14 px corpo, numeri tabulari per velocità/dimensioni.
- Icone: Lucide (ISC) come icon font, monocromatiche.
- Densità: righe 32–36 px; modalità compatta opzionale.
- Accessibilità: contrasto AA, navigazione da tastiera completa, tooltip su icone.

## Scorciatoie

`Ctrl+N` aggiungi · `Ctrl+V` (finestra principale) incolla e aggiungi · `Spazio` pausa/riprendi ·
`Canc` annulla/rimuovi · `Ctrl+F` cerca · `Invio` apri file · `Ctrl+Invio` apri cartella · `Ctrl+,` impostazioni.

## Impostazioni (poche pagine)

Generale (lingua, tema, avvio con il sistema, tray, azione a fine coda) · Download (cartella default,
download simultanei, connessioni per file/host, temp dir) · Velocità (limiti, modalità ridotta, pianificatore) ·
Torrent (porta, DHT/PEX, UPnP, seeding ratio/tempo, interfaccia di rete) · Categorie & regole ·
Integrazioni (estensione browser, yt-dlp/ffmpeg con aggiornamento automatico, monitor appunti) ·
Account hoster · Avanzate (log, proxy, user agent, DNS, reset).
