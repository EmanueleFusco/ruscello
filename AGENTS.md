# AGENTS.md — Istruzioni per gli agenti che lavorano su Ruscello

Ruscello è un download manager desktop scritto **interamente in Rust** che unisce un client BitTorrent
(stile qBittorrent) e un download manager HTTP/hoster (stile JDownloader/IDM).

Prima di scrivere codice leggi, in quest'ordine:
1. `docs/PLAN.md` — roadmap, milestone, task con checkbox, Definition of Done (**fonte di verità**).
2. `docs/ARCHITECTURE.md` — crate, responsabilità, flussi, crate esterni consigliati.
3. `docs/adr/` — decisioni già prese (non ribaltarle senza un nuovo ADR).
4. `docs/UI.md` — specifiche dell'interfaccia.
5. `docs/TESTING.md` — strategia di test e gate obbligatori.
6. `docs/reference/legacy-scripts/README.md` — casi d'uso reali del committente da supportare.

Lingua: documentazione di progetto in **italiano**; codice, identificatori, commenti nel codice, messaggi di
commit, titoli di PR in **inglese**. Stringhe UI tramite i18n (`it` + `en`).

---

## 1. Regole non negoziabili

1. **Tutto Rust.** GUI (iced) e backend in Rust. Eccezioni ammesse e già decise:
   - estensione browser (`extensions/browser`): JS/TS minimo richiesto dalle API delle estensioni;
   - **sidecar opzionali** esterni scaricati dall'app con consenso dell'utente: `yt-dlp`, `ffmpeg`,
     eventualmente `7z`/`unrar`. L'app deve funzionare senza di essi.
   Nessun Electron/Tauri-con-frontend-web, nessun Python, nessun Node lato app.
2. **Niente aggiramento automatico di verifiche anti-bot.** Non implementare risolutori di captcha,
   click automatici su Turnstile/reCAPTCHA/hCaptcha, servizi di solving a pagamento, spoofing di
   fingerprint/TLS (JA3) o browser "undetected/stealth". Le verifiche vengono **mostrate all'utente** nel
   browser helper e la sessione risultante viene riutilizzata (ADR-0004). Le attese obbligatorie degli
   hoster (countdown) vanno rispettate.
3. **Mai corrompere o perdere download.** Qualsiasi modifica a `ruscello-http`, writer, control file o
   storage deve mantenere verdi i test di crash/resume (kill del processo) e l'invariante
   *dati fsync → poi control file*.
4. **CI verde obbligatoria** prima del merge: `cargo fmt --check`, `cargo clippy --workspace --all-targets
   -- -D warnings`, `cargo test --workspace` (o `cargo nextest run`) su Windows, Linux, macOS, `cargo deny check`.
5. **Nessun test che dipende dalla rete reale** nella suite di default. I test live stanno dietro la feature
   `live-tests` e/o `#[ignore]`.
6. **Licenza GPL-3.0-or-later.** Dipendenze con licenze compatibili (cargo-deny lo verifica). Codice preso da
   altri progetti va attribuito nel file/commento e deve avere licenza compatibile.
7. **Niente segreti nel repository** (token, cookie, account). Credenziali utente solo nel keyring di sistema.
8. **Nessun `unwrap()`/`expect()` su percorsi di runtime** (ammessi nei test e per invarianti dimostrabili con
   commento `// INVARIANT:`). `unsafe` vietato salvo FFI di sistema documentata con `// SAFETY:` e motivata
   nella PR.

## 2. Comandi

```powershell
cargo check --workspace
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace                 # oppure: cargo nextest run --workspace
cargo deny check                       # licenze, advisories, duplicati
cargo run -p ruscello-gui              # GUI
cargo run -p ruscello-cli -- get <url> # motore senza GUI
./scripts/check.ps1                    # tutti i controlli locali (Windows) — ./scripts/check.sh su Linux/macOS
```

Requisiti Linux per compilare il browser helper (quando verrà aggiunto `wry`):
`libwebkit2gtk-4.1-dev libsoup-3.0-dev libgtk-3-dev libxdo-dev` (+ `libayatana-appindicator3-dev` per la tray).

## 3. Struttura del repository

```
apps/
  gui/        ruscello-gui      binario principale (iced)
  cli/        ruscello-cli      CLI/headless, usata anche nei test di crash
  webview/    ruscello-webview  processo helper browser (wry/tao), avviato on demand
crates/
  core/       ruscello-core     tipi di dominio, state machine, eventi, errori comuni (niente I/O)
  http/       ruscello-http     motore HTTP/FTP segmentato, writer, control file, rate limit
  torrent/    ruscello-torrent  wrapper su librqbit
  media/      ruscello-media    HLS/DASH, gestione sidecar yt-dlp/ffmpeg
  resolver/   ruscello-resolver plugin hoster/crawler, ricette TOML, LinkGrabber e punteggio
  browser/    ruscello-browser  protocollo + client del processo webview
  storage/    ruscello-storage  SQLite, migrazioni, impostazioni
  engine/     ruscello-engine   orchestratore: coda, scheduler, EngineHandle, post-processing
  api/        ruscello-api      API locale per estensione/CLI, IPC istanza singola
  testkit/    ruscello-testkit  server di test e utilità (solo dev-dependency)
extensions/browser/             estensione MV3 Chrome/Firefox (M8)
docs/                           piano, architettura, ADR, UI, test, riferimenti
```

Direzione delle dipendenze (niente cicli): `core` ← `http`/`torrent`/`media`/`resolver`/`browser`/`storage`
← `engine` ← `api` ← `apps/*`. La GUI dipende solo da `engine` (+ `core`), mai direttamente da `http`/`torrent`.

## 4. Modo di lavorare

- **Una milestone alla volta**, nell'ordine di `docs/PLAN.md` (M4/M5/M7 parallelizzabili dopo M3).
- Per ogni task: issue GitHub → branch `feat/mX-breve-descrizione` → commit piccoli (Conventional Commits) →
  PR con descrizione, checklist DoD, come è stato testato → CI verde → squash merge su `main`.
  Usa `gh` (`gh issue list --milestone "M1 ..."`, `gh pr create`, `gh pr merge --squash`).
- Aggiorna le checkbox in `docs/PLAN.md` nella stessa PR che completa il task; aggiorna `CHANGELOG.md`
  (sezione *Unreleased*).
- Decisioni architetturali nuove o cambi di quelle esistenti → nuovo file `docs/adr/NNNN-titolo.md`
  (formato: Contesto, Decisione, Alternative, Conseguenze).
- Se un requisito è ambiguo o una decisione ha impatto per l'utente (UX, licenze, privacy, dipendenze
  pesanti) **chiedi al committente** invece di indovinare; per dettagli implementativi hai libertà.

### Dove hai libertà
Scelta di crate specifici (oltre a quelli fissati negli ADR), struttura interna dei moduli, algoritmi interni
(purché rispettino invarianti e test), dettagli grafici entro `docs/UI.md`, formato interno dei file di
controllo, organizzazione dei test. I crate consigliati in `docs/ARCHITECTURE.md` sono suggerimenti:
verifica sempre la versione più recente su crates.io e lo stato di manutenzione.

### Cosa non fare
- Non introdurre lavoro "di cortesia" non richiesto (refactor massivi, rinomini) mescolato a feature.
- Non disattivare lint o test per far passare la CI; non usare `#[allow(...)]` senza commento.
- Non aggiungere dipendenze pesanti senza motivarlo nella PR (dimensione binario, tempo di build).
- Non fare chiamate di rete nei test di default; non committare file binari grandi (generare le fixture).

## 5. Definition of Done (vale per ogni PR)

- [ ] Compila senza warning su Windows/Linux/macOS (CI).
- [ ] Test nuovi per il comportamento nuovo (unit + integrazione dove ha senso); i test esistenti passano.
- [ ] Nessuna regressione del budget risorse (`docs/PLAN.md` §2) per le parti toccate.
- [ ] Errori gestiti e mostrati all'utente in modo comprensibile (niente panic, niente messaggi grezzi).
- [ ] Log `tracing` utili (livello `debug` per i dettagli, niente dati sensibili: cookie/token mascherati).
- [ ] `docs/PLAN.md` e `CHANGELOG.md` aggiornati; ADR se è stata presa una decisione.

## 6. Convenzioni di codice

- Edition 2024, toolchain stable (`rust-toolchain.toml`). Lint di workspace in `Cargo.toml`.
- Errori: `thiserror` nelle librerie (enum di errori per crate), `anyhow` solo nei binari.
- Async: tokio. Nessun blocco del runtime: I/O su file pesante tramite task dedicati o `spawn_blocking`.
- Concorrenza: preferire attori (task + canali) a `Mutex` condivisi; mai tenere un lock attraverso un `.await`.
- Cancellazione: `tokio_util::sync::CancellationToken` propagato in tutti i task di download.
- Tempo e rete astratti dove serve per testare (es. clock iniettabile per scheduler e rate limiter).
- Nomi file/percorsi: gestire Unicode, percorsi lunghi su Windows (`\\?\`), caratteri vietati.
- La GUI non fa I/O: invia comandi all'`EngineHandle` e reagisce agli eventi.
