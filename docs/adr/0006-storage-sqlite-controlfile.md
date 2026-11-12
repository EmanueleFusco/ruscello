# ADR-0006 — Persistenza: SQLite + control file per download

- Stato: Accettato (2026-11-12)

## Contesto
Requisito centrale: nessun download perso se si spegne il PC o cade la connessione.

## Decisione
- **SQLite** (`rusqlite` bundled, WAL, `synchronous=NORMAL`) per coda, metadati, categorie, impostazioni,
  accessi tramite un attore dedicato con scritture batch.
- **Control file** accanto al file parziale (`.rscpart.ctl`) con i ranges effettivamente persistiti, i
  validatori remoti (ETag/Last-Modified/size) e le informazioni per ri-risolvere il link. Formato binario
  versionato con CRC; scrittura atomica (temp + fsync + rename).
- Ordine dei checkpoint: `sync_data()` del file dati **prima** di aggiornare il control file.
- All'avvio: riconciliazione DB ↔ control file (il control file vince sui byte).

## Alternative
- Solo DB: aggiornamenti di progresso ad alta frequenza nel DB sono più costosi e accoppiano lo stato dei
  byte al DB; perdere il DB significherebbe perdere i parziali.
- `redb`/`sled`: validi, ma SQLite è più ispezionabile e collaudato.

## Conseguenze
- I parziali sono ripristinabili anche se il DB viene cancellato (ricostruzione da control file).
