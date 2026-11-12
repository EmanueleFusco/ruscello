# Script legacy del committente (casi d'uso da supportare)

Questi due script Python sono ciò che il committente usa oggi. **Non vanno portati così come sono**: vanno
resi possibili dentro Ruscello in modo più robusto. Sono qui solo come riferimento del comportamento atteso.

| File | Cosa fa |
|------|---------|
| `fuckingfast_autodownloader.py` | Legge `links.txt` (una pagina fuckingfast per riga), apre ogni pagina in Chrome (SeleniumBase UC), se c'è un challenge Cloudflare a pagina intera tenta di superarlo, clicca il pulsante `DOWNLOAD` (il primo click apre spesso un popup pubblicitario → chiude i popup e riclicca), lascia scaricare a Chrome, attende che spariscano i `.crdownload`. |
| `xfilesharing_autodownloader.py` | Stesso schema per un hoster in stile **XFileSharing**: click su `#method_free` → attesa → chiusura popup → click su `#downloadbtn` → verifica che sia comparso un `.crdownload`. Pause randomizzate tra i link. |

## Problemi degli script che Ruscello deve risolvere

1. Il download lo fa Chrome → se cade la connessione o si spegne il PC il file è perso, nessuna
   segmentazione, nessun controllo.
2. Popup pubblicitari gestiti a posteriori chiudendo finestre.
3. Fine download rilevata guardando la cartella.
4. Fragilità: selettori fissi, versione di ChromeDriver, attese fisse.

## Come Ruscello copre questi casi

| Esigenza | Soluzione in Ruscello | Milestone |
|----------|-----------------------|-----------|
| Lista di link da file/appunti | Dialog Aggiungi multilinea, import `.txt`, watch folder, deduplicazione; tutti i link di un batch in un **Package** | M3, M5.6, M9.6 |
| fuckingfast | Plugin nativo: legge la pagina via HTTP ed estrae il link diretto `/dl/…` dallo script (da verificare sulla pagina reale e coprire con fixture) → download col motore segmentato e riprendibile. Nessun browser, nessun popup | M5.4 |
| Hoster XFileSharing | Plugin generico XFS (form `op=download1`/`method_free` → countdown rispettato → `op=download2`/`downloadbtn`) via HTTP; ricetta TOML per varianti | M5.3, M5.4 |
| Popup e nuove finestre | In HTTP puro non esistono; in modalità browser vengono bloccati a livello nativo nel webview | M6.4 |
| Pagina di verifica Cloudflare / captcha | Il download passa a "Verifica richiesta"; l'utente completa la verifica nella finestra del browser helper (normalmente **una volta per tutto il batch**, finché la sessione è valida); poi tutto riparte da solo. Niente click automatici sui captcha (vedi ADR-0004) | M6.2, M6.3 |
| Download avviato dalla pagina | Intercettato dal webview e passato al motore (segmentato, riprendibile, verificato) | M6.4 |
| Sapere quando ha finito | Stati espliciti, notifiche, azione a fine coda (spegni/sospendi) | M2, M9.2 |
| Archivi multi-parte (tipico di questi batch) | Estrazione automatica quando tutte le parti del Package sono complete; verifica hash se presenti file `.md5`/`.sfv` | M9.4, M9.5 |

## Test di accettazione (da automatizzare con il testkit)

- 20 pagine "fuckingfast-like" servite localmente → import da `.txt` → tutti i file scaricati con SHA-256
  corretto, anche con kill dell'app a metà e caduta di rete simulata.
- 10 pagine "XFS-like" con countdown e popup `window.open` → nessun popup aperto, countdown rispettati,
  download completati.
- Pagina di verifica finta davanti a 5 link dello stesso dominio → una sola interazione utente (simulata nel
  test) sblocca tutti e 5.
