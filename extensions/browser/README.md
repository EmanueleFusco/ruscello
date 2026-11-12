# Estensione browser (M8)

Estensione Manifest V3 per Chrome/Chromium e Firefox. Codice volutamente minimo (JS/TS è obbligatorio per le
API delle estensioni); tutta la logica sta nell'app Rust.

Funzioni previste (vedi `docs/PLAN.md` M8):
- menu contestuale "Scarica con Ruscello" (link, selezione, pagina → LinkGrabber);
- intercettazione opzionale dei download del browser (per tipo/dimensione);
- invio di cookie/referer/User-Agent della sessione per download autenticati;
- pairing con l'app (API locale con token o native messaging — da decidere con un ADR).

Da non implementare prima della M8.
