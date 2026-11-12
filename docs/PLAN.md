# Ruscello — Piano di lavoro

> Download manager desktop **tutto in Rust** che unisce un client BitTorrent (stile qBittorrent) e un
> download manager HTTP/hoster (stile JDownloader / IDM), con interfaccia minimale e professionale.
>
> Questo documento è la **fonte di verità** per l'avanzamento. Ogni milestone ha obiettivi, task,
> criteri di accettazione ("Definition of Done") e test richiesti. Le milestone corrispondono alle
> *GitHub Milestones* del repository; i task principali corrispondono a *GitHub Issues*.
> Aggiornare le checkbox man mano (vedi `AGENTS.md`).

---

## 0. Requisiti (dal committente)

| # | Requisito | Dove viene soddisfatto |
|---|-----------|------------------------|
| R1 | Torrent (magnet, .torrent, selezione file, seeding) | M4 |
| R2 | Download HTTP/HTTPS/FTP da "quanti più siti possibile" | M1, M5, M6, M7 |
| R3 | Pausa, ripresa, annullamento | M1, M2 |
| R4 | Nessuna perdita se si spegne il PC o cade la connessione (crash-safe resume) | M1 (cuore del progetto) |
| R5 | Massima velocità: segmentazione dinamica, multi-connessione, mirror, riuso connessioni | M1, M10 |
| R6 | Massima automazione (code, retry, riconnessione, post-processing, regole) | M2, M9 |
| R7 | Analisi pagina → elenco contenuti con evidenziati i file probabili (LinkGrabber) | M5 |
| R8 | Pagine protette da verifiche anti-bot (Cloudflare ecc.) | M6 — **approccio human-in-the-loop**, vedi ADR-0004 |
| R9 | UI gradevole, minimale, professionale; download divisi per tipologia e categoria | M3 + `docs/UI.md` |
| R10 | Efficiente con le risorse | Budget in §2, verificato in M10 |
| R11 | Stabile e testato | `docs/TESTING.md`, gate CI su ogni PR |
| R12 | Tutto in Rust (UI + backend) | ADR-0001, ADR-0002 (eccezioni: estensione browser JS minima, sidecar opzionali yt-dlp/ffmpeg) |
| R13 | Windows e Linux priorità pari; macOS compatibile | CI matrix su 3 OS, ADR-0007 |
| R14 | Casi d'uso degli script personali (batch da hoster tipo fuckingfast / XFileSharing) | M5 + M6, vedi `docs/reference/legacy-scripts/README.md` |

---

## 1. Decisioni architetturali (riassunto — dettagli in `docs/adr/`)

- **GUI**: [iced](https://github.com/iced-rs/iced) ≥ 0.14 (Elm architecture, reactive rendering, MIT). ADR-0001.
- **Runtime async**: tokio. Engine come libreria in-process; la GUI parla con l'engine tramite `EngineHandle`
  (comandi via `mpsc`, eventi via `broadcast`). ADR-0003.
- **HTTP**: `reqwest` (hyper + rustls) con motore di segmentazione proprio (algoritmo stile aria2/IDM con
  *work stealing* dinamico). ADR-0003.
- **Torrent**: [`librqbit`](https://github.com/ikatson/rqbit) (Apache-2.0, ≥ 9.x). ADR-0005.
- **Persistenza**: SQLite (`rusqlite`, bundled, WAL) per stato/coda/impostazioni + file di controllo
  per-download per la bitmap dei segmenti. ADR-0006.
- **Siti video/streaming**: estrattori nativi Rust per HLS/DASH + **yt-dlp come sidecar opzionale** gestito
  automaticamente dall'app (download/aggiornamento). ffmpeg opzionale per merge/remux. ADR-0002.
- **Pagine con verifica anti-bot / login**: processo helper `ruscello-webview` (wry) in cui **l'utente**
  completa la verifica; cookie/sessione vengono poi riutilizzati automaticamente. Niente risoluzione
  automatica di captcha, niente stealth/fingerprint spoofing. ADR-0004.
- **Licenza**: GPL-3.0-or-later. ADR-0007.

---

## 2. Budget di risorse e prestazioni (obiettivi misurabili, verificati in M10)

| Metrica | Obiettivo |
|---------|-----------|
| RAM a riposo (GUI aperta, 0 download attivi, webview chiuso) | < 80 MB |
| RAM con 10 download HTTP attivi | < 150 MB |
| CPU a riposo | ~0% (nessun redraw/poll senza eventi) |
| CPU a 1 Gbit/s su singolo file | < 15% di un core (escluso I/O kernel) |
| Avvio a freddo fino a finestra utilizzabile | < 1 s |
| Aggiornamenti UI progress | max 4 Hz per riga visibile, aggregati |
| Perdita dati dopo kill -9 / spegnimento | ≤ ultimi ~2 s di dati scaricati (riscaricati), mai corruzione |
| Throughput segmentato vs aria2 (`-x16 -s16`) su server locale con latenza simulata | ≥ 95% |

---

## 3. Roadmap

```
M0 Bootstrap ─► M1 Motore HTTP ─► M2 Coda & Storage ─► M3 GUI v1 ─┬─► M4 Torrent
                                                                   ├─► M5 Resolver & LinkGrabber ─► M6 Browser helper
                                                                   └─► M7 Media (HLS/DASH/yt-dlp)
                                 M8 Estensione browser + API locale (dopo M5)
                                 M9 Automazione & post-processing
                                 M10 Hardening, performance, packaging, release 1.0
```

M4, M5, M7 possono procedere in parallelo dopo M3 (anche con più agenti su branch diversi).

---

## M0 — Bootstrap del progetto ✅ (impostato dall'agente di planning)

- [x] Workspace Cargo multi-crate, edition 2024, lint di workspace
- [x] `AGENTS.md`, piano, ADR, documenti UI/test/architettura
- [x] CI GitHub Actions (fmt, clippy, test su Windows/Linux/macOS, cargo-deny)
- [x] Repository GitHub, milestone e issue
- [ ] (M0.1, primo task del prossimo agente) Verificare `cargo check --workspace` verde in CI; aggiungere
      `tracing` + `tracing-subscriber` con log su file a rotazione in `%LOCALAPPDATA%`/`$XDG_STATE_HOME`
      (crate `directories`); struttura errori (`thiserror` nelle librerie, `anyhow` solo nei binari).

---

## M1 — Motore HTTP segmentato e crash-safe (crate `ruscello-http`, `ruscello-testkit`)

**Obiettivo**: scaricare un URL HTTP/HTTPS in modo veloce, interrompibile e ripristinabile in qualunque
momento, senza mai corrompere il file.

### Task
- [ ] **M1.1 Testkit server** (`ruscello-testkit`): server HTTP locale (axum/hyper) con scenari configurabili:
      range supportato/non supportato, `Content-Length` assente, chunked, redirect multipli,
      `Content-Disposition` (anche RFC 5987 `filename*=`), ETag/Last-Modified che cambiano a metà,
      connessione che cade dopo N byte, throttling per-connessione, limite max connessioni per IP
      (risponde 429/503), latenza artificiale, risposte 5xx intermittenti. File deterministici generati
      da seed (nessun file binario nel repo).
- [ ] **M1.2 Probe**: richiesta iniziale (GET con `Range: bytes=0-` preferito a HEAD, perché molti server
      gestiscono male HEAD) → dimensione, supporto range, nome file (Content-Disposition > URL finale >
      MIME → estensione), validatori (ETag, Last-Modified), URL finale dopo redirect.
- [ ] **M1.3 Allocatore segmenti** (puro, senza I/O, testato con `proptest`):
  - split iniziale in N segmenti con `min_segment_size` (default 1 MiB, adattivo);
  - **work stealing**: quando una connessione finisce, divide a metà il segmento rimanente più grande
    (o quello col tempo di completamento stimato più alto) se il residuo > soglia;
  - **slow start delle connessioni**: aggiunge connessioni finché il throughput aggregato cresce
    (> +10%) e fino al limite per-host; le riduce se il server risponde 429/503 o resetta;
  - invarianti: nessuna sovrapposizione, copertura totale, byte completati monotoni.
- [ ] **M1.4 Writer**: file `*.rscpart` preallocato (`set_len`; sparse su NTFS quando possibile), scritture
      posizionali (`FileExt::seek_write`/`write_at`) da un task writer dedicato con buffer coalescenti
      (es. 1–4 MiB per segmento) per evitare micro-scritture; rename atomico al nome finale a fine download;
      gestione collisioni nome (`file (1).ext`) e caratteri illegali per OS.
- [ ] **M1.5 Stato persistente / resume**: file di controllo `*.rscpart.ctl` (bincode/postcard +
      checksum CRC32 + versione formato) con bitmap/ranges completati, scritto con *write-temp + fsync +
      rename* ogni ~2 s o ogni X MiB; ordine garantito: **prima fsync dei dati, poi aggiornamento del
      control file** (così il control file non dichiara mai byte non persistiti). Al resume: verifica
      validatori con `If-Range`; se il file remoto è cambiato → ricomincia (con avviso) invece di
      corrompere.
- [ ] **M1.6 Download senza range**: fallback a connessione singola; se cade, ricomincia da zero (segnalato
      in UI come "non riprendibile").
- [ ] **M1.7 Retry & riconnessione**: backoff esponenziale con jitter, classificazione errori
      (transitorio / permanente / auth / link scaduto → richiede re-resolve dal plugin), rilevamento
      perdita rete e attesa del ritorno della connettività senza consumare tentativi.
- [ ] **M1.8 Limitatore di banda**: token bucket globale + per-download, modificabile a caldo.
- [ ] **M1.9 Mirror**: più URL per lo stesso file (stessa dimensione/hash) → segmenti distribuiti tra mirror.
- [ ] **M1.10 Verifica integrità**: hash opzionale (MD5/SHA-1/SHA-256/CRC32) fornito dall'utente o dal plugin.
- [ ] **M1.11 FTP/FTPS** (crate `suppaftp`) con resume via `REST` — priorità bassa, può slittare a M9.
- [ ] **M1.12 CLI di sviluppo** (`apps/cli`): `ruscello-cli get <url> [-o dir] [-x conns]` per testare il
      motore senza GUI.

### Definition of Done M1
- Test di integrazione su tutti gli scenari del testkit, inclusi:
  - **kill del processo durante il download → riavvio → file finale con SHA-256 corretto** (test che lancia
    `ruscello-cli` come sottoprocesso e lo termina brutalmente);
  - caduta di rete simulata a metà → ripresa automatica;
  - ETag cambiato → nessun file corrotto.
- Benchmark `criterion`/script vs aria2 sul testkit con latenza 50 ms: throughput ≥ 95%.

---

## M2 — Coda, scheduler, storage, categorie (crate `ruscello-engine`, `ruscello-storage`, `ruscello-core`)

- [ ] **M2.1 Modello di dominio** (`ruscello-core`): `Download`, `Package` (gruppo di download come in
      JDownloader), `Source` (Http/Torrent/Media/Hoster), `Status` (Queued, Resolving, WaitingUser,
      Downloading, Paused, Seeding, PostProcessing, Completed, Failed{reason}), `Category`, `Tag`,
      `Priority`. Macchina a stati esplicita con transizioni testate.
- [ ] **M2.2 Storage SQLite** con migrazioni versionate (`rusqlite_migration` o equivalente), WAL,
      scrittura asincrona batch (attore dedicato), backup automatico del DB. Recupero da DB corrotto
      (ricostruzione dai control file).
- [ ] **M2.3 Scheduler**: max download simultanei globali, max connessioni per host, priorità, ordine
      manuale (drag), "forza avvio", retry automatici con limite, ripresa automatica all'avvio dei
      download che erano attivi.
- [ ] **M2.4 API dell'engine**: `EngineHandle` con comandi (`add`, `pause`, `resume`, `cancel{delete_files}`,
      `set_priority`, `move`, `set_limits`…) ed eventi (`Added`, `Progress` aggregati e throttled,
      `StatusChanged`, `Log`). Tutto `Send + Clone`; nessun lock esposto alla GUI.
- [ ] **M2.5 Categorizzazione automatica**:
  - *Tipologia* (automatica, non modificabile): Video, Audio, Immagini, Documenti, Archivi, Programmi,
    Torrent, Altro — da estensione + MIME + magic bytes (`infer`) a fine download;
  - *Categorie* (utente): regole su dominio / estensione / regex / sorgente → cartella di destinazione e
    limiti; categorie predefinite: Giochi, Film & Serie, Musica, Software, Lavoro.
- [ ] **M2.6 Gestione disco**: controllo spazio libero prima e durante il download (pausa automatica con
      avviso se lo spazio sta per finire), cartella temporanea opzionale separata.

### DoD M2
Test unitari della state machine; test di integrazione engine con testkit (coda di 50 download,
limite 3 simultanei, riavvio engine a metà → tutti ripresi). Nessun `unwrap()` su percorsi di runtime.

---

## M3 — GUI v1 (`apps/gui`, iced) — vedi `docs/UI.md`

- [ ] **M3.1** Shell finestra: sidebar (Stato / Tipologie / Categorie), lista download virtualizzata,
      pannello dettagli richiudibile, barra di stato (velocità totale ↓↑, limite attivo, spazio libero).
- [ ] **M3.2** Dialog "Aggiungi" unico e intelligente: incolla uno o più link / magnet / percorso .torrent;
      rilevamento automatico del tipo; scelta categoria e cartella; drag & drop di file e link.
- [ ] **M3.3** Azioni: pausa/riprendi/annulla (con conferma ed opzione "elimina file"), apri file/cartella,
      copia link, priorità, riordino; selezione multipla; scorciatoie tastiera.
- [ ] **M3.4** Impostazioni (poche pagine chiare): Generale, Download, Connessione/Limiti, Torrent,
      Categorie & regole, Integrazioni (browser, yt-dlp/ffmpeg), Avanzate.
- [ ] **M3.5** Tray icon (`tray-icon`), chiusura in tray, notifiche di sistema (`notify-rust`), avvio con il
      sistema, **istanza singola** (seconda istanza inoltra link/magnet alla prima via IPC locale).
- [ ] **M3.6** Associazioni: `magnet:` e `.torrent` (Windows registry / `.desktop` + `xdg-mime` su Linux).
- [ ] **M3.7** Tema chiaro/scuro (segue il sistema), font Inter incluso, icone Lucide (ISC) come font;
      i18n con `fluent` (italiano + inglese fin da subito).
- [ ] **M3.8** Test GUI con `iced_test` (simulatore headless) sui flussi principali + snapshot delle viste.

### DoD M3
L'utente può aggiungere, monitorare, mettere in pausa, riprendere e annullare download HTTP dalla GUI;
CPU a riposo ~0%; nessun blocco del thread UI (tutte le operazioni lunghe passano dall'engine).

---

## M4 — Torrent (crate `ruscello-torrent`)

- [ ] **M4.1** Integrazione `librqbit` come sessione unica gestita dall'engine (DHT, PEX, trackers,
      UPnP/NAT-PMP, porta configurabile, persistenza fastresume di librqbit nella data dir).
- [ ] **M4.2** Aggiunta da magnet / file / URL di .torrent; finestra pre-aggiunta con albero file,
      selezione file, dimensione, cartella; recupero metadati per magnet con stato "Recupero metadati".
- [ ] **M4.3** Limiti: velocità up/down, ratio e tempo di seeding (poi stop automatico), max connessioni.
- [ ] **M4.4** Dettagli: file con priorità, peer, tracker, pezzi (barra), velocità; "Ricontrolla dati".
- [ ] **M4.5** Mappatura stati librqbit → `Status` comune; i torrent appaiono nella stessa lista degli
      altri download (tipologia "Torrent", ma categorizzabili come gli altri).
- [ ] **M4.6** Opzionale: interfaccia di rete vincolata (bind a VPN), filtro IP (formato eMule/P2P).

### DoD M4
Test con tracker+seeder locali (librqbit può creare e servire torrent) che scaricano un torrent multi-file,
interrompono, riprendono e verificano gli hash. Test magnet senza tracker via DHT locale se fattibile,
altrimenti marcato `#[ignore]` e documentato.

---

## M5 — Resolver, plugin hoster e LinkGrabber (crate `ruscello-resolver`)

**Obiettivo**: dato un qualsiasi URL (pagina web, hoster, link diretto, lista di link), capire cosa
scaricare. Ispirato a JDownloader (plugin host/decrypter, LinkGrabber).

- [ ] **M5.1 Pipeline di risoluzione**: `URL → [Decrypter/crawler] → candidati → [Host plugin] →
      link diretto(i) + header/cookie richiesti + max connessioni consentite + scadenza`.
      Il link diretto viene sempre scaricato dal **nostro motore** (segmentato, riprendibile). Se il link
      scade durante il download, l'engine chiede al plugin un nuovo link e riprende dagli stessi byte.
- [ ] **M5.2 Trait `HostPlugin` / `CrawlerPlugin`** in Rust (plugin compilati, registrati staticamente) con:
      `matches(url)`, `resolve(ctx) -> Resolution`, `requires_browser() -> bool`, limiti (connessioni,
      download paralleli per hoster, attese obbligatorie). Contesto con client HTTP condiviso,
      cookie jar per dominio, accesso all'helper browser (M6) e richiesta di interazione utente.
- [ ] **M5.3 "Ricette" dichiarative** (TOML, ricaricabili, aggiungibili dall'utente senza ricompilare) per
      siti semplici: passi tipo `get`, `submit_form{selector|fields}`, `wait{seconds}`, `extract{css|regex}`,
      `follow`, e in modalità browser `click{selector}`, `wait_for{selector}`. Questo rende gli script
      personali (vedi legacy-scripts) delle ricette di poche righe.
- [ ] **M5.4 Plugin integrati iniziali** (ognuno con fixture HTML registrate e test offline):
  - generico "link diretto" (riconosce risposte non-HTML);
  - **fuckingfast** (estrazione del link `/dl/...` dallo script della pagina via HTTP, senza browser;
    fallback browser se necessario) — caso d'uso prioritario del committente;
  - **XFileSharing generico** (`op=download1`/`method_free` → countdown → `op=download2`/`downloadbtn`),
    pattern usato da molti hoster — caso d'uso prioritario del committente;
  - pixeldrain, gofile, mediafire, Google Drive (file pubblici, conferma file grandi), Dropbox/OneDrive link
    pubblici, GitHub releases, SourceForge, archive.org;
  - crawler di pagine "indice" (es. pagine con elenchi di link a hoster): estrae tutti i link supportati e
    crea un **Package**.
- [ ] **M5.5 LinkGrabber con punteggio** (requisito R7). Analisi della pagina (HTML con `scraper`, script
      inline con regex, JSON-LD/OpenGraph, `<video>/<source>`, iframe di player, URL di hoster noti, URL
      m3u8/mpd, risorse caricate se la pagina è stata aperta nel browser helper). Ogni candidato riceve un
      punteggio spiegabile, es.:
  - + estensione di file "scaricabile" (archivi, video, iso, exe, pdf…), + dimensione grande (HEAD/probe
    in parallelo, limitato), + testo/ARIA del link contiene "download/scarica/get", + nome simile al
    titolo della pagina, + match di un plugin hoster, + `Content-Disposition: attachment`;
  - − domini pubblicitari/tracker (lista tipo EasyList/hosts, aggiornabile), − file minuscoli, immagini di
    UI, CSS/JS, − link a social/login.
  
  UI: lista raggruppata (Probabili ⭐ / Altri file / Media / Pagine collegate), i "probabili" pre-selezionati,
  motivo del punteggio visibile in tooltip ("archivio 4.2 GB · nome simile al titolo"). Filtri per tipo,
  ricerca, selezione multipla, "aggiungi selezionati". Regole di **auto-accettazione** configurabili
  (es. "se un plugin hoster riconosce il link, avvia subito senza chiedere").
- [ ] **M5.6 Import in blocco**: incolla testo libero (estrazione di tutti gli URL), file `.txt`, monitor
      degli appunti (opzionale, off di default), container `.torrent`/`.metalink`/`.m3u`. Deduplicazione.
- [ ] **M5.7 Account hoster** (premium/login) salvati nel keyring di sistema (`keyring` crate), mai in chiaro
      nel DB.

### DoD M5
Test offline con fixture per ogni plugin; test del punteggio su un corpus di ≥ 30 pagine salvate con
risultato atteso (il file giusto nei primi 3 candidati in ≥ 90% dei casi); test end-to-end: lista di 20
link fuckingfast-like serviti dal testkit (pagine finte con lo stesso pattern) → tutti scaricati.
Test "live" (rete reale) separati sotto feature `live-tests`, mai in CI obbligatoria.

---

## M6 — Browser helper e pagine con verifiche (app `apps/webview`, crate `ruscello-browser`)

Vedi ADR-0004. Il browser helper è un **processo separato** (wry + tao, WebView2 su Windows, WebKitGTK su
Linux, WKWebView su macOS) avviato solo quando serve e chiuso dopo inattività (risparmio RAM). Comunicazione
JSON-lines su stdin/stdout definita in `ruscello-browser::protocol`.

- [ ] **M6.1 Protocollo e ciclo di vita** del processo helper (spawn on demand, timeout, crash → restart).
- [ ] **M6.2 Finestra "Verifica richiesta"**: quando un plugin/ricetta rileva una pagina di verifica
      (challenge Cloudflare a pagina intera, captcha dell'hoster, login), l'engine mette il download in stato
      `WaitingUser`, mostra una notifica non invasiva; l'utente apre la finestra, **completa lui la
      verifica**, l'helper rileva il ritorno alla pagina attesa e si chiude da solo. I download dello stesso
      dominio in coda attendono la stessa verifica (una verifica sblocca tutto il batch finché la sessione
      è valida).
- [ ] **M6.3 Riuso della sessione**: cookie (`cookies_for_url`, inclusi HttpOnly) e User-Agent del webview
      vengono passati al client HTTP dell'engine per il dominio. Se il server non accetta la sessione fuori
      dal browser, si usa la modalità M6.4.
- [ ] **M6.4 Risoluzione assistita dal browser**: la ricetta viene eseguita *dentro* il webview (DOM: click,
      wait_for, extract), con **popup e nuove finestre bloccati** a livello nativo
      (`new_window_req_handler`), e il download avviato dalla pagina viene **intercettato**
      (`download_started_handler`): si annulla il download del browser e si passa URL + cookie + referer
      al nostro motore (segmentato, riprendibile). Il webview può restare nascosto quando non serve
      interazione.
- [ ] **M6.5 Cattura risorse**: script iniettato che raccoglie `performance.getEntriesByType('resource')`,
      `<video>` src, richieste fetch/XHR → candidati per il LinkGrabber (utile per player video).
- [ ] **M6.6 Import cookie da browser installati** (best effort: Firefox semplice; Chromium su Windows con
      cifratura app-bound potrebbe non essere fattibile → preferire l'estensione M8). Solo su azione
      esplicita dell'utente.

### DoD M6
Test di integrazione con pagina locale che simula "pagina di verifica → click utente simulato da test →
pagina download": l'engine riprende da solo. Test di intercettazione download e blocco popup su pagina
locale con `window.open` multipli (replica del comportamento degli hoster degli script legacy).

---

## M7 — Media: HLS/DASH nativi + yt-dlp sidecar (crate `ruscello-media`)

- [ ] **M7.1** HLS (`m3u8-rs`): playlist master → scelta qualità, segmenti scaricati in parallelo dal
      nostro motore, AES-128 supportato, resume a livello di segmento, concatenazione `.ts`.
- [ ] **M7.2** DASH (MPD) base: segment template/list, tracce audio+video separate.
- [ ] **M7.3** Gestore sidecar: download di yt-dlp (release ufficiali GitHub, verifica checksum),
      aggiornamento automatico periodico, stesso per ffmpeg (build statiche) — **solo con consenso
      dell'utente** nelle impostazioni; percorso custom supportato.
- [ ] **M7.4** Integrazione yt-dlp: `yt-dlp -J` per ottenere titolo/formati/playlist → finestra di scelta
      formato semplice ("Migliore", "1080p", "Solo audio MP3"…); download preferibilmente con il nostro
      motore usando gli URL dei formati (header inclusi) e fallback a yt-dlp stesso come esecutore con
      parsing del progresso (`--progress-template`, `--newline`). Cookie della sessione passati con
      `--cookies` (file Netscape temporaneo).
- [ ] **M7.5** Merge/remux audio+video con ffmpeg sidecar se presente; altrimenti scelta di formati
      progressivi.
- [ ] **M7.6** Playlist/canali → Package con tutti gli elementi, selezionabili.

### DoD M7
Test HLS/DASH su stream generati localmente (fixture), test parser output yt-dlp con JSON registrati;
test live opzionali.

---

## M8 — API locale ed estensione browser (crate `ruscello-api`, `extensions/browser`)

- [ ] **M8.1** API locale HTTP su `127.0.0.1` (axum) con token generato all'installazione, CORS ristretto
      all'ID dell'estensione; endpoint: aggiungi link (con cookie/referer/UA), stato, versione.
      In alternativa/aggiunta *native messaging host* (più sicuro, niente porta aperta) — scegliere e
      motivare con un ADR.
- [ ] **M8.2** Estensione Manifest V3 (Chrome + Firefox), codice minimo (TS/JS obbligatorio per le
      estensioni): intercettazione download del browser (opzionale, per tipi/dimensioni configurabili),
      menu contestuale "Scarica con Ruscello" su link/selezione/pagina ("analizza pagina" → LinkGrabber),
      invio cookie del dominio per download autenticati.
- [ ] **M8.3** Pairing semplice estensione ↔ app dalla pagina impostazioni.

---

## M9 — Automazione e post-processing

- [ ] **M9.1** Pianificatore: fasce orarie per velocità/limiti, avvio/stop coda a orario.
- [ ] **M9.2** Azioni a fine coda: niente / notifica / sospensione / spegnimento (con countdown annullabile).
- [ ] **M9.3** Prevenzione sospensione del sistema mentre ci sono download attivi (Windows
      `SetThreadExecutionState`, Linux `org.freedesktop.login1` inhibit via `zbus`, macOS IOPMAssertion).
- [ ] **M9.4** Estrazione automatica archivi: zip, 7z (crate Rust), tar.*; RAR tramite 7-Zip/unrar
      esterni se presenti (la licenza UnRAR non è compatibile GPL → niente binding statico; decidere in ADR).
      Archivi multi-parte (`.part1.rar`, `.7z.001`) estratti quando tutte le parti del Package sono
      complete; lista password; eliminazione archivi dopo estrazione (opzionale).
- [ ] **M9.5** Verifica hash automatica se il sito/pacchetto fornisce checksum (file `.sfv`, `.md5`,
      `.sha256` nello stesso Package).
- [ ] **M9.6** Regole "watch folder" per `.torrent` e `.txt` di link.
- [ ] **M9.7** Hook post-download (comando utente) — opzionale.

---

## M10 — Hardening, performance, packaging, release

- [ ] **M10.1** Fuzzing (`cargo-fuzz`) dei parser: Content-Disposition, m3u8/MPD, ricette, control file.
- [ ] **M10.2** Soak test: 24 h con 200 download misti, interruzioni di rete casuali, kill casuali → nessuna
      corruzione, nessuna crescita di memoria.
- [ ] **M10.3** Verifica budget §2 con script ripetibili; profiling (`tracy`/`perf`/`samply`).
- [ ] **M10.4** Packaging: Windows (MSI o NSIS via `cargo-packager`/`cargo-dist`, firma opzionale),
      Linux (AppImage + .deb + Flatpak opzionale), macOS (.dmg). Release automatiche su tag `v*` con GitHub
      Actions.
- [ ] **M10.5** Aggiornamento automatico dell'app (verifica su GitHub Releases, opt-in).
- [ ] **M10.6** Documentazione utente (README, guida rapida, FAQ), screenshot.
- [ ] **M10.7** Repository pubblico: rimozione di eventuali riferimenti personali, controllo licenze dipendenze
      (`cargo-deny`), `SECURITY.md`.

---

## 4. Rischi principali e mitigazioni

| Rischio | Mitigazione |
|---------|-------------|
| Hoster cambiano HTML spesso | Ricette dichiarative aggiornabili senza ricompilare; fixture + test che falliscono in modo chiaro; fallback modalità browser. |
| Sessione da verifica anti-bot non accettata dal client HTTP | Modalità di risoluzione assistita nel webview (M6.4): il file finale viene comunque scaricato dal nostro motore. |
| Due event loop (iced/winit e wry/tao) nello stesso processo | Helper webview in processo separato (ADR-0004). |
| Dimensione/peso di WebView2/WebKitGTK | Helper avviato solo on demand e chiuso dopo inattività. |
| yt-dlp/ffmpeg non sono Rust | Sidecar opzionali, l'app funziona senza; ADR-0002. |
| Licenze (GPL vs UnRAR, ffmpeg) | cargo-deny in CI; binari esterni non linkati. |
| Corruzione file su spegnimento | Ordine fsync dati → control file; test kill -9 obbligatori in CI. |

## 5. Flusso di lavoro con GitHub

- Ogni task `Mx.y` = una issue (o checklist in un'issue epic per i task piccoli). Branch
  `feat/m1-segment-allocator`, `fix/...`; PR verso `main` con CI verde; squash merge.
- Conventional Commits (`feat(http): ...`, `fix(gui): ...`, `test(...)`, `docs(...)`).
- A fine milestone: aggiornare questo file, `CHANGELOG.md`, chiudere la milestone, tag `v0.<M>.0`.
