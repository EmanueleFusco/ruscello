# ADR-0004 — Verifiche anti-bot: human-in-the-loop nel browser helper

- Stato: Accettato (2026-11-12)

## Contesto
Alcune pagine (hoster, siti dietro Cloudflare) mostrano verifiche anti-bot, captcha o login. Il committente
vuole che i download vadano a buon fine anche in questi casi, con la massima automazione possibile.

## Decisione
- **Non** implementiamo risoluzione automatica di captcha, click automatici su widget di verifica, servizi
  di solving, spoofing di fingerprint/TLS o browser "stealth/undetected". Motivi: affidabilità (corsa agli
  armamenti che si rompe di continuo), rischio di ban di IP/account, termini di servizio, e posizione del
  progetto come software affidabile e pubblicabile.
- Approccio adottato (lo stesso di JDownloader in modalità manuale e di yt-dlp con i cookie):
  1. il resolver rileva la pagina di verifica e mette il download in stato `WaitingUser` (gli altri download
     continuano);
  2. l'utente apre la finestra del **browser helper** (`ruscello-webview`, wry: WebView2/WebKitGTK/WKWebView)
     e completa lui la verifica;
  3. cookie (inclusi HttpOnly) e User-Agent vengono riutilizzati dal client HTTP per quel dominio; una
     verifica sblocca tutti i download del dominio finché la sessione è valida;
  4. se la sessione non è utilizzabile fuori dal browser, la ricetta viene eseguita dentro il webview e il
     download avviato dalla pagina viene intercettato e passato al nostro motore;
  5. alternative a zero interazione: estensione browser che passa i download con i cookie della sessione
     reale dell'utente (M8), import cookie su richiesta, account premium degli hoster.
- Il browser helper è un **processo separato** avviato on demand (event loop di tao incompatibile con
  quello di iced nello stesso processo; risparmio RAM quando non serve).

## Conseguenze
- Per i batch dietro verifica serve in genere una sola interazione per sessione; tutto il resto è automatico.
- Alcuni siti potrebbero comunque non funzionare fuori dal browser: la modalità 4 copre la maggior parte dei casi.
